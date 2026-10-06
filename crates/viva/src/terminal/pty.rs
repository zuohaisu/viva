//! A minimal, owned PTY backend (V14 S3, issue #45).
//!
//! The office previously spawned terminals through `portable-pty`, which
//! does not expose the master file descriptor. Live handoff (ADR 0012
//! decision 2) transfers the master fd itself — SCM_RIGHTS needs the raw
//! fd — so the backend is now a small owned implementation of the same
//! Unix PTY contract portable-pty provided:
//!
//! - `posix_openpt` → `grantpt` → `unlockpt` → `ptsname_r` for the pair;
//! - the child runs via `std::process::Command` with `pre_exec(setsid +
//!   TIOCSCTTY)` on the slave fd, so the child's pgid equals its pid and a
//!   group signal reaches the whole tree (the V05 discipline, unchanged);
//! - the master is an owned raw fd: read/write/resize directly, duplicate
//!   for reader/writer handles, transfer by fd passing.
//!
//! Every unsafe block here is a single libc call with its contract stated.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::terminal::TerminalSpec;

/// Duplicate close-on-exec atomically: a concurrent spawn must never
/// inherit another session's PTY. Keep copies above stdio so Command
/// always dup2s them onto 0/1/2, clearing CLOEXEC on the child's stdio even
/// when the parent's standard descriptors were initially closed.
fn dup_cloexec(fd: RawFd) -> io::Result<RawFd> {
    // SAFETY: F_DUPFD_CLOEXEC duplicates our open fd with a minimum of 3.
    #[cfg(unix)]
    unsafe {
        let dup = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3);
        if dup < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(dup)
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(io::Error::other("PTY requires unix"))
    }
}

/// `libc::ptsname` uses a static buffer and is NOT thread-safe: two
/// concurrent `open`s would race the name and hand a child the WRONG
/// session's slave. Every pty open serializes on this lock for the
/// grantpt/unlockpt/ptsname window.
static PTSNAME_LOCK: Mutex<()> = Mutex::new(());

/// An owned PTY master: one raw fd closed on drop.
pub struct OwnedMaster {
    fd: RawFd,
}

unsafe impl Send for OwnedMaster {}
unsafe impl Sync for OwnedMaster {}

impl OwnedMaster {
    /// Open a new PTY pair sized for the spec. Returns the owned master
    /// and the raw slave fd (the caller owns that fd and passes it to
    /// [`spawn_child`] exactly once).
    pub fn open(spec: &TerminalSpec) -> OfficeResult<(Self, RawFd)> {
        spec.validate()?;
        // SAFETY: posix_openpt returns a fresh master fd or -1; grantpt and
        // unlockpt finalize the slave; ptsname names it (under the global
        // lock - it is not thread-safe). Every error path closes what was
        // already opened.
        #[cfg(unix)]
        unsafe {
            let _name_guard = PTSNAME_LOCK.lock().expect("ptsname lock");
            let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
            let master = libc::posix_openpt(flags);
            if master < 0 {
                return Err(OfficeError::Io(io::Error::last_os_error()));
            }
            if libc::grantpt(master) != 0 {
                let err = io::Error::last_os_error();
                libc::close(master);
                return Err(OfficeError::Io(err));
            }
            if libc::unlockpt(master) != 0 {
                let err = io::Error::last_os_error();
                libc::close(master);
                return Err(OfficeError::Io(err));
            }
            let mut name_buf = [0u8; 256];
            // `ptsname` (not _r) is what the libc crate exposes on macOS;
            // this open path is single-threaded per pair, and the name is
            // copied out immediately.
            let name_ptr = libc::ptsname(master);
            if name_ptr.is_null() {
                let err = io::Error::last_os_error();
                libc::close(master);
                return Err(OfficeError::Io(err));
            }
            let slave_cname = std::ffi::CStr::from_ptr(name_ptr);
            let slave_name_bytes = slave_cname.to_bytes();
            if slave_name_bytes.len() >= name_buf.len() {
                libc::close(master);
                return Err(OfficeError::Validation(
                    "ptsname exceeded the name buffer".into(),
                ));
            }
            name_buf[..slave_name_bytes.len()].copy_from_slice(slave_name_bytes);
            let name_len = slave_name_bytes.len();
            let slave_name = match std::ffi::CString::new(&name_buf[..name_len]) {
                Ok(name) => name,
                Err(e) => {
                    libc::close(master);
                    return Err(OfficeError::Validation(format!("ptsname: {e}")));
                }
            };
            let slave = libc::open(slave_name.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
            if slave < 0 {
                let err = io::Error::last_os_error();
                libc::close(master);
                return Err(OfficeError::Io(err));
            }
            let master = OwnedMaster { fd: master };
            if let Err(err) = master.resize(spec.cols, spec.rows) {
                libc::close(slave);
                return Err(err);
            }
            Ok((master, slave))
        }
        #[cfg(not(unix))]
        {
            let _ = spec;
            Err(OfficeError::Validation("PTY requires unix".into()))
        }
    }

    /// Duplicate the master fd for an independent I/O handle (reader
    /// thread, input writer).
    pub fn dup(&self) -> OfficeResult<RawFd> {
        dup_cloexec(self.fd).map_err(OfficeError::Io)
    }

    /// Resize the pty. The kernel delivers SIGWINCH to the child's
    /// foreground process group — full-screen programs repaint themselves.
    pub fn resize(&self, cols: u16, rows: u16) -> OfficeResult<()> {
        // SAFETY: TIOCSWINSZ on our own fd with a plain winsize struct.
        #[cfg(unix)]
        unsafe {
            let win = libc::winsize {
                ws_col: cols,
                ws_row: rows,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            if libc::ioctl(self.fd, libc::TIOCSWINSZ, &win) != 0 {
                return Err(OfficeError::Io(io::Error::last_os_error()));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = (cols, rows);
            Err(OfficeError::Validation("PTY requires unix".into()))
        }
    }

    /// Take ownership of a raw master fd received by fd passing. The
    /// receiver is now responsible for closing it.
    ///
    /// SAFETY contract for callers: `fd` must be a valid, open master fd
    /// that nothing else will close.
    pub unsafe fn from_received(fd: RawFd) -> Self {
        Self { fd }
    }
}

impl Drop for OwnedMaster {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::close(self.fd);
        }
    }
}

/// Spawn the child on the slave side: own process group (pgid == pid) and
/// the slave as its controlling terminal. Returns the standard `Child`,
/// whose non-blocking `try_wait` keeps the V05 reap discipline intact.
pub fn spawn_child(spec: &TerminalSpec, slave_fd: RawFd) -> OfficeResult<std::process::Child> {
    spec.validate()?;
    // Own every dup immediately, including on partial setup/spawn errors.
    // The tty reference must stay open in the PARENT until spawn returns:
    // pre_exec runs in the child after fork, not when the closure is set.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: each File consumes exactly one fresh CLOEXEC dup.
        let stdin_file = std::fs::File::from_raw_fd(dup_cloexec(slave_fd)?);
        let stdout_file = std::fs::File::from_raw_fd(dup_cloexec(slave_fd)?);
        let stderr_file = std::fs::File::from_raw_fd(dup_cloexec(slave_fd)?);
        let tty_file = std::fs::File::from_raw_fd(dup_cloexec(slave_fd)?);
        let mut command = Command::new(&spec.argv[0]);
        command
            .args(&spec.argv[1..])
            .current_dir(spec.cwd.clone())
            .env("TERM", "xterm-256color");
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        command
            .stdin(Stdio::from(stdin_file))
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        let tty_for_tty = tty_file.as_raw_fd();
        command.pre_exec(move || {
            // Between fork and exec: the child becomes a session leader —
            // setsid alone gives it pgid == pid (the V05 group discipline).
            // NOTE: no Command::process_group here — a prior setpgid would
            // make the child a group leader and setsid would fail EPERM.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(
                tty_for_tty,
                libc::TIOCSCTTY as libc::c_ulong,
                0 as libc::c_int,
            ) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
        let child = command
            .spawn()
            .map_err(|e| OfficeError::Validation(format!("failed to spawn terminal: {e}")));
        // In the child CLOEXEC closes the reference at exec; in the parent
        // the guard is dropped only AFTER pre_exec has finished.
        drop(tty_file);
        child
    }
    #[cfg(not(unix))]
    Err(OfficeError::Validation("PTY requires unix".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_rejects_a_non_tty_instead_of_ignoring_tiocsctty_failure() {
        let not_a_tty = std::fs::File::open("/dev/null").expect("open");
        let spec = TerminalSpec::new(vec!["/bin/true".into()], std::env::temp_dir()).expect("spec");
        match spawn_child(&spec, not_a_tty.as_raw_fd()) {
            Err(err) => assert!(err.to_string().contains("failed to spawn terminal")),
            Ok(mut child) => {
                let _ = child.wait();
                panic!("spawn must report failure to acquire the controlling terminal");
            }
        }
    }
}
