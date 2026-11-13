//! Executed only by the explicit native integration build, never counted as
//! coverage from the default zero-test feature-disabled run. Not UI acceptance.
#![cfg(all(feature = "herdr-runtime", unix))]
#[path = "support/owner_anchor.rs"]
mod owner_support;
use owner_support::{call, spawn_with, wait_ready};
use serde_json::{Value, json};
use std::{
    io::Read,
    os::unix::net::UnixStream,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use viva::{
    foundation::{paths, store::Store},
    members::MemberRegistry,
    tasks::TaskRegistry,
};

#[test]
fn actual_native_server_reads_canonical_facts_and_audits_unbound_refusals() {
    let temp = tempfile::Builder::new().prefix("vn").tempdir().unwrap();
    let home = temp.path();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let member = MemberRegistry::new(&store)
        .register("Native fixture member")
        .unwrap();
    let task = TaskRegistry::new(&store)
        .create_task(
            "Native fixture task",
            vec![],
            Some(member.member_id.clone()),
            None,
            None,
        )
        .unwrap();
    drop(store);
    let runtime = home.join("runtime/workbench");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(runtime.join("config.toml"), "[terminal]\ndefault_shell = '/bin/sh'\nshell_mode = 'non_login'\n[update]\nversion_check = true\nmanifest_check = true\n[experimental]\npane_history = true\n").unwrap();
    let foreign = home.join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("sentinel"), "unchanged").unwrap();
    let mut child = spawn_with(home, "bridge-snapshot", |command| {
        command
            .current_dir(home)
            .env("XDG_CONFIG_HOME", &foreign)
            .env("XDG_STATE_HOME", &foreign)
            .env("HERDR_CONFIG_PATH", foreign.join("bad.toml"))
            .env("HERDR_SOCKET_PATH", foreign.join("bad.sock"))
            .env("HERDR_CLIENT_SOCKET_PATH", foreign.join("bad-client.sock"))
            .env("HERDR_SESSION", "foreign-session");
    });
    wait_ready(home, &child);
    let response = call(home, "viva.snapshot", json!({}));
    let snapshot = &response["result"]["snapshot"];
    assert_eq!(snapshot["backend"], "herdr-source-library", "{response}");
    assert_eq!(
        snapshot["runtime_pid"].as_u64(),
        Some(u64::from(child.pid()))
    );
    assert_eq!(
        snapshot["members"][0]["member_id"],
        member.member_id.as_str()
    );
    assert_eq!(
        snapshot["members"][0]["display_name"],
        "Native fixture member"
    );
    assert_eq!(snapshot["tasks"][0]["task_id"], task.task_id.as_str());
    assert_eq!(snapshot["member_dispatch"], "owner_gated_launch_spec");
    // Real same-UID socket, no host-issued capability: reject before hello/input.
    let mut raw = UnixStream::connect(runtime.join("herdr-client.sock")).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut byte = [0u8; 1];
    match raw.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("unbound raw client was not closed before protocol: {other:?}"),
    }
    let invalid = call(
        home,
        "viva.client.authenticate",
        json!({"credential":"fixture-bootstrap-do-not-persist"}),
    );
    assert_eq!(
        invalid["error"]["code"], "owner_anchor_mismatch",
        "{invalid}"
    );
    // The real frontend/PTY probe separately proves default-file reattachment.
    assert!(home.join("runtime-capability/owner-anchor").is_file());
    assert!(!runtime.join("owner-channel.json").exists());
    for method in ["server.stop", "server.live_handoff", "workspace.create"] {
        let params = if method == "server.live_handoff" {
            json!({"request_id":"forged", "member_id":"owner", "grant_id":"forged"})
        } else {
            json!({"member_id":"owner", "grant_id":"forged"})
        };
        let response = call(home, method, params);
        assert_eq!(
            response["error"]["code"], "authority_not_integrated",
            "{method}: {response}"
        );
        assert!(child.try_wait().unwrap().is_none());
    }
    let ping = call(home, "ping", json!({}));
    assert_eq!(ping["result"]["type"], "pong");
    assert_eq!(ping["result"]["capabilities"]["viva_domain_schema"], 1);
    assert_eq!(ping["result"]["capabilities"]["live_handoff"], false);
    assert_eq!(
        ping["result"]["capabilities"]["ssh_agent_registration"],
        false
    );
    // The legacy CLI must not silently invoke the old host/UI behind a native host.
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .arg("status")
        .env("VIVA_HOME", home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("a native runtime host owns this home")
    );
    assert!(!home.join("server.sock").exists());
    assert_eq!(std::fs::read_dir(&foreign).unwrap().count(), 1);
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let recorded_host: String = store.connection().query_row(
        "SELECT subject_id FROM events WHERE kind='native_domain_attached' ORDER BY seq DESC LIMIT 1", [], |row| row.get(0)).unwrap();
    assert_eq!(snapshot["runtime_host_id"], recorded_host);
    assert_eq!(
        snapshot["execution_attribution"],
        "pane_bound_owner_dispatch"
    );
    let audit: String = store
        .connection()
        .query_row(
            "SELECT group_concat(payload) FROM events WHERE kind='native_automation_refused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(audit.contains("server.stop"), "{audit}");
    assert!(audit.contains("server.live_handoff"), "{audit}");
    assert!(audit.contains("workspace.create"), "{audit}");
    assert!(audit.contains("native.raw_client"), "{audit}");
    assert!(audit.contains("viva.client.authenticate"), "{audit}");
    assert!(!audit.contains("fixture-bootstrap-do-not-persist"));
    assert!(!audit.contains("fixture-secret-do-not-persist"));
    assert!(!audit.contains("forged"));
    let peer: String = store.connection().query_row(
        "SELECT payload FROM events WHERE kind='native_automation_refused' AND json_extract(payload, '$.method')='server.stop' ORDER BY seq DESC LIMIT 1", [], |row| row.get(0)).unwrap();
    let peer: Value = serde_json::from_str(&peer).unwrap();
    assert_eq!(
        peer["actor"], "unbound",
        "same UID/PID must not imply owner"
    );
    assert_eq!(
        peer["transport_peer"]["pid"].as_u64(),
        Some(u64::from(std::process::id()))
    );
    // SAFETY: geteuid is an argument-free OS query.
    assert_eq!(
        peer["transport_peer"]["uid"].as_u64(),
        Some(u64::from(unsafe { libc::geteuid() }))
    );
    assert_eq!(
        TaskRegistry::new(&store).list_tasks(0, 100).unwrap(),
        vec![task]
    );
    assert!(runtime.join("herdr-client.sock").exists());
}

#[test]
fn owner_only_preview_refuses_unbound_pending_execution_without_reconciling_it() {
    use viva::foundation::records::{
        AttributionSnapshot, ExecutionRecord, get_execution, insert_execution,
    };
    let temp = tempfile::Builder::new().prefix("vn").tempdir().unwrap();
    let home = temp.path();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let member = MemberRegistry::new(&store)
        .register("pending fixture, no process")
        .unwrap();
    let task = TaskRegistry::new(&store)
        .create_task(
            "not a native dispatch",
            vec![],
            Some(member.member_id.clone()),
            None,
            None,
        )
        .unwrap();
    let execution = ExecutionRecord::start(
        task.task_id.clone(),
        AttributionSnapshot::capture(member.member_id, "fixture", "fixture", "fixture"),
    );
    insert_execution(&store, &execution).unwrap();
    // Keep this original connection: reopening with Store::open would itself
    // reconcile data and invalidate the no-reconciliation assertion.
    paths::ensure_private_dir(home).unwrap();
    let log_path = home.join("native.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(["runtime", "server"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("VIVA_HOME", home)
        .env("HOME", home)
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log_path).unwrap())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "owner-only preview unexpectedly stayed active"
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert!(!status.success());
    assert!(
        std::fs::read_to_string(&log_path)
            .unwrap()
            .contains("native member/process bindings are not integrated")
    );
    assert!(!home.join("runtime/workbench/owner-channel.json").exists());
    assert!(!home.join("runtime/workbench/herdr-client.sock").exists());
    assert_eq!(
        get_execution(&store, &execution.execution_id).unwrap(),
        Some(execution)
    );
    assert_eq!(
        TaskRegistry::new(&store).get_task(&task.task_id).unwrap(),
        Some(task)
    );
}
