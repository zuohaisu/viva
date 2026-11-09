//! Executed only by the explicit native integration build, never counted as
//! coverage from the default zero-test feature-disabled run. Not UI acceptance.
#![cfg(all(feature = "herdr-runtime", unix))]
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use viva::{
    foundation::{paths, store::Store},
    members::MemberRegistry,
    tasks::TaskRegistry,
};

struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        // Exact task-owned PID. Forced fixture cleanup is not lifecycle/UI PASS.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn call(home: &Path, method: &str, params: Value) -> Value {
    let mut socket = UnixStream::connect(home.join("runtime/workbench/herdr.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        socket,
        "{}",
        json!({"id":"fixture-secret-do-not-persist", "method":method, "params":params,
            "transport_peer":{"pid":1,"uid":0}, "peer_pid":1, "peer_uid":0, "claimed_role":"user"})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn actual_native_server_reads_canonical_facts_and_audits_unbound_refusals() {
    let temp = tempfile::Builder::new()
        .prefix("vn-")
        .tempdir_in("/tmp")
        .unwrap();
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
    let log = std::fs::File::create(home.join("native.log")).unwrap();
    let mut child = Fixture(
        Command::new(env!("CARGO_BIN_EXE_viva"))
            .args(["runtime", "server"])
            .current_dir(home)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("VIVA_HOME", home)
            .env("XDG_CONFIG_HOME", &foreign)
            .env("XDG_STATE_HOME", &foreign)
            .env("HERDR_CONFIG_PATH", foreign.join("bad.toml"))
            .env("HERDR_SOCKET_PATH", foreign.join("bad.sock"))
            .env("HERDR_CLIENT_SOCKET_PATH", foreign.join("bad-client.sock"))
            .env("HERDR_SESSION", "foreign-session")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !runtime.join("herdr.sock").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "native host exited: {}",
            std::fs::read_to_string(home.join("native.log")).unwrap()
        );
        assert!(Instant::now() < deadline, "native startup deadline");
        thread::sleep(Duration::from_millis(25));
    }
    let response = call(home, "viva.snapshot", json!({}));
    let snapshot = &response["result"]["snapshot"];
    assert_eq!(snapshot["backend"], "herdr-source-library", "{response}");
    assert_eq!(
        snapshot["runtime_pid"].as_u64(),
        Some(u64::from(child.0.id()))
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
    assert_eq!(snapshot["member_dispatch"], "not_integrated");
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
        assert!(child.0.try_wait().unwrap().is_none());
    }
    let ping = call(home, "ping", json!({}));
    assert_eq!(ping["result"]["type"], "pong");
    assert_eq!(ping["result"]["capabilities"]["viva_domain_schema"], 1);
    assert_eq!(ping["result"]["capabilities"]["live_handoff"], false);
    assert_eq!(
        ping["result"]["capabilities"]["ssh_agent_registration"],
        false
    );
    // The explicit native preview must not silently invoke the old host/UI.
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .arg("status")
        .env("VIVA_HOME", home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("native development preview"));
    assert!(!home.join("server.sock").exists());
    assert_eq!(std::fs::read_dir(&foreign).unwrap().count(), 1);
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let recorded_host: String = store.connection().query_row(
        "SELECT subject_id FROM events WHERE kind='native_domain_attached' ORDER BY seq DESC LIMIT 1", [], |row| row.get(0)).unwrap();
    assert_eq!(snapshot["runtime_host_id"], recorded_host);
    assert_eq!(snapshot["execution_attribution"], "not_integrated");
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
