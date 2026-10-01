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
use std::os::fd::{FromRawFd, RawFd};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::terminal::TerminalSpec;

/// Duplicate an fd close-on-exec. The copies live inside one process as
/// reader/writer handles or become the child's stdio.
fn dup_cloexec(fd: RawFd) -> io::Result<RawFd> {
    // SAFETY: dup on our own open fd; the flag update only touches the dup.
    #[cfg(unix)]
    unsafe {
        let dup = libc::dup(fd);
        if dup < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = libc::fcntl(dup, libc::F_GETFD);
        if flags >= 0 {
            let _ = libc::fcntl(dup, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
        Ok(dup)
    }
    #[cfg(not(unix))]
    {
        let _ = fd;
        Err(io::Error::other("PTY requires unix"))
    }
}

/// Duplicate an fd WITHOUT close-on-exec, for the child's stdio: when the
/// duplicated fd happens to land on 0/1/2, std skips the dup2 (already the
/// target) and a CLOEXEC flag would close the child's stdio at exec — the
/// child would see an instantly-EOF stdin and die. The parent's copies are
/// closed right after spawn, so the flag is only needed there.
fn dup_no_cloexec(fd: RawFd) -> io::Result<RawFd> {
    // SAFETY: dup on our own open fd.
    #[cfg(unix)]
    unsafe {
        let dup = libc::dup(fd);
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
    // Dedicated dups for each stdio stream plus one the pre_exec closure
    // uses for TIOCSCTTY. All CLOEXEC: after exec only the child's real
    // stdio remains.
    #[cfg(unix)]
    let (stdin_fd, stdout_fd, stderr_fd, tty_fd) = {
        // The three stdio dups must NOT carry CLOEXEC (see dup_no_cloexec);
        // the TTY reference fd does (it is used in pre_exec, then gone).
        let s = dup_no_cloexec(slave_fd).map_err(OfficeError::Io)?;
        match (|| -> io::Result<(RawFd, RawFd, RawFd)> {
            Ok((
                dup_no_cloexec(slave_fd)?,
                dup_no_cloexec(slave_fd)?,
                dup_cloexec(slave_fd)?,
            ))
        })() {
            Ok(tuple) => (s, tuple.0, tuple.1, tuple.2),
            Err(err) => {
                unsafe { libc::close(s) };
                return Err(OfficeError::Io(err));
            }
        }
    };
    #[cfg(not(unix))]
    let (stdin_fd, stdout_fd, stderr_fd, tty_fd) = (-1, -1, -1, -1);

    // SAFETY: from_raw_fd of fresh dups above; each Stdio consumes exactly
    // one and closes it in the parent after spawn.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt as _;
        let stdin_file = std::fs::File::from_raw_fd(stdin_fd);
        let stdout_file = std::fs::File::from_raw_fd(stdout_fd);
        let stderr_file = std::fs::File::from_raw_fd(stderr_fd);
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
        let tty_for_tty = tty_fd;
        command.pre_exec(move || {
            // Between fork and exec: the child becomes a session leader —
            // setsid alone gives it pgid == pid (the V05 group discipline).
            // NOTE: no Command::process_group here — a prior setpgid would
            // make the child a group leader and setsid would fail EPERM.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            let _ = libc::ioctl(
                tty_for_tty,
                libc::TIOCSCTTY as libc::c_ulong,
                0 as libc::c_int,
            );
            Ok(())
        });
        // The pre_exec reference fd is CLOEXEC: it closes at exec inside
        // the child; close the parent's reference here.
        let _ = libc::close(tty_fd);
        command
            .spawn()
            .map_err(|e| OfficeError::Validation(format!("failed to spawn terminal: {e}")))
    }
    #[cfg(not(unix))]
    Err(OfficeError::Validation("PTY requires unix".into()))
}
