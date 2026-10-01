//! The live-handoff wire protocol (V14 S3, issue #45; ADR 0012 decision 2).
//!
//! A server upgrade transfers every live PTY master fd to the next server
//! process: the old server binds `handoff.sock` and answers a
//! `ServerRestart` request with its path; the resumed server connects,
//! receives one manifest + one fd per terminal (SCM_RIGHTS), and acks.
//!
//! Non-destructive by construction: SCM_RIGHTS duplicates the open file
//! description — the old server keeps its own reference until it exits
//! after a full ack. Every failure path (new server never starts, partial
//! transfer, ack timeout) therefore leaves the old server serving and the
//! children untouched: exactly the type-1 fallback semantics, with zero
//! interruption.

use std::io::{BufRead as _, Read as _, Write as _};
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::foundation::error::{OfficeError, OfficeResult};

/// Handoff wire version.
pub const HANDOFF_PROTOCOL: u32 = 1;
/// Per-message timeout during the transfer.
const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// One terminal's transfer manifest: identity (kept across the restart),
/// process facts, size and the old session's scrollback as history text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffEntry {
    pub terminal_id: String,
    /// `user_shell` | `agent_cli` | `test_run` | `member_execution:<id>`.
    pub owner: String,
    pub worktree_id: Option<String>,
    pub purpose: String,
    pub pid: u32,
    pub pid_start_marker: String,
    pub cols: u16,
    pub rows: u16,
    /// The old session's scrollback lines (plain text), rendered above the
    /// live grid by the adopted session.
    pub history: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffManifest {
    pub protocol: u32,
    pub host_id: String,
    pub entries: Vec<HandoffEntry>,
}

/// Send the manifest and one fd per entry (in order), then wait for the
/// receiver's ack. The caller keeps its own fd references until the ack —
/// a failed transfer changes nothing on the sending side.
pub fn send_entries(
    stream: &mut UnixStream,
    manifest: &HandoffManifest,
    fds: &[RawFd],
) -> OfficeResult<()> {
    if manifest.entries.len() != fds.len() {
        return Err(OfficeError::Validation(format!(
            "handoff manifest has {} entries but {} fds",
            manifest.entries.len(),
            fds.len()
        )));
    }
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    let mut text = serde_json::to_string(manifest)?;
    text.push('\n');
    stream.write_all(text.as_bytes())?;
    stream.flush()?;
    for fd in fds {
        send_one_fd(stream, *fd)?;
    }
    // The receiver acks and closes; a half-close after the ack line is
    // normal, so read errors past the ack are not transfer failures.
    let mut ack = String::new();
    let mut reader = stream.try_clone()?;
    let _ = reader.read_to_string(&mut ack);
    if !ack.contains("\"ok\":true") && !ack.contains("\"ok\": true") {
        return Err(OfficeError::Validation(format!(
            "handoff receiver did not ack: {}",
            ack.trim()
        )));
    }
    Ok(())
}

/// Read the manifest and the per-entry fds from the CONNECTED handoff
/// stream, then ack. The receiver owns the received fds from that point on.
pub fn receive_entries(stream: &UnixStream) -> OfficeResult<(HandoffManifest, Vec<RawFd>)> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut stream = stream.try_clone()?;
    let mut line = String::new();
    {
        let mut reader = std::io::BufReader::new(stream.try_clone()?);
        reader.read_line(&mut line)?;
    }
    if line.trim().len() > crate::office::MAX_MESSAGE_BYTES {
        return Err(OfficeError::Validation("handoff manifest over the size bound".into()));
    }
    let manifest: HandoffManifest = serde_json::from_str(line.trim())
        .map_err(|e| OfficeError::Validation(format!("bad handoff manifest: {e}")))?;
    if manifest.protocol != HANDOFF_PROTOCOL {
        return Err(OfficeError::Validation(format!(
            "handoff protocol {} (expected {HANDOFF_PROTOCOL})",
            manifest.protocol
        )));
    }
    let mut fds = Vec::new();
    for _ in &manifest.entries {
        fds.push(receive_one_fd(&stream)?);
    }
    stream.write_all(b"{\"ok\":true}\n")?;
    stream.flush()?;
    Ok((manifest, fds))
}

/// Send one fd over the stream via SCM_RIGHTS.
fn send_one_fd(stream: &UnixStream, fd: RawFd) -> OfficeResult<()> {
    #[cfg(unix)]
    unsafe {
        use std::os::fd::AsRawFd as _;
        let mut dummy: [u8; 1] = [0];
        let mut iov = libc::iovec {
            iov_base: dummy.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
        let mut cmsg_buf = vec![0u8; space];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        let cmsg = cmsg_buf.as_mut_ptr() as *mut libc::cmsghdr;
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
        *data = fd;
        let sent = libc::sendmsg(stream.as_raw_fd(), &msg, 0);
        if sent < 0 {
            return Err(OfficeError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (stream, fd);
        Err(OfficeError::Validation("fd passing requires unix".into()))
    }
}

/// Receive one fd sent via SCM_RIGHTS. The received fd is owned by the
/// caller (and carries no close-on-exec flag).
fn receive_one_fd(stream: &UnixStream) -> OfficeResult<RawFd> {
    #[cfg(unix)]
    unsafe {
        use std::os::fd::AsRawFd as _;
        let mut dummy = [0u8; 1];
        let mut iov = libc::iovec {
            iov_base: dummy.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
        let mut cmsg_buf = vec![0u8; space];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        let n = libc::recvmsg(stream.as_raw_fd(), &mut msg, 0);
        if n < 0 {
            return Err(OfficeError::Io(std::io::Error::last_os_error()));
        }
        if msg.msg_controllen == 0 {
            return Err(OfficeError::Validation(
                "handoff fd message carried no control data".into(),
            ));
        }
        let cmsg = cmsg_buf.as_mut_ptr() as *mut libc::cmsghdr;
        if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Err(OfficeError::Validation(
                "handoff control message was not SCM_RIGHTS".into(),
            ));
        }
        let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
        Ok(*data)
    }
    #[cfg(not(unix))]
    {
        let _ = stream;
        Err(OfficeError::Validation("fd passing requires unix".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    /// Round trip: a written byte travels through a received duplicate of
    /// the same file description.
    #[test]
    fn fd_passing_transfers_an_open_description() {
        let (mut a, mut b) = UnixStream::pair().expect("pair");
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("probe");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .expect("open");
        let fd = file.as_raw_fd();
        send_one_fd(&mut a, fd).expect("send");
        let received = receive_one_fd(&mut b).expect("recv");
        // The received fd is a fresh reference: own it and use it.
        let mut file = unsafe { std::fs::File::from_raw_fd(received) };
        file.write_all(b"probe-write").expect("write via received fd");
        drop(file);
        let content = std::fs::read(&path).expect("read");
        assert_eq!(content, b"probe-write");
    }
}
