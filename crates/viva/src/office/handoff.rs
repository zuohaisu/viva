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

use std::io::Write as _;
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
    #[serde(default)]
    pub screen_hex: String,
    /// Trailing unfinished escape/UTF-8 bytes the old reader consumed but
    /// its parser had not yet applied — replayed into the adopted parser
    /// before its own reader starts, so a sequence split by the transfer
    /// is reassembled. Empty for old senders (serde default) and for tails
    /// that ended on a sequence boundary.
    #[serde(default)]
    pub pending_hex: String,
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
    if text.len() > 64 * 1024 * 1024 {
        return Err(OfficeError::Validation(
            "handoff manifest exceeds 64 MiB; old server stays resident".into(),
        ));
    }
    stream.write_all(text.as_bytes())?;
    stream.flush()?;
    for fd in fds {
        send_one_fd(stream, *fd)?;
    }
    // The ack is one line; read it newline-bounded (the receiver may keep
    // the socket open). Read errors past a complete line are not failures.
    let mut ack = String::new();
    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let _ = std::io::BufRead::read_line(&mut reader, &mut ack);
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
///
/// Implementation note (the Linux CI hang, issue #45 QA round 1): the
/// manifest and the fd messages MAY coalesce into one socket segment, and
/// a buffered reader that receives WITHOUT a control buffer makes the
/// kernel DISCARD the passed fds. So this side receives with raw recvmsg
/// and a control buffer on EVERY call, collecting fds per segment and
/// parsing the manifest from the byte stream - coalescing then cannot
/// lose anything.
pub fn receive_entries(stream: &UnixStream) -> OfficeResult<(HandoffManifest, Vec<RawFd>)> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    // SAFETY: every recvmsg below targets our own connected fd with valid
    // iov + control buffers; cmsg walking uses the kernel-provided lengths.
    #[cfg(unix)]
    unsafe {
        use std::os::fd::AsRawFd as _;
        let fd = stream.as_raw_fd();
        let mut data: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        let ctl_space = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize * 8;
        let mut ctl = vec![0u8; ctl_space];
        let mut received_fds: Vec<RawFd> = Vec::new();
        let mut manifest: Option<HandoffManifest> = None;
        let outcome = loop {
            let mut iov = libc::iovec {
                iov_base: chunk.as_mut_ptr().cast(),
                iov_len: chunk.len(),
            };
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = ctl.as_mut_ptr().cast();
            msg.msg_controllen = ctl.len() as _;
            let n = libc::recvmsg(fd, &mut msg, 0);
            if msg.msg_controllen as usize >= libc::CMSG_LEN(0) as usize {
                let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
                while !cmsg.is_null() {
                    if (*cmsg).cmsg_level == libc::SOL_SOCKET
                        && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                    {
                        let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                            / std::mem::size_of::<RawFd>();
                        let base = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                        for i in 0..count {
                            received_fds.push(*base.add(i));
                        }
                    }
                    cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
                }
            }
            if n < 0 {
                break Err(OfficeError::Io(std::io::Error::last_os_error()));
            }
            let n = n as usize;
            data.extend_from_slice(&chunk[..n]);
            if data.len() > 64 * 1024 * 1024 {
                break Err(OfficeError::Validation(
                    "handoff manifest exceeds 64 MiB".into(),
                ));
            }
            if manifest.is_none() {
                if let Some(pos) = data.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = data.drain(..=pos).collect();
                    match serde_json::from_slice::<HandoffManifest>(&line[..line.len() - 1]) {
                        Ok(parsed) => {
                            if parsed.protocol != HANDOFF_PROTOCOL {
                                break Err(OfficeError::Validation(format!(
                                    "handoff protocol {} (expected {HANDOFF_PROTOCOL})",
                                    parsed.protocol
                                )));
                            }
                            manifest = Some(parsed);
                        }
                        Err(e) => {
                            break Err(OfficeError::Validation(format!(
                                "bad handoff manifest: {e}"
                            )));
                        }
                    }
                }
            }
            if let Some(parsed) = &manifest {
                if received_fds.len() >= parsed.entries.len() {
                    break Ok((parsed.clone(), std::mem::take(&mut received_fds)));
                }
            }
            if n == 0 {
                break Err(OfficeError::Validation(
                    "handoff stream closed before all fds arrived".into(),
                ));
            }
        };
        // On any failure the already-received fds are ours to close.
        if outcome.is_err() {
            for fd in received_fds {
                libc::close(fd);
            }
            return outcome;
        }
        // Ack ONLY after every fd is in: the sender keeps its own fd
        // references until it sees this line.
        let mut writer = stream.try_clone()?;
        let _ = writer.write_all(b"{\"ok\":true}\n");
        let _ = writer.flush();
        outcome
    }
    #[cfg(not(unix))]
    {
        let _ = stream;
        Err(OfficeError::Validation("fd passing requires unix".into()))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd as _;

    /// Round trip through the REAL protocol: send_entries delivers a
    /// manifest + one fd; receive_entries recovers both — including the
    /// Linux coalescing case (manifest and fd message in one segment),
    /// which is exactly what the CI hang was made of.
    #[test]
    fn entries_round_trip_through_the_real_protocol() {
        let (client_side, server_side) = UnixStream::pair().expect("pair");
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("probe");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("open");
        let manifest = HandoffManifest {
            protocol: HANDOFF_PROTOCOL,
            host_id: "host-test".into(),
            entries: vec![HandoffEntry {
                terminal_id: "term-test".into(),
                owner: "user_shell".into(),
                worktree_id: None,
                purpose: "probe".into(),
                pid: 1,
                pid_start_marker: "pid=1".into(),
                cols: 80,
                rows: 24,
                history: vec!["old line".into()],
                screen_hex: String::new(),
                // `\x1b[3` — a handoff that stopped mid-SGR (hex: 1b 5b 33).
                pending_hex: "1b5b33".into(),
            }],
        };
        let fds = [file.as_raw_fd()];
        let sender = std::thread::spawn({
            let mut client_side = client_side;
            let manifest = manifest.clone();
            move || send_entries(&mut client_side, &manifest, &fds).expect("send")
        });
        let (received_manifest, received_fds) = {
            let outcome = receive_entries(&server_side).expect("receive");
            drop(server_side); // done receiving; the sender may see EOF past the ack
            outcome
        };
        sender.join().expect("sender");
        assert_eq!(received_manifest, manifest);
        assert_eq!(received_fds.len(), 1);
        // The received fd is a live reference to the same file description.
        let mut file: std::fs::File =
            unsafe { std::os::fd::FromRawFd::from_raw_fd(received_fds[0]) };
        file.write_all(b"probe-write")
            .expect("write via received fd");
        drop(file);
        assert_eq!(std::fs::read(&path).expect("read"), b"probe-write");
    }

    /// A manifest from an OLDER sender (no `pending_hex`) still parses —
    /// the field defaults to empty, so an upgraded resumed server adopts
    /// sessions from a pre-update host.
    #[test]
    fn manifest_without_pending_hex_parses() {
        let text = r#"{"protocol":1,"host_id":"h","entries":[{"terminal_id":"t","owner":"user_shell","worktree_id":null,"purpose":"p","pid":1,"pid_start_marker":"pid=1","cols":80,"rows":24,"history":[],"screen_hex":""}]}"#;
        let manifest: HandoffManifest = serde_json::from_str(text).expect("old manifest");
        assert_eq!(manifest.entries[0].pending_hex, "");
    }
}
