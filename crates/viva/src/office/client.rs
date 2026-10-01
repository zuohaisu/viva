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
use crate::office::protocol::{
    new_request, round_trip, OfficeRequest, OfficeRequestKind, OfficeResponse,
};
use crate::office::OFFICE_SOCKET_NAME;

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
        spawn_detached_server(home)?;
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
        let OfficeResponse { ok, result, error, .. } = round_trip(&mut self.stream, &request)?;
        if ok {
            Ok(result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(OfficeError::Validation(error.unwrap_or_else(|| {
                "rejected without a reason".into()
            })))
        }
    }

    /// Raw access for callers that need the response envelope (rare).
    pub fn round_trip_raw(&mut self, request: &OfficeRequest) -> OfficeResult<OfficeResponse> {
        round_trip(&mut self.stream, request)
    }
}

/// Spawn `viva server` detached: its own process group (so this client's
/// death signals nothing), stdin dropped, stdout+stderr appended to
/// `server.log` in the 0700 home. Returns once the spawn is handed to the
/// OS — readiness is the caller's polling job.
fn spawn_detached_server(home: &Path) -> OfficeResult<()> {
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
    command
        .spawn()
        .map_err(|e| OfficeError::Validation(format!("failed to spawn the resident server: {e}")))?;
    Ok(())
}
