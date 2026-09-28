//! The office control-channel wire protocol (V07, issue #16).
//!
//! A private, local-only JSON-lines protocol over the per-`VIVA_HOME` Unix
//! domain socket. It carries the same discipline as the foundation control
//! envelope: a protocol version, a request id, a bounded payload, and
//! machine-readable rejections. What it adds is the office-level request
//! vocabulary: status queries (read-only, also answerable offline from the
//! store), dispatch of a task execution under a named grant, terminal
//! observation and control, task results, and the host shutdown request.
//!
//! This is deliberately NOT a general RPC surface and not a network server:
//! the socket lives inside the 0700 `VIVA_HOME` and only the office host
//! listens on it. No daemon is ever started by a client that finds the
//! socket absent — mutation without an active office is a clean rejection.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::foundation::error::{OfficeError, OfficeResult};

/// Current wire-protocol version. A bump is a coordinated contract change.
pub const PROTOCOL_VERSION: u32 = 1;
/// Hard bound on one framed message (request or response), in bytes.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// Per-read timeout on the socket; a silent peer cannot hold a thread.
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// One request over the control channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfficeRequest {
    pub request_id: String,
    pub version: u32,
    pub kind: OfficeRequestKind,
}

/// The office-level request vocabulary. Read-only kinds are answerable from
/// the store alone; every other kind needs the live host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OfficeRequestKind {
    /// Liveness probe; no side effects.
    Ping,
    /// Office snapshot: host identity, terminal list, execution counts.
    Status,
    /// Dispatch one task execution under a grant. Idempotent by
    /// `request_key`: a retry never spawns a second execution.
    Dispatch {
        task_id: String,
        member_id: String,
        grant_id: String,
        request_key: String,
        argv: Vec<String>,
        cwd: String,
        worktree_id: Option<String>,
    },
    /// All registered terminals with owner/purpose/work location.
    TerminalList,
    /// Snapshot one terminal's visible grid + scrollback.
    TerminalSnapshot { terminal_id: String },
    /// Write bytes to one terminal's stdin.
    TerminalInput {
        terminal_id: String,
        bytes_hex: String,
    },
    /// Stop one terminal (its process group only — never the neighbors).
    TerminalStop { terminal_id: String },
    /// Results recorded for a task (process facts, QA conclusions, PR/CI).
    TaskResults { task_id: String },
    /// Ask the host to shut down gracefully: stop new dispatch, stop owned
    /// terminals, persist the handoff, release the channel.
    Shutdown,
}

/// One response, correlated by request id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OfficeResponse {
    pub request_id: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl OfficeResponse {
    pub fn ok(request_id: impl Into<String>, result: Value) -> Self {
        Self {
            request_id: request_id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

/// Read one framed JSON message. The frame is a single line; a peer that
/// sends an over-long or non-JSON frame is a protocol violation, not a panic.
pub fn read_message(stream: &mut UnixStream) -> OfficeResult<String> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(OfficeError::Io(std::io::Error::other(
            "control channel closed before a message arrived",
        )));
    }
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(OfficeError::Validation(format!(
            "control message is {} bytes; the bound is {MAX_MESSAGE_BYTES}",
            line.len()
        )));
    }
    Ok(line.trim_end().to_string())
}

/// Write one framed JSON message.
pub fn write_message(stream: &mut UnixStream, value: &impl Serialize) -> OfficeResult<()> {
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut text = serde_json::to_string(value)?;
    text.push('\n');
    let bytes = text.into_bytes();
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(OfficeError::Validation(format!(
            "control message is {} bytes; the bound is {MAX_MESSAGE_BYTES}",
            bytes.len()
        )));
    }
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

/// Send one request and wait for its correlated response.
pub fn round_trip(
    stream: &mut UnixStream,
    request: &OfficeRequest,
) -> OfficeResult<OfficeResponse> {
    write_message(stream, request)?;
    let line = read_message(stream)?;
    let response: OfficeResponse = serde_json::from_str(&line).map_err(|e| {
        OfficeError::Validation(format!("control channel sent a malformed response: {e}"))
    })?;
    if response.request_id != request.request_id {
        return Err(OfficeError::Validation(format!(
            "response id `{}` does not match request `{}`",
            response.request_id, request.request_id
        )));
    }
    Ok(response)
}

/// Build a request with a fresh id and the current protocol version.
pub fn new_request(kind: OfficeRequestKind) -> OfficeRequest {
    OfficeRequest {
        request_id: format!("req-{}", uuid::Uuid::new_v4().simple()),
        version: PROTOCOL_VERSION,
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pair() -> (UnixStream, UnixStream) {
        let (a, b) = std::os::unix::net::UnixStream::pair().expect("pair");
        (a, b)
    }

    #[test]
    fn request_and_response_round_trip_over_a_real_socket() {
        let (mut client, mut server) = pair();
        let request = new_request(OfficeRequestKind::Ping);
        let sent = request.clone();
        let writer = std::thread::spawn(move || {
            write_message(&mut server, &sent).expect("write");
        });
        let line = read_message(&mut client).expect("read");
        let received: OfficeRequest = serde_json::from_str(&line).expect("parse");
        writer.join().expect("writer");
        assert_eq!(received, request);
        assert_eq!(received.version, PROTOCOL_VERSION);
    }

    #[test]
    fn response_correlation_is_enforced() {
        let (mut client, mut server) = pair();
        let request = new_request(OfficeRequestKind::Ping);
        write_message(&mut server, &OfficeResponse::err("other-id", "mismatch"))
            .expect("write response");
        let err = round_trip(&mut client, &request).expect_err("correlation must fail");
        assert!(err.to_string().contains("does not match"), "got: {err}");
    }

    #[test]
    fn oversize_frames_are_rejected_not_truncated() {
        let (mut client, mut server) = pair();
        let blob = "x".repeat(MAX_MESSAGE_BYTES + 1);
        // The writer runs on its own thread: the peer buffer fills long
        // before the reader sees the newline, so a same-thread write would
        // deadlock against itself.
        let writer = std::thread::spawn(move || {
            serde_json::to_writer(
                &mut server,
                &json!({"request_id": "r", "version": 1, "kind": {"type": "ping"}}),
            )
            .ok();
            let _ = server.write_all(format!("  {blob}\n").as_bytes());
        });
        let err = read_message(&mut client).expect_err("oversize must fail");
        assert!(err.to_string().contains("bound"), "got: {err}");
        writer.join().expect("writer");
    }
}
