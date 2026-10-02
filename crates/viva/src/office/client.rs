//! The office client: the attach side of the resident-server split (V14
//! S1, issue #43; ADR 0012).
//!
//! Before this module the TUI process WAS the office host. Now the host is
//! a resident `viva server` process that owns the store, the terminals and
//! the socket, and the workbench / headless CLI are clients that connect to
//! it, render projections, and may detach at any time — the terminals keep
//! running on the server.
//!
//! Bootstrapping: when no healthy host answers, `ensure_server` spawns one
//! detached (`viva server`, own process group, output appended to
//! `server.log` inside the 0700 home) and waits for the socket. This is the
//! ADR 0012 sanctioned resident runtime — its lifecycle is explicit and
//! recorded, not a hidden side effect of some other command.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::office::OFFICE_SOCKET_NAME;
use crate::office::protocol::{
    OfficeRequest, OfficeRequestKind, OfficeResponse, new_request, round_trip, round_trip_within,
};

/// How long `ensure_server` waits for a freshly spawned server to bind its
/// socket before declaring the start failed.
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for the socket.
const START_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One connected client session. Requests are synchronous round trips; the
/// server handles each connection on its own thread.
pub struct OfficeClient {
    stream: UnixStream,
}

impl OfficeClient {
    /// Connect to the healthy host at `home`. Fails honestly when the
    /// socket is absent or wedged — use [`ensure_server`] for the
    /// spawn-and-wait behavior.
    pub fn connect(home: &Path) -> OfficeResult<Self> {
        let socket_path = home.join(OFFICE_SOCKET_NAME);
        if !socket_path.exists() {
            return Err(OfficeError::Validation(format!(
                "no active office server in {} (start one with `viva server`)",
                home.display()
            )));
        }
        let stream = UnixStream::connect(&socket_path)?;
        let mut client = Self { stream };
        // Prove the peer is a healthy host before handing out the session.
        let response = client.call(OfficeRequestKind::Ping)?;
        if response.get("pid").is_none() {
            return Err(OfficeError::Validation(
                "socket answered but the host handshake failed; refusing to attach".into(),
            ));
        }
        Ok(client)
    }

    /// Connect, spawning a detached resident server first when none is
    /// running. The spawned server outlives this process by construction:
    /// its own process group, its output in `server.log`.
    pub fn ensure_server(home: &Path) -> OfficeResult<Self> {
        let socket_path = home.join(OFFICE_SOCKET_NAME);
        if socket_path.exists() {
            match OfficeClient::connect(home) {
                Ok(client) => return Ok(client),
                Err(err) => {
                    return Err(OfficeError::Validation(format!(
                        "socket {} exists but no healthy host answered ({}); \
                         inspect the wedged server first — it will not be killed blindly",
                        socket_path.display(),
                        err
                    )));
                }
            }
        }
        spawn_detached_server(home, &[])?;
        let deadline = Instant::now() + SERVER_START_TIMEOUT;
        loop {
            if socket_path.exists() {
                if let Ok(client) = OfficeClient::connect(home) {
                    return Ok(client);
                }
            }
            if Instant::now() >= deadline {
                return Err(OfficeError::Validation(format!(
                    "the spawned server did not bind {} within {SERVER_START_TIMEOUT:?}; \
                     check {}/server.log",
                    socket_path.display(),
                    home.display()
                )));
            }
            std::thread::sleep(START_POLL_INTERVAL);
        }
    }

    /// One request → result value, or the server's error text as a
    /// validation error.
    pub fn call(&mut self, kind: OfficeRequestKind) -> OfficeResult<serde_json::Value> {
        self.call_request(new_request(kind))
    }

    /// [`Self::call`] with a pre-built request (grant fields set).
    pub fn call_request(&mut self, request: OfficeRequest) -> OfficeResult<serde_json::Value> {
        let OfficeResponse {
            ok, result, error, ..
        } = round_trip(&mut self.stream, &request)?;
        if ok {
            Ok(result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(OfficeError::Validation(
                error.unwrap_or_else(|| "rejected without a reason".into()),
            ))
        }
    }

    /// Raw access for callers that need the response envelope (rare).
    pub fn round_trip_raw(&mut self, request: &OfficeRequest) -> OfficeResult<OfficeResponse> {
        round_trip(&mut self.stream, request)
    }

    /// One request with an explicit read budget. The budget is carried
    /// through the whole round trip — `read_message` used to reset it to
    /// IO_TIMEOUT mid-flight, voiding this API entirely (QA F14: a 16s
    /// sync errored at the client at 10.03s while the server succeeded).
    pub fn call_with_timeout(
        &mut self,
        kind: OfficeRequestKind,
        read_timeout: std::time::Duration,
    ) -> OfficeResult<serde_json::Value> {
        self.call_request_with_timeout(new_request(kind), read_timeout)
    }

    /// [`Self::call_with_timeout`] for a pre-built request (grant fields
    /// intact).
    pub fn call_request_with_timeout(
        &mut self,
        request: OfficeRequest,
        read_timeout: std::time::Duration,
    ) -> OfficeResult<serde_json::Value> {
        let OfficeResponse {
            ok, result, error, ..
        } = round_trip_within(&mut self.stream, &request, read_timeout)?;
        if ok {
            Ok(result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(OfficeError::Validation(
                error.unwrap_or_else(|| "rejected without a reason".into()),
            ))
        }
    }
}

/// Spawn `viva server` detached: its own process group (so this client's
/// death signals nothing), stdin dropped, stdout+stderr appended to
/// `server.log` in the 0700 home. Returns once the spawn is handed to the
/// OS — readiness is the caller's polling job.
fn spawn_detached_server(home: &Path, extra_args: &[&str]) -> OfficeResult<()> {
    let exe = std::env::current_exe()?;
    let log_path = home.join("server.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let err_log = log.try_clone()?;
    let mut command = Command::new(exe);
    command
        .arg("server")
        .args(extra_args)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err_log)
        .env("VIVA_HOME", home);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Own process group: the server is a resident runtime, not a child
        // that dies with the attaching client (ADR 0012 decision 1).
        command.process_group(0);
    }
    command.spawn().map_err(|e| {
        OfficeError::Validation(format!("failed to spawn the resident server: {e}"))
    })?;
    Ok(())
}

/// A live-handoff restart (S3, issue #45): ask the running server to begin
/// the handoff, spawn the resumed server detached, and wait until a new
/// host answers on the control socket. Returns the new host's pid.
pub fn restart_server(home: &Path) -> OfficeResult<u32> {
    crate::foundation::paths::ensure_private_dir(home)?;
    // The OLD server must be running; a restart never spawns anything.
    // Record its identity: during the handoff window the old host keeps
    // answering, and the resumed host REBINDS the same socket path within
    // milliseconds of the old one leaving - so "socket gone" is not a
    // usable phase boundary. The identity change is.
    let mut client = OfficeClient::connect(home)?;
    let before = client.call(OfficeRequestKind::Ping)?;
    let old_pid = before.get("pid").and_then(|p| p.as_u64());
    let response = client.call(OfficeRequestKind::ServerRestart)?;
    let handoff_socket = response
        .get("handoff_socket")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            OfficeError::Validation("restart response carried no handoff socket".into())
        })?
        .to_string();
    let _ = handoff_socket; // the resumed server finds it under the home
    drop(client);

    spawn_detached_server(home, &["--resume"])?;

    // Wait until whoever answers is NOT the old host (QA F1 round 2): the
    // old host answers pings during the transfer window; the moment the
    // resumed host rebinds, the pid changes. A failed handoff leaves the
    // old host answering forever and times out honestly here.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(mut client) = OfficeClient::connect(home) {
            if let Ok(status) = client.call(OfficeRequestKind::Ping) {
                if let Some(pid) = status.get("pid").and_then(|p| p.as_u64()) {
                    if Some(pid) != old_pid {
                        return Ok(pid as u32);
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(OfficeError::Validation(
                "45s after a restart request the old host is still the one serving - the \
                 handoff failed (see the recovery records and server.log)"
                    .into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
