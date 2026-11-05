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
        // `--machine` dials that machine and must not open or spawn the
        // local default socket. `from_stream` performs the handshake on the
        // bridge, so this branch does not call back into `connect`.
        if let Some(record) = crate::machines::invocation_target() {
            return crate::machines::connect_client(&record);
        }
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

    /// Handshake on an already connected stream (an SSH bridge). Reconnect
    /// later still goes through [`Self::connect`], which honors `--machine`.
    pub fn from_stream(stream: UnixStream, home: &Path) -> Result<Self> {
        let mut client = Self {
            stream,
            home: home.to_path_buf(),
            last_activity: Instant::now(),
            reconnect: false,
        };
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
        if crate::machines::invocation_target().is_some() {
            return Self::connect(home);
        }
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
                // Nobody listens: a killed host left the socket behind.
                // The spawned server claims it with a record (`Host::open`).
                Err(Error::Io(err)) if err.kind() == std::io::ErrorKind::ConnectionRefused => {}
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

    /// Workbench startup after installation: attach to this executable's
    /// generation, handing live sessions over when an older host answers.
    /// Ordinary read/control clients deliberately keep using `ensure_server`:
    /// a status query must not unexpectedly upgrade a running host.
    ///
    /// The startup lock serializes independent clients. Binary hashes (not
    /// package versions or paths) distinguish same-version development builds
    /// and an executable replaced atomically at its original path. Hosts from
    /// before #191 omit the hash and get one compatibility handoff attempt.
    pub fn ensure_current_server(home: &Path) -> Result<Self> {
        if crate::machines::invocation_target().is_some() {
            // Never launch this machine's binary on a remote peer implicitly.
            return Self::connect(home);
        }
        let _startup = StartupLock::acquire(home)?;
        let expected = executable_sha256()?;
        let mut client = Self::ensure_server(home)?;
        let before = client.call(RequestKind::Ping)?;
        if before.get("binary_sha256").and_then(|v| v.as_str()) == Some(&expected) {
            return Ok(client);
        }
        if let Some(runtime) = before.get("runtime").and_then(|v| v.as_str()) {
            if runtime != "viva-rust-v1" {
                return Err(Error::Validation(format!(
                    "automatic handoff cannot yet import runtime `{runtime}`; \
                     the existing host was not stopped"
                )));
            }
        }
        drop(client);
        restart_server(home)?;
        let mut client = Self::connect(home)?;
        let after = client.call(RequestKind::Ping)?;
        if after.get("binary_sha256").and_then(|v| v.as_str()) != Some(&expected) {
            return Err(Error::Validation(
                "handoff returned a host from an unexpected executable; \
                 refusing to report the upgrade as successful"
                    .into(),
            ));
        }
        Ok(client)
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

/// A per-process identity captured before a host starts serving. Caching
/// prevents a later on-disk replacement from changing the old host's identity.
pub(super) fn executable_sha256() -> Result<String> {
    use std::io::Read as _;
    use std::sync::OnceLock;

    use sha2::{Digest as _, Sha256};

    static HASH: OnceLock<std::result::Result<String, String>> = OnceLock::new();
    HASH.get_or_init(|| {
        (|| -> std::io::Result<String> {
            let mut file = std::fs::File::open(std::env::current_exe()?)?;
            let mut hash = Sha256::new();
            let mut bytes = [0u8; 64 * 1024];
            loop {
                let n = file.read(&mut bytes)?;
                if n == 0 {
                    break;
                }
                hash.update(&bytes[..n]);
            }
            Ok(format!("{:x}", hash.finalize()))
        })()
        .map_err(|err| format!("cannot identify this executable: {err}"))
    })
    .clone()
    .map_err(Error::Validation)
}

/// Reuses the same OS file-lock primitive as the store. Never unlink the
/// lock: waiters must continue locking the same inode. Dropping it unlocks,
/// including on every failed handoff path. No process-wide environment edits.
struct StartupLock(std::fs::File);

impl StartupLock {
    fn acquire(home: &Path) -> Result<Self> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::OpenOptionsExt as _;

        crate::foundation::paths::ensure_private_dir(home)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home.join("startup.lock"))?;
        if !file.metadata()?.is_file() {
            return Err(Error::Validation(
                "startup.lock is not a regular file; no existing host was changed".into(),
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            // SAFETY: `file` owns this descriptor for the guard's lifetime.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::WouldBlock
                && err.kind() != std::io::ErrorKind::Interrupted
            {
                return Err(err.into());
            }
            if Instant::now() >= deadline {
                return Err(Error::Validation(
                    "another startup/handoff still holds startup.lock after 60s; \
                     no existing session was stopped by this caller"
                        .into(),
                ));
            }
            std::thread::sleep(START_POLL_INTERVAL);
        }
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd as _;
        // SAFETY: the guard still owns the descriptor. Closing also releases
        // the lock if unlocking is interrupted.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
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
    // A first run with a fresh VIVA_HOME has no directory yet.
    crate::foundation::paths::ensure_private_dir(home)?;
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

    #[test]
    fn executable_identity_is_stable_and_matches_actual_bytes() {
        use sha2::{Digest as _, Sha256};
        let expected = format!(
            "{:x}",
            Sha256::digest(std::fs::read(std::env::current_exe().unwrap()).unwrap())
        );
        assert_eq!(executable_sha256().unwrap(), expected);
        assert_eq!(executable_sha256().unwrap(), expected);
    }

    #[test]
    fn startup_lock_rejects_symlinks_without_changing_the_target() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("unrelated");
        std::fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, home.path().join("startup.lock")).unwrap();
        assert!(StartupLock::acquire(home.path()).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
    }

    #[test]
    fn incompatible_runtime_is_rejected_without_requesting_a_restart() {
        use crate::server::protocol::{read_message, write_message};
        use std::os::unix::net::UnixListener;

        let home = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(home.path().join(SOCKET_NAME)).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            for _ in 0..2 {
                let request: Request =
                    serde_json::from_str(&read_message(&mut stream).unwrap()).unwrap();
                assert!(matches!(request.kind, RequestKind::Ping));
                write_message(
                    &mut stream,
                    &Response::ok(
                        request.request_id,
                        serde_json::json!({"pid": 1, "runtime": "unrelated-runtime"}),
                    ),
                )
                .unwrap();
            }
            // The caller must close rather than mutate a runtime it cannot
            // import. No stop, unlink or ServerRestart request is sent.
            assert!(read_message(&mut stream).is_err());
        });
        let err = match Client::ensure_current_server(home.path()) {
            Ok(_) => panic!("foreign runtime must not be adopted"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("existing host was not stopped"));
        worker.join().unwrap();
        assert!(home.path().join(SOCKET_NAME).exists());
    }

    #[test]
    fn startup_lock_serializes_clients_and_releases_on_drop() {
        let home = tempfile::tempdir().unwrap();
        let first = StartupLock::acquire(home.path()).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let path = home.path().to_owned();
        let waiter = std::thread::spawn(move || {
            let _next = StartupLock::acquire(&path).unwrap();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        waiter.join().unwrap();
    }

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
