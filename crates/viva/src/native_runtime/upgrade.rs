//! #201: bounded, owner-gated native activation. No PID guessing, signals,
//! cold command replay, download source selection, or database rollback.
use super::{authenticate_owner, process_start_marker, send_native_request_with_timeout};
use crate::foundation::{Error, Result};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

fn invalid(message: impl Into<String>) -> Error {
    Error::Validation(message.into())
}

pub(super) fn run(home: &Path, args: &[String]) -> Result<()> {
    if args == ["build-info"] {
        println!(
            "{}",
            json!({"native_upgrade_protocol": 1,
            "binary_sha256": crate::server::executable_sha256()?})
        );
        return Ok(());
    }
    if args == ["status"] {
        let socket = home.join("runtime/workbench/herdr.sock");
        let state = snapshot(&socket)?;
        authenticate_owner(&socket, home)?;
        println!(
            "{}",
            json!({"runtime_host_id": state["runtime_host_id"],
            "runtime_binary_sha256": state["runtime_binary_sha256"],
            "runtime_pid": state["runtime_pid"], "runtime_start_marker": state["runtime_start_marker"]})
        );
        return Ok(());
    }
    let (check, entry) = match args {
        [flag, entry] if flag == "--entry" => (false, entry),
        [check, flag, entry] if check == "--check" && flag == "--entry" => (true, entry),
        _ => {
            return Err(invalid(
                "usage: viva runtime upgrade [--check] --entry <installed-viva>",
            ));
        }
    };
    if ["VIVA_MEMBER_ID", "VIVA_GRANT_ID", "VIVA_MEMBER_CREDENTIAL"]
        .iter()
        .any(|key| std::env::var_os(key).is_some())
    {
        return Err(invalid(
            "runtime upgrade is a local-controller installation operation, not a member action",
        ));
    }
    crate::native_runtime::owner_anchor::prepare_home(home)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(home.join("native-upgrade.lock"))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(invalid("another runtime upgrade is in progress"));
    }
    use std::os::unix::fs::MetadataExt;
    let metadata = lock.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(invalid(
            "unsafe native upgrade lock ownership/type/permissions",
        ));
    }
    let entry = std::fs::canonicalize(entry)?;
    let socket = home.join("runtime/workbench/herdr.sock");
    let Some(old) = snapshot_if_running(&socket)? else {
        if connect_if_running(&home.join("runtime/workbench/herdr-client.sock"))?.is_some() {
            return Err(invalid(
                "native client endpoint is still listening but host identity is unavailable; no handoff requested",
            ));
        }
        // An old runtime is not absence; do not hand it to the wrong protocol.
        if super::legacy_import::legacy_control_socket_present(home) {
            return Err(invalid(
                "legacy host present; native activation did not alter it",
            ));
        }
        println!("{}", json!({"state": "no_runtime", "started": false}));
        return Ok(());
    };
    if old["native_upgrade_protocol"] != 1 {
        return Err(invalid(
            "running native host does not support cooperative upgrade (including 0.3.10); installation/runtime unchanged by activation; live work retained",
        ));
    }
    let old_pid = old["runtime_pid"]
        .as_u64()
        .ok_or_else(|| invalid("host PID missing"))? as i64;
    let old_marker = old["runtime_start_marker"]
        .as_str()
        .ok_or_else(|| invalid("host birth identity missing"))?;
    if process_start_marker(old_pid).as_deref() != Some(old_marker) {
        return Err(invalid(
            "runtime birth identity changed; no handoff requested",
        ));
    }
    let installed = old["runtime_executable"]
        .as_str()
        .ok_or_else(|| invalid("runtime installation identity unavailable"))?;
    // current_exe may preserve /tmp while the entry resolves to /private/tmp
    // on Darwin. Normalize aliases, never accept a different installation.
    if std::fs::canonicalize(installed)? != entry {
        return Err(invalid(
            "runtime belongs to another installation; no handoff requested",
        ));
    }
    // Exact-schema, no migrations. Probe only a locally verified executable;
    // caller/installer is responsible for archive integrity and platform.
    let mut probe = std::process::Command::new(&entry);
    probe.args(["runtime", "upgrade", "build-info"]);
    let raw = crate::update::capture(&mut probe, Duration::from_secs(15))?;
    let target: Value = serde_json::from_str(&raw)?;
    if target["native_upgrade_protocol"] != 1
        || target["binary_sha256"]
            .as_str()
            .is_none_or(|s| s.len() != 64)
    {
        return Err(invalid(
            "candidate native upgrade protocol/build identity unavailable",
        ));
    }
    // Already-active is observation, not a handoff: active work must not
    // turn an up-to-date retry into a false failure. Installer --check stays
    // strict because its entry may still be the pre-replacement binary.
    if !check && old["runtime_binary_sha256"] == target["binary_sha256"] {
        println!(
            "{}",
            json!({"state": "already_active", "runtime_host_id": old["runtime_host_id"],
            "client_recovery": "not_needed_no_handoff"})
        );
        return Ok(());
    }
    if old["native_upgrade_ready"] != true {
        return Err(invalid(
            "active member execution/credential transfer is unsupported; runtime/work retained, no handoff requested",
        ));
    }
    authenticate_owner(&socket, home)?;
    if check {
        println!(
            "{}",
            json!({"state": "prepared", "runtime_host_id": old["runtime_host_id"], "runtime_binary_sha256": old["runtime_binary_sha256"], "runtime_start_marker": old["runtime_start_marker"], "target": target})
        );
        return Ok(());
    }
    eprintln!(
        "viva: phase=handoff; live shells survive; active member execution transfer is not supported"
    );
    // Herdr serializes the preflight and handoff with execution start/stop.
    // Its importer restores sockets/readers on pre-commit failures. Never kill
    // the old host or retry an ambiguous effect after a transport failure.
    send_native_request_with_timeout(&socket, "server.live_handoff", json!({"import_exe": entry}), Duration::from_secs(90))
        .map_err(|error| invalid(format!("activation failed or outcome unknown: {error}; inspect runtime identity before retrying; files-installed is not upgrade-success")))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let previous_clients = old["raw_clients"].as_array().cloned().unwrap_or_default();
    let mut last_check = json!({});
    loop {
        let ready = snapshot(&socket).ok().filter(|new| {
            let host_changed = new["runtime_host_id"] != old["runtime_host_id"];
            let target_build = new["runtime_binary_sha256"] == target["binary_sha256"];
            let old_exited = process_start_marker(old_pid).as_deref() != Some(old_marker);
            let clients_recovered = previous_clients.iter().all(|client| {
                new["raw_clients"]
                    .as_array()
                    .is_some_and(|current| current.contains(client))
            });
            last_check = json!({"host_changed": host_changed, "target_build": target_build,
                "old_exited": old_exited, "clients_recovered": clients_recovered});
            host_changed && target_build && old_exited && clients_recovered
        });
        if let Some(new) = ready {
            // Real fresh controller admission against the new incarnation,
            // not just a socket file or a disk --version probe.
            authenticate_owner(&socket, home)?;
            println!(
                "{}",
                json!({"state": "upgrade_complete",  "runtime_host_id": new["runtime_host_id"],
                "binary_sha256": target["binary_sha256"],
                "previous_frontend_count": previous_clients.len(),
                "fresh_controller_verified": true,
                "client_recovery": if previous_clients.is_empty() { "no_previous_clients" } else { "previous_frontends_negotiated_and_fresh_controller_verified" },
                "client_code": "compatible_existing_processes_not_reloaded"})
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(invalid(format!(
                "activation verification timed out: {last_check}; do not report upgrade success or blindly restore files",
            )));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn snapshot(socket: &Path) -> Result<Value> {
    snapshot_if_running(socket)?.ok_or_else(|| invalid("no native runtime is listening"))
}

fn absent_connection(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::NotFound
            | std::io::ErrorKind::NotConnected
    ) || error.raw_os_error() == Some(libc::ENOTSOCK)
}

fn connect_if_running(socket: &Path) -> Result<Option<std::os::unix::net::UnixStream>> {
    // Never unlink endpoints. A refused connection is absence, not authority;
    // unsafe ownership/symlinks and permission/timeouts remain real failures.
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(socket) {
        Ok(meta) if meta.uid() != unsafe { libc::geteuid() } || meta.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "unsafe native endpoint ownership or symlink",
            )
            .into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        _ => {}
    }
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(stream) => Ok(Some(stream)),
        Err(error) if absent_connection(&error) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn snapshot_if_running(socket: &Path) -> Result<Option<Value>> {
    // Match the serving socket's OS peer, not a claimed JSON PID or pidfile.
    let Some(stream) = connect_if_running(socket)? else {
        return Ok(None);
    };
    let mut uid = 0;
    let mut gid = 0;
    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0
            || uid != unsafe { libc::geteuid() }
        {
            return Err(invalid("native socket is not owned by the current user"));
        }
    }
    #[cfg(target_os = "linux")]
    let linux_peer = {
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&peer) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut peer as *mut _ as *mut _,
                &mut len,
            )
        } != 0
            || peer.uid != unsafe { libc::geteuid() }
        {
            return Err(invalid("native socket is not owned by the current user"));
        }
        peer.pid
    };
    #[cfg(not(target_os = "macos"))]
    let _ = (&mut uid, &mut gid);
    let value = send_native_request_with_timeout(
        socket,
        "viva.snapshot",
        json!({}),
        Duration::from_secs(5),
    )?;
    let snapshot = value["snapshot"].clone();
    #[cfg(target_os = "macos")]
    {
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                &mut pid as *mut _ as *mut _,
                &mut len,
            )
        } != 0
            || snapshot["runtime_pid"].as_i64() != Some(pid as i64)
        {
            return Err(invalid("runtime snapshot does not match socket peer"));
        }
    }
    #[cfg(target_os = "linux")]
    if snapshot["runtime_pid"].as_i64() != Some(linux_peer as i64) {
        return Err(invalid("runtime snapshot does not match socket peer"));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(invalid(
        "native upgrade process identity is unsupported on this OS",
    ));
    Ok(Some(snapshot))
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_definitive_connection_absence_is_no_runtime() {
        for errno in [
            libc::ECONNREFUSED,
            libc::ENOENT,
            libc::ENOTCONN,
            libc::ENOTSOCK,
        ] {
            assert!(super::absent_connection(
                &std::io::Error::from_raw_os_error(errno)
            ));
        }
        for errno in [libc::EACCES, libc::EPERM, libc::ETIMEDOUT, libc::EIO] {
            assert!(!super::absent_connection(
                &std::io::Error::from_raw_os_error(errno)
            ));
        }
    }
}
