//! Prepared receiver for both historical v1 and current v2 senders.
//! Binding, schema compatibility, every session, and every reader thread
//! must be ready BEFORE the sender is allowed to retire. Readers remain
//! parked until commit so rejecting any entry cannot steal rollback output.

use super::*;
use std::os::fd::{AsRawFd as _, IntoRawFd as _, OwnedFd};

struct PreparedHost {
    host: Option<Host>,
}

impl Drop for PreparedHost {
    fn drop(&mut self) {
        if let Some(host) = self.host.take() {
            let shared = host.shared();
            shared.terminals.cancel_prepared();
            let watchers: Vec<_> = shared
                .watchers
                .lock()
                .expect("watchers")
                .drain(..)
                .collect();
            for watcher in watchers {
                let _ = watcher.join();
            }
            // Parked readers/watchers were cancelled without consuming
            // output, signalling children or writing process outcomes.
            if let Ok(store) = shared.store.lock() {
                let _ = store.connection().execute(
                    "UPDATE hosts SET exited_at = ?2, exit_kind = 'handoff_rejected' WHERE host_id = ?1",
                    rusqlite::params![shared.host_id, utc_now()],
                );
                let _ = record_recovery(
                    &store,
                    &shared.host_id,
                    "handoff_prepare_failed",
                    "candidate discarded before READY; sender retains its sessions",
                );
            }
            remove_owned_socket(&shared.socket_path, shared.socket_identity);
        }
    }
}

fn prepare(home: &Path, manifest: &HandoffManifest, fds: Vec<OwnedFd>) -> Result<PreparedHost> {
    // Alternate two bounded endpoint names so a resumed host can itself
    // transfer again without overwriting its own live listener.
    let path = [RESUME_SOCKET_NAME, RESUME_NEXT_SOCKET_NAME]
        .iter()
        .map(|name| home.join(name))
        .find(|path| {
            !path.exists()
                || matches!(UnixStream::connect(path),
                    Err(ref err) if err.kind() == std::io::ErrorKind::ConnectionRefused)
        })
        .ok_or_else(|| {
            Error::Validation(
                "both prepared endpoints are occupied; sender retains its sessions".into(),
            )
        })?;
    let host = Host::open_at(home, path, true)?;
    let prepared = PreparedHost { host: Some(host) };
    let shared = prepared.host.as_ref().expect("prepared host").shared();
    let mut ids = std::collections::HashSet::new();
    let mut pids = std::collections::HashSet::new();
    for (entry, fd) in manifest.entries.iter().zip(fds) {
        let terminal_id = TerminalId::from_str(&entry.terminal_id)?;
        if !ids.insert(entry.terminal_id.clone()) || !pids.insert(entry.pid) || entry.pid == 0 {
            return Err(Error::Validation(
                "duplicate terminal/pid or zero pid in handoff".into(),
            ));
        }
        let owner = owner_from_label(&entry.owner)?;
        let worktree = entry
            .worktree_id
            .as_deref()
            .map(WorktreeId::from_str)
            .transpose()?;
        let screen = hex_decode(&entry.screen_hex)?;
        let pending = hex_decode(&entry.pending_hex)?;
        // SAFETY: this OwnedFd is a descriptor we received and still own.
        if unsafe { libc::isatty(fd.as_raw_fd()) } != 1 {
            return Err(Error::Validation(
                "handoff descriptor is not a terminal".into(),
            ));
        }
        let watch = if let TerminalOwner::MemberExecution(execution) = &owner {
            let store = shared.store.lock().expect("store");
            if crate::foundation::records::get_execution(&store, execution)?.is_none() {
                return Err(Error::Validation(
                    "handoff references an unknown execution".into(),
                ));
            }
            let (key, task): (String, String) = store.connection().query_row(
                "SELECT request_key, task_id FROM launch_intents WHERE execution_id = ?1",
                [execution.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Some((key, TaskId::from_str(&task)?, execution.clone()))
        } else {
            None
        };
        let handle = crate::terminal::TerminalHandle::prepare_adoption(
            fd.into_raw_fd(),
            entry.pid,
            entry.pid_start_marker.clone(),
            entry.cols,
            entry.rows,
            entry.history.clone(),
            None,
            &screen,
            &pending,
        )?;
        shared.terminals.register_prepared(
            crate::terminal::TerminalEntry {
                terminal_id: terminal_id.clone(),
                owner,
                worktree_id: worktree,
                purpose: entry.purpose.clone(),
            },
            handle,
        );
        if let Some((key, task, execution)) = watch {
            let handle = shared
                .terminals
                .handle(&terminal_id)?
                .expect("prepared handle");
            register_exit_watcher(&shared, &handle, key, task, execution)?;
        }
    }
    // Exercise a real Ping round-trip through the already-bound listener
    // before READY, including accepted-socket configuration and response
    // serialization. Merely having a pathname is not readiness.
    let host = prepared.host.as_ref().expect("prepared host");
    let mut probe = UnixStream::connect(&shared.socket_path)?;
    let request = new_request(RequestKind::Ping);
    write_message(&mut probe, &request)?;
    let (mut peer, _) = host.listener.accept()?;
    peer.set_nonblocking(false)?;
    if !peer_is_owner(&peer) {
        return Err(Error::Validation("receiver preflight peer mismatch".into()));
    }
    let received: Request = serde_json::from_str(&read_message(&mut peer)?)?;
    if received.request_id != request.request_id || !matches!(received.kind, RequestKind::Ping) {
        return Err(Error::Validation(
            "receiver preflight connection changed; sender retained".into(),
        ));
    }
    let result = handle_request(shared.clone(), &received)?;
    write_message(&mut peer, &Response::ok(received.request_id, result))?;
    let response: Response = serde_json::from_str(&read_message(&mut probe)?)?;
    if !response.ok {
        return Err(Error::Validation("receiver preflight rejected".into()));
    }
    drop(peer);
    drop(probe);
    let store = shared.store.lock().expect("store");
    record_recovery(
        &store,
        &shared.host_id,
        "live_handoff_prepared",
        &format!(
            "prepared {} terminal(s) from `{}` on an already-bound endpoint; readers parked",
            shared.terminals.list().len(),
            manifest.host_id,
        ),
    )?;
    drop(store);
    Ok(prepared)
}

/// Publication is only an alias operation, NOT the point where we first
/// acquire the sessions or bind a listener. The prepared endpoint remains
/// reachable even if an old v1 host unlinks the conventional socket or a
/// filesystem obstruction prevents creating that alias.
fn publish_when_released(shared: Arc<Shared>) {
    let target = shared.home.join(SOCKET_NAME);
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    while target.exists() && Instant::now() < deadline {
        if shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        if socket_identity(&target).ok() == Some(shared.socket_identity) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if shared.stopping.load(Ordering::SeqCst) {
        return;
    }
    // Never remove/overwrite an unrelated file or socket to force success.
    if let Err(err) = std::fs::hard_link(&shared.socket_path, &target) {
        let store = shared.store.lock().expect("store");
        let _ = record_recovery(
            &store,
            &shared.host_id,
            "handoff_alias_unavailable",
            &format!(
                "normal socket alias unavailable ({err}); sessions remain served on the prepared endpoint"
            ),
        );
    }
}

pub(super) fn run(home: &Path) -> Result<()> {
    crate::foundation::paths::ensure_private_dir(home)?;
    crate::foundation::legacy::refuse_live_handoff(&crate::foundation::paths::database_path(home))?;
    let path = home.join(HANDOFF_SOCKET_NAME);
    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    let stream = loop {
        if let Ok(stream) = UnixStream::connect(&path) {
            break stream;
        }
        if Instant::now() >= deadline {
            return Err(Error::Validation(
                "handoff rendezvous did not become ready".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (manifest, fds) = handoff::receive_entries(&stream)?;
    let mut prepared = prepare(home, &manifest, fds)?;
    // No catch-and-continue adoption and no ACK on receipt. Any error above
    // drops all candidate handles while the sender still owns every master.
    handoff::acknowledge_prepared(&stream, &manifest)?;
    drop(stream);
    let host = prepared.host.take().expect("prepared host");
    host.shared.terminals.activate_prepared();
    {
        let store = host.shared.store.lock().expect("store");
        if let Err(err) = record_recovery(
            &store,
            &host.shared.host_id,
            "live_handoff_in",
            &format!(
                "activated all {} prepared terminal(s) from `{}`",
                host.shared.terminals.list().len(),
                manifest.host_id
            ),
        ) {
            // A post-commit audit error must not drop the only live owner.
            eprintln!("handoff committed; recovery audit write failed: {err}");
        }
    }
    let shared = host.shared();
    if let Err(err) = std::thread::Builder::new().spawn(move || publish_when_released(shared)) {
        eprintln!("handoff alias publisher unavailable ({err}); prepared endpoint stays active");
    }
    eprintln!(
        "server: resumed with {} prepared terminal(s)",
        host.shared.terminals.list().len()
    );
    host.serve()
}
