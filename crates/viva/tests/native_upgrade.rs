//! Real native-process contracts, not packaged-install or human UI acceptance.
#![cfg(all(feature = "herdr-runtime", unix))]
#[path = "support/owner_anchor.rs"]
mod owner_support;
use owner_support::{authenticate, call, spawn, spawn_with, wait_owner_ready};
use serde_json::json;
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn handoff_requires_bound_controller_and_new_host_is_actually_ready() {
    let root = tempfile::Builder::new().prefix("vu").tempdir().unwrap();
    let home = root.path();
    let mut host = spawn(home, "upgrade");
    wait_owner_ready(home, &host);
    let before = call(home, "viva.snapshot", json!({}))["result"]["snapshot"].clone();
    let response = call(
        home,
        "server.live_handoff",
        json!({"import_exe": env!("CARGO_BIN_EXE_viva")}),
    );
    assert!(response.get("error").is_none(), "{response}");
    let deadline = Instant::now() + Duration::from_secs(15);
    while host.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "old host did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
    let after = call(home, "viva.snapshot", json!({}))["result"]["snapshot"].clone();
    assert_ne!(before["runtime_host_id"], after["runtime_host_id"]);
    assert_ne!(before["runtime_pid"], after["runtime_pid"]);
    assert_eq!(
        before["runtime_binary_sha256"],
        after["runtime_binary_sha256"]
    );
    let proof = std::fs::read_to_string(home.join("runtime-capability/owner-anchor")).unwrap();
    authenticate(home, proof.trim());
    assert!(
        call(home, "viva.host.pause", json!({"paused": true}))
            .get("error")
            .is_none()
    );
    // Exact task-created identity; never signal a production runtime.
    let pid = after["runtime_pid"].as_u64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
}

#[test]
fn failed_import_preserves_old_host_and_restores_its_capability() {
    let root = tempfile::Builder::new().prefix("vu").tempdir().unwrap();
    let home = root.path();
    let mut host = spawn_with(home, "rollback", |command| {
        command.env("HERDR_TEST_HANDOFF_IMPORT_FAIL", "after_domain_ready");
    });
    wait_owner_ready(home, &host);
    let before = call(home, "viva.snapshot", json!({}));
    let response = call(
        home,
        "server.live_handoff",
        json!({"import_exe": env!("CARGO_BIN_EXE_viva")}),
    );
    assert_eq!(response["error"]["code"], "handoff_failed", "{response}");
    assert!(host.try_wait().unwrap().is_none());
    assert_eq!(
        before["result"]["snapshot"]["runtime_host_id"],
        call(home, "viva.snapshot", json!({}))["result"]["snapshot"]["runtime_host_id"]
    );
    assert_eq!(
        std::fs::read_to_string(home.join("runtime-capability/owner-anchor"))
            .unwrap()
            .trim(),
        host.anchor()
    );
    authenticate(home, host.anchor());
    assert!(
        call(home, "viva.host.pause", json!({"paused": true}))
            .get("error")
            .is_none()
    );
}

#[test]
fn active_canonical_execution_refuses_upgrade_without_interrupting_work() {
    use viva::{
        authority::{AuthorityEngine, GrantMode},
        foundation::{ids::utc_now, paths, store::Store},
        members::{MemberBinding, MemberRegistry},
        tasks::TaskRegistry,
    };
    let root = tempfile::Builder::new().prefix("vu").tempdir().unwrap();
    let home = root.path();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let member = MemberRegistry::new(&store)
        .register("upgrade fixture")
        .unwrap();
    MemberRegistry::new(&store)
        .set_binding(&MemberBinding {
            member_id: member.member_id.clone(),
            role: "fixture".into(),
            model_binding: "fixture:model".into(),
            tools: vec![],
            updated_at: utc_now(),
        })
        .unwrap();
    let task = TaskRegistry::new(&store)
        .create_task(
            "upgrade work",
            vec![],
            Some(member.member_id.clone()),
            None,
            None,
        )
        .unwrap();
    let grant = AuthorityEngine::new(&store)
        .issue_root_grant(
            Some(member.member_id.clone()),
            Some(task.task_id.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let host = spawn(home, "active");
    wait_owner_ready(home, &host);
    let started = call(
        home,
        "viva.execution.start",
        json!({"member_id": member.member_id.as_str(), "task_id": task.task_id.as_str(), "grant_id": grant.grant_id.as_str(), "request_key": "upgrade-work", "argv": ["/bin/sleep", "60"], "cwd": "/tmp"}),
    );
    assert!(started.get("error").is_none(), "{started}");
    let response = call(
        home,
        "server.live_handoff",
        json!({"import_exe": env!("CARGO_BIN_EXE_viva")}),
    );
    assert_eq!(response["error"]["code"], "handoff_failed", "{response}");
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args([
            "runtime",
            "upgrade",
            "--check",
            "--entry",
            env!("CARGO_BIN_EXE_viva"),
        ])
        .env_clear()
        .env("HOME", home)
        .env("VIVA_HOME", home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("active member execution"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // No-op activation is observation, not a request to transfer active work.
    let same_build = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(["runtime", "upgrade", "--entry", env!("CARGO_BIN_EXE_viva")])
        .env_clear()
        .env("HOME", home)
        .env("VIVA_HOME", home)
        .output()
        .unwrap();
    assert!(
        same_build.status.success(),
        "{}",
        String::from_utf8_lossy(&same_build.stderr)
    );
    assert!(String::from_utf8_lossy(&same_build.stdout).contains("already_active"));
    assert_eq!(
        call(home, "viva.snapshot", json!({}))["result"]["snapshot"]["runtime_pid"],
        host.pid()
    );
    let id = started["result"]["execution_id"].as_str().unwrap();
    let status: String = store
        .connection()
        .query_row(
            "SELECT status FROM executions WHERE execution_id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "running");
    assert!(
        call(home, "viva.execution.stop", json!({"execution_id": id}))
            .get("error")
            .is_none()
    );
}

#[test]
fn stale_native_endpoints_are_absence_without_unlink_or_daemon_spawn() {
    use std::os::unix::net::UnixListener;
    let root = tempfile::Builder::new().prefix("vs").tempdir().unwrap();
    for regular in [false, true] {
        let home = root.path().join(if regular { "file" } else { "stale" });
        viva::foundation::paths::ensure_private_dir(&home).unwrap();
        let socket = home.join("runtime/workbench/herdr.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        if regular {
            std::fs::write(&socket, b"not-a-listener").unwrap();
        } else {
            drop(UnixListener::bind(&socket).unwrap());
        }
        for check in [false, true] {
            let mut cli = Command::new(env!("CARGO_BIN_EXE_viva"));
            cli.args(["runtime", "upgrade"]);
            if check {
                cli.arg("--check");
            }
            let output = cli
                .args(["--entry", env!("CARGO_BIN_EXE_viva")])
                .env_clear()
                .env("HOME", &home)
                .env("VIVA_HOME", &home)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("no_runtime"));
            assert!(
                socket.exists(),
                "a stale endpoint must not be deleted by the updater"
            );
            assert!(!home.join("runtime-capability/owner-anchor").exists());
        }
    }
}

#[test]
fn unsafe_or_partially_live_native_endpoints_are_not_absence() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let root = tempfile::Builder::new().prefix("vs").tempdir().unwrap();
    let home = root.path();
    viva::foundation::paths::ensure_private_dir(home).unwrap();
    let runtime = home.join("runtime/workbench");
    std::fs::create_dir_all(&runtime).unwrap();
    let api = runtime.join("herdr.sock");
    symlink(home.join("missing"), &api).unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_viva"))
            .args([
                "runtime",
                "upgrade",
                "--check",
                "--entry",
                env!("CARGO_BIN_EXE_viva"),
            ])
            .env_clear()
            .env("HOME", home)
            .env("VIVA_HOME", home)
            .output()
            .unwrap()
    };
    let unsafe_path = invoke();
    assert!(!unsafe_path.status.success());
    assert!(String::from_utf8_lossy(&unsafe_path.stderr).contains("unsafe native endpoint"));
    assert!(api.symlink_metadata().unwrap().file_type().is_symlink());
    std::fs::remove_file(&api).unwrap(); // remove only this test's symlink
    let raw = UnixListener::bind(runtime.join("herdr-client.sock")).unwrap();
    let partial_host = invoke();
    assert!(!partial_host.status.success());
    assert!(String::from_utf8_lossy(&partial_host.stderr).contains("still listening"));
    drop(raw);
}

#[test]
fn concurrent_activation_is_rejected_even_without_a_runtime() {
    use std::{
        fs::OpenOptions,
        os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    };
    let root = tempfile::Builder::new().prefix("vu").tempdir().unwrap();
    viva::foundation::paths::ensure_private_dir(root.path()).unwrap();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.path().join("native-upgrade.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(["runtime", "upgrade", "--entry", env!("CARGO_BIN_EXE_viva")])
        .env_clear()
        .env("HOME", root.path())
        .env("VIVA_HOME", root.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("another runtime upgrade"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unbound_process_cannot_start_handoff_and_absent_runtime_is_not_spawned() {
    let root = tempfile::Builder::new().prefix("vu").tempdir().unwrap();
    let home = root.path().join("live");
    let host = spawn(&home, "refusal");
    owner_support::wait_ready(&home, &host);
    let before = call(&home, "viva.snapshot", json!({}));
    let response = call(
        &home,
        "server.live_handoff",
        json!({"import_exe": env!("CARGO_BIN_EXE_viva")}),
    );
    assert_eq!(response["error"]["code"], "authority_not_integrated");
    assert_eq!(
        before["result"]["snapshot"]["runtime_host_id"],
        call(&home, "viva.snapshot", json!({}))["result"]["snapshot"]["runtime_host_id"]
    );
    let empty = root.path().join("empty");
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(["runtime", "upgrade", "--entry", env!("CARGO_BIN_EXE_viva")])
        .env_clear()
        .env("HOME", root.path())
        .env("VIVA_HOME", &empty)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("no_runtime"));
    assert!(!empty.join("runtime/workbench/herdr.sock").exists());
}
