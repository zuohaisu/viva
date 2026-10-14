//! The client: the attach side of the resident-server split (V14
//! S1, issue #43; ADR 0012).
//!
//! Before this module the TUI process WAS the server. Now the host is
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

use crate::foundation::error::{Error, Result};
use crate::server::SOCKET_NAME;
use crate::server::protocol::{Request, RequestKind, Response, new_request, round_trip_within};

/// How long `ensure_server` waits for a freshly spawned server to bind its
/// socket before declaring the start failed.
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval while waiting for the socket.
const START_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// One connected client session. Requests are synchronous round trips; the
/// server handles each connection on its own thread.
pub struct Client {
    stream: UnixStream,
    home: std::path::PathBuf,
    last_activity: Instant,
    reconnect: bool,
}

impl Client {
    /// Connect to the healthy host at `home`. Fails honestly when the
    /// socket is absent or wedged — use [`ensure_server`] for the
    /// spawn-and-wait behavior.
    pub fn connect(home: &Path) -> Result<Self> {
        let socket_path = home.join(SOCKET_NAME);
        if !socket_path.exists() {
            return Err(Error::Validation(format!(
                "no active server in {} (start one with `viva server`)",
                home.display()
            )));
        }
        let stream = UnixStream::connect(&socket_path)?;
        let mut client = Self {
            stream,
            home: home.to_path_buf(),
            last_activity: Instant::now(),
            reconnect: false,
        };
        // Prove the peer is a healthy host before handing out the session.
        let response = client.call(RequestKind::Ping)?;
        if response.get("pid").is_none() {
            return Err(Error::Validation(
                "socket answered but the host handshake failed; refusing to attach".into(),
            ));
        }
        Ok(client)
    }

    /// Connect, spawning a detached resident server first when none is
    /// running. The spawned server outlives this process by construction:
    /// its own process group, its output in `server.log`.
    pub fn ensure_server(home: &Path) -> Result<Self> {
        if crate::foundation::legacy::old_control_socket_alive(home)? {
            return Err(Error::Validation(format!(
                "the previous control socket {} is still serving; refusing to start a second \
                 server. Stop the old server with the previous version's `viva shutdown`, then \
                 start Viva again to finish the migration",
                home.join(crate::foundation::legacy::OLD_SOCKET_NAME)
                    .display()
            )));
        }
        let socket_path = home.join(SOCKET_NAME);
        if socket_path.exists() {
            match Client::connect(home) {
                Ok(client) => return Ok(client),
                Err(err) => {
                    return Err(Error::Validation(format!(
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
                if let Ok(client) = Client::connect(home) {
                    return Ok(client);
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Validation(format!(
                    "the spawned server did not bind {} within {SERVER_START_TIMEOUT:?}; \
                     check {}/server.log",
                    socket_path.display(),
                    home.display()
                )));
            }
            std::thread::sleep(START_POLL_INTERVAL);
        }
    }

    /// A separate socket for background view/input work; never clone a stream
    /// whose request/response correlation belongs to another thread.
    pub fn independent(&self) -> Result<Self> {
        Self::connect(&self.home)
    }
    /// The host bounds an idle connection at 10s. Reconnect BEFORE sending
    /// after 8s idle. A transport failure marks the socket for the NEXT call;
    /// it never retries an already sent, possibly applied mutation.
    fn exchange(&mut self, request: &Request, timeout: Duration) -> Result<Response> {
        if self.reconnect || self.last_activity.elapsed() >= Duration::from_secs(8) {
            *self = Self::connect(&self.home)?;
        }
        let result = round_trip_within(&mut self.stream, request, timeout);
        self.last_activity = Instant::now();
        self.reconnect = result.is_err();
        result
    }
    /// One request → result value, or the server's error text as a
    /// validation error.
    pub fn call(&mut self, kind: RequestKind) -> Result<serde_json::Value> {
        self.call_request(new_request(kind))
    }

    /// [`Self::call`] with a pre-built request (grant fields set).
    pub fn call_request(&mut self, request: Request) -> Result<serde_json::Value> {
        let Response {
            ok, result, error, ..
        } = self.exchange(&request, crate::server::protocol::IO_TIMEOUT)?;
        if ok {
            Ok(result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(Error::Validation(
                error.unwrap_or_else(|| "rejected without a reason".into()),
            ))
        }
    }

    /// Raw access for callers that need the response envelope (rare).
    pub fn round_trip_raw(&mut self, request: &Request) -> Result<Response> {
        self.exchange(request, crate::server::protocol::IO_TIMEOUT)
    }

    /// One request with an explicit read budget. The budget is carried
    /// through the whole round trip — `read_message` used to reset it to
    /// IO_TIMEOUT mid-flight, voiding this API entirely (QA F14: a 16s
    /// sync errored at the client at 10.03s while the server succeeded).
    pub fn call_with_timeout(
        &mut self,
        kind: RequestKind,
        read_timeout: std::time::Duration,
    ) -> Result<serde_json::Value> {
        self.call_request_with_timeout(new_request(kind), read_timeout)
    }

    /// [`Self::call_with_timeout`] for a pre-built request (grant fields
    /// intact).
    pub fn call_request_with_timeout(
        &mut self,
        request: Request,
        read_timeout: std::time::Duration,
    ) -> Result<serde_json::Value> {
        let Response {
            ok, result, error, ..
        } = self.exchange(&request, read_timeout)?;
        if ok {
            Ok(result.unwrap_or(serde_json::Value::Null))
        } else {
            Err(Error::Validation(
                error.unwrap_or_else(|| "rejected without a reason".into()),
            ))
        }
    }
}

/// Spawn `viva server` detached: its own process group (so this client's
/// death signals nothing), stdin dropped, stdout+stderr appended to
/// `server.log` in the 0700 home. Returns once the spawn is handed to the
/// OS — readiness is the caller's polling job.
fn spawn_detached_server(home: &Path, extra_args: &[&str]) -> Result<()> {
    spawn_detached_server_from(home, &std::env::current_exe()?, extra_args)
}

/// [`spawn_detached_server`] with an explicit entry point. The update flow
/// passes the freshly installed wrapper/binary: the updating process itself
/// is the OLD generation, and its on-disk executable has just been replaced.
fn spawn_detached_server_from(home: &Path, entry: &Path, extra_args: &[&str]) -> Result<()> {
    let log_path = home.join("server.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let err_log = log.try_clone()?;
    let mut command = Command::new(entry);
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
    command
        .spawn()
        .map_err(|e| Error::Validation(format!("failed to spawn the resident server: {e}")))?;
    Ok(())
}

/// A live-handoff restart (S3, issue #45): ask the running server to begin
/// the handoff, spawn the resumed server detached, and wait until a new
/// host answers on the control socket. Returns the new host's pid.
pub fn restart_server(home: &Path) -> Result<u32> {
    restart_server_with(home, &std::env::current_exe()?)
}

/// [`restart_server`] with an explicit server entry point for the resumed
/// host. The update flow passes the entry it just installed and verified —
/// the handover then IS the upgrade: the old generation hands its live
/// terminals to the new version.
pub fn restart_server_with(home: &Path, server_entry: &Path) -> Result<u32> {
    crate::foundation::paths::ensure_private_dir(home)?;
    // Do not ask a server that still runs on the previous table names to hand
    // over: the resumed server would have to rename them underneath it.
    crate::foundation::legacy::refuse_live_handoff(&crate::foundation::paths::database_path(home))?;
    // The OLD server must be running; a restart never spawns anything.
    // Record its identity: during the handoff window the old host keeps
    // answering, and the resumed host REBINDS the same socket path within
    // milliseconds of the old one leaving - so "socket gone" is not a
    // usable phase boundary. The identity change is.
    let mut client = Client::connect(home)?;
    let before = client.call(RequestKind::Ping)?;
    let old_pid = before.get("pid").and_then(|p| p.as_u64());
    let response = client.call(RequestKind::ServerRestart)?;
    let handoff_socket = response
        .get("handoff_socket")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Validation("restart response carried no handoff socket".into()))?
        .to_string();
    let _ = handoff_socket; // the resumed server finds it under the home
    drop(client);

    spawn_detached_server_from(home, server_entry, &["--resume"])?;

    // Wait until whoever answers is NOT the old host (QA F1 round 2): the
    // old host answers pings during the transfer window; the moment the
    // resumed host rebinds, the pid changes. A failed handoff leaves the
    // old host answering forever and times out honestly here.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(mut client) = Client::connect(home) {
            if let Ok(status) = client.call(RequestKind::Ping) {
                if let Some(pid) = status.get("pid").and_then(|p| p.as_u64()) {
                    if Some(pid) != old_pid {
                        return Ok(pid as u32);
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Validation(
                "45s after a restart request the old host is still the one serving - the \
                 handoff failed (see the recovery records and server.log)"
                    .into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::server::protocol::{read_message, write_message};
    use std::os::unix::net::UnixListener;
    #[test]
    fn expired_idle_socket_reconnects_but_an_ambiguous_mutation_is_never_replayed() {
        let home = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(home.path().join(SOCKET_NAME)).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let ping: Request = serde_json::from_str(&read_message(&mut first).unwrap()).unwrap();
            write_message(
                &mut first,
                &Response::ok(ping.request_id, serde_json::json!({"pid":1})),
            )
            .unwrap();
            // Idle connection is deliberately closed; the next operation must
            // arrive on a fresh connection rather than the stale fd.
            drop(first);
            let (mut second, _) = listener.accept().unwrap();
            let ping: Request = serde_json::from_str(&read_message(&mut second).unwrap()).unwrap();
            assert!(matches!(ping.kind, RequestKind::Ping));
            write_message(
                &mut second,
                &Response::ok(ping.request_id, serde_json::json!({"pid":1})),
            )
            .unwrap();
            let mutation: Request =
                serde_json::from_str(&read_message(&mut second).unwrap()).unwrap();
            assert!(matches!(mutation.kind, RequestKind::Workspace { .. }));
            drop(second); // applied, but its response was lost
            let (mut third, _) = listener.accept().unwrap();
            let ping: Request = serde_json::from_str(&read_message(&mut third).unwrap()).unwrap();
            assert!(matches!(ping.kind, RequestKind::Ping));
            write_message(
                &mut third,
                &Response::ok(ping.request_id, serde_json::json!({"pid":1})),
            )
            .unwrap();
            let request: Request =
                serde_json::from_str(&read_message(&mut third).unwrap()).unwrap();
            assert!(matches!(request.kind, RequestKind::Status));
            write_message(
                &mut third,
                &Response::ok(request.request_id, serde_json::json!({"safe":true})),
            )
            .unwrap();
        });
        let mut client = Client::connect(home.path()).unwrap();
        client.last_activity = Instant::now() - Duration::from_secs(12);
        assert!(
            client
                .call(RequestKind::Workspace {
                    action: "new".into(),
                    value: Some("once".into()),
                    project_id: None
                })
                .is_err()
        );
        assert_eq!(client.call(RequestKind::Status).unwrap()["safe"], true);
        worker.join().unwrap();
    }
}
