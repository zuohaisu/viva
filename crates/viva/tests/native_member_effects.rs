//! P2 source-linked member effects and persistence boundaries, private fixtures only.
#![cfg(all(feature = "herdr-runtime", unix))]
#[path = "support/owner_anchor.rs"]
mod owner_support;
use owner_support::{OwnerHost, authenticate, call, spawn_with, wait_owner_ready};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use viva::{
    authority::{AuthorityEngine, GrantMode},
    foundation::{paths, store::Store},
    members::{MemberBinding, MemberRegistry},
    tasks::TaskRegistry,
};
const SECRET: &str = "fixture-P2-configured-secret-not-a-real-key";
include!("support/member_effect_cases.rs");
fn binary() -> PathBuf {
    std::env::var_os("VIVA_NATIVE_TEST_BIN")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_viva").into())
        .into()
}
fn wait(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !check() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        thread::sleep(Duration::from_millis(25));
    }
}
fn member(store: &Store, name: &str) -> String {
    let registry = MemberRegistry::new(store);
    let member = registry.register(name).unwrap();
    registry
        .set_binding(&MemberBinding {
            member_id: member.member_id.clone(),
            role: "fixture".into(),
            model_binding: "fixture:model".into(),
            tools: vec!["fixture-tool".into()],
            updated_at: viva::foundation::ids::utc_now(),
        })
        .unwrap();
    member.member_id.to_string()
}
fn start(home: &Path) -> OwnerHost {
    let host = spawn_with(home, "p2", |command| {
        command.env("HERDR_LOG", "herdr=debug");
    });
    wait_owner_ready(home, &host);
    authenticate(home, host.anchor());
    host
}
fn launch(home: &Path, task: &str, member: &str, grant: &str, key: &str, script: &str) -> Value {
    let response = call(
        home,
        "viva.execution.start",
        json!({"task_id":task,"member_id":member,"grant_id":grant,"request_key":key,"cwd":home,"argv":["/bin/sh","-c",script]}),
    );
    assert!(
        response.get("error").is_none(),
        "launch refused: {response}"
    );
    response["result"].clone()
}
fn shutdown(home: &Path, host: &mut OwnerHost) {
    let out = call(home, "server.stop", json!({}));
    assert!(out.get("error").is_none(), "{out}");
    wait("owned host exit", || host.try_wait().unwrap().is_some());
}
fn load(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn bind_fields(home: &Path, credential: &str, action: &str, extra: Value) -> Value {
    let mut params = extra;
    params["credential"] = json!(credential);
    params["action"] = json!(action);
    // Fake JSON provenance must not override the accepted OS peer.
    params["transport_peer"] = json!({"pid":0,"uid":0});
    params["claimed_role"] = json!("user");
    call(home, "viva.member.effect", params)
}

#[test]
fn bound_member_dispatches_and_hands_off_without_becoming_user() {
    let temp = tempfile::Builder::new().prefix("p2").tempdir().unwrap();
    let home = temp.path();
    std::fs::write(home.join("redaction.secrets"), SECRET).unwrap();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let coordinator = member(&store, "coordinator");
    let worker = member(&store, "worker");
    let tasks = TaskRegistry::new(&store);
    let source_task = tasks
        .create_task("source", vec![], None, None, None)
        .unwrap();
    let target_task = tasks
        .create_task("target", vec![], None, None, None)
        .unwrap();
    let engine = AuthorityEngine::new(&store);
    let source = engine
        .issue_root_grant(
            Some(coordinator.parse().unwrap()),
            None,
            vec!["dispatch_delegated".into(), "member_report".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let child = engine
        .delegate_grant(
            &source.grant_id,
            worker.parse().unwrap(),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let mut host = start(home);
    let handoff = home.join("handoff-result.json");
    let handoff_replay = home.join("handoff-replay.json");
    let dispatched = home.join("dispatch-result.json");
    let replay = home.join("replay-result.json");
    let credential = home.join("fixture-credential");
    let member_stderr = home.join("fixture-member-stderr");
    let handoff_again = format!(
        "'{}' handoff --task \"$VIVA_TASK_ID\" --member \"$VIVA_MEMBER_ID\" --summary 'continue {SECRET}' > '{}'",
        binary().display(),
        handoff_replay.display()
    );
    let script = format!(
        "exec 2> '{}'; set -e; umask 077; printf '%s' \"$VIVA_MEMBER_CREDENTIAL\" > '{}'; '{}' handoff --task \"$VIVA_TASK_ID\" --member \"$VIVA_MEMBER_ID\" --summary 'continue {SECRET}' > '{}'; {handoff_again}; '{}' dispatch --task '{}' --member '{}' --grant '{}' --request-key member-child --cwd '{}' -- /bin/sh -c 'sleep 120' > '{}'; '{}' dispatch --task '{}' --member '{}' --grant '{}' --request-key member-child --cwd '{}' -- /bin/sh -c 'sleep 120' > '{}'; sleep 120",
        member_stderr.display(),
        credential.display(),
        binary().display(),
        handoff.display(),
        binary().display(),
        target_task.task_id,
        worker,
        child.grant_id,
        home.display(),
        dispatched.display(),
        binary().display(),
        target_task.task_id,
        worker,
        child.grant_id,
        home.display(),
        replay.display()
    );
    let parent = launch(
        home,
        source_task.task_id.as_str(),
        &coordinator,
        source.grant_id.as_str(),
        "parent",
        &script,
    );
    wait("bound handoff and dispatch outputs", || {
        let exit: Option<i64> = store
            .connection()
            .query_row(
                "SELECT process_exit FROM executions WHERE execution_id=?1",
                [parent["execution_id"].as_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        if let Some(code) = exit {
            let screen = call(
                home,
                "pane.read",
                json!({"pane_id":parent["pane_id"], "source":"recent", "format":"text"}),
            );
            panic!(
                "member fixture exited {code}: {} / {}",
                screen["result"]["read"]["text"],
                std::fs::read_to_string(&member_stderr).unwrap_or_default()
            );
        }
        replay.metadata().is_ok_and(|m| m.len() > 0)
    });
    let first = load(&dispatched);
    let second = load(&replay);
    assert_eq!(first["execution_id"], second["execution_id"]);
    assert_eq!(first["pid"], second["pid"]);
    assert!(first["terminal_id"].is_string());
    assert_eq!(first["terminal_id"], second["terminal_id"]);
    assert_eq!(second["replayed"], true);
    assert!(load(&handoff)["event_seq"].as_i64().is_some());
    assert_eq!(
        load(&handoff)["event_seq"],
        load(&handoff_replay)["event_seq"]
    );
    let requesters: Vec<String> = store
        .connection()
        .prepare("SELECT payload FROM events WHERE kind='native_member_dispatch_prepared'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(requesters.len(), 2);
    for raw in requesters {
        let p: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(p["requester_execution_id"], parent["execution_id"]);
        assert_eq!(p["requester_member_id"], coordinator);
        assert_eq!(p["human_presence_verified"], false);
    }
    let cli = Command::new(binary())
        .args(["brief", source_task.task_id.as_str()])
        .env_clear()
        .env("HOME", home)
        .env("VIVA_HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let brief: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(
        brief["member_handoffs"][0]["source"],
        "native_member_process"
    );
    assert!(
        brief["member_handoffs"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    );
    assert!(!String::from_utf8_lossy(&cli.stdout).contains(SECRET));
    use sha2::{Digest, Sha256};
    let claim = &brief["member_handoffs"][0];
    let public_key = format!(
        "{:x}",
        Sha256::digest(
            format!(
                "{}:{}",
                parent["execution_id"].as_str().unwrap(),
                claim["summary"].as_str().unwrap()
            )
            .as_bytes()
        )
    );
    assert_eq!(claim["request_key"], public_key);
    assert_ne!(
        tasks
            .require_task(&source_task.task_id)
            .unwrap()
            .status
            .as_str(),
        "done"
    );
    // Stolen credential + fabricated JSON peer/role still cannot act from this
    // external process. All further permission tests use actual member children.
    let token = std::fs::read_to_string(&credential).unwrap();
    let forged = bind_fields(home, &token, "handoff", json!({"detail":"forged"}));
    assert!(
        forged.get("error").is_some(),
        "forged external peer accepted"
    );
    let raw: String = store
        .connection()
        .query_row(
            "SELECT argv FROM launch_specs WHERE execution_id=?1",
            [parent["execution_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!raw.contains(SECRET));
    assert!(!raw.contains(&token));
    let count: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_member_handoff'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    shutdown(home, &mut host);
}

#[test]
fn a_bound_worker_reports_a_handoff_under_the_report_action() {
    let temp = tempfile::Builder::new().prefix("p2").tempdir().unwrap();
    let home = temp.path();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let worker = member(&store, "worker");
    let task = TaskRegistry::new(&store)
        .create_task("worker handoff", vec![], None, None, None)
        .unwrap();
    let grant = AuthorityEngine::new(&store)
        .issue_root_grant(
            Some(worker.parse().unwrap()),
            Some(task.task_id.clone()),
            // Dispatch plus reporting: no terminal-control permission.
            vec!["dispatch_delegated".into(), "member_report".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let mut host = start(home);
    let output = home.join("worker-handoff.json");
    let execution = launch(
        home,
        task.task_id.as_str(),
        &worker,
        grant.grant_id.as_str(),
        "worker-handoff",
        &format!(
            "'{}' handoff --summary worker-done > '{}'; sleep 120",
            binary().display(),
            output.display()
        ),
    );
    assert!(execution["pane_id"].is_string());
    wait("worker handoff output", || {
        output.metadata().is_ok_and(|meta| meta.len() > 0)
    });
    let reply: Value = load(&output);
    assert!(reply["event_seq"].as_i64().is_some(), "{reply}");
    let raw: String = store
        .connection()
        .query_row(
            "SELECT payload FROM events WHERE kind='native_member_handoff' AND subject_id=?1 ORDER BY seq DESC LIMIT 1",
            [task.task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(payload["member_id"].as_str(), Some(worker.as_str()));
    assert_eq!(
        payload["claim_scope"].as_str(),
        Some("member_report_not_task_completion")
    );
    assert_ne!(
        TaskRegistry::new(&store)
            .require_task(&task.task_id)
            .unwrap()
            .status
            .as_str(),
        "done"
    );
    shutdown(home, &mut host);
}

#[test]
fn native_trace_and_snapshot_are_scrubbed_while_the_terminal_stays_raw() {
    let temp = tempfile::Builder::new().prefix("p2").tempdir().unwrap();
    let home = temp.path();
    std::fs::write(home.join("redaction.secrets"), SECRET).unwrap();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let name = member(&store, &format!("member-{SECRET}"));
    let task = TaskRegistry::new(&store)
        .create_task(format!("goal {SECRET}"), vec![], None, None, None)
        .unwrap();
    let grant = AuthorityEngine::new(&store)
        .issue_root_grant(
            Some(name.parse().unwrap()),
            Some(task.task_id.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let mut host = start(home);
    let execution = launch(
        home,
        task.task_id.as_str(),
        &name,
        grant.grant_id.as_str(),
        "redaction",
        &format!("printf '%s' '{SECRET}'; sleep 120"),
    );
    let mut stream = UnixStream::connect(home.join("runtime/workbench/herdr.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        json!({"id":SECRET,"method":"ping","params":{}})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    wait("raw terminal output", || {
        call(
            home,
            "pane.read",
            json!({"pane_id":execution["pane_id"],"source":"recent","format":"text"}),
        )["result"]["read"]["text"]
            .as_str()
            .is_some_and(|text| text.contains(SECRET))
    });
    let metadata = home.join("runtime/workbench/session.json");
    wait("derived native snapshot", || metadata.exists());
    let saved = std::fs::read_to_string(&metadata).unwrap();
    assert!(!saved.contains(SECRET), "snapshot leaked configured secret");
    assert!(saved.contains("[REDACTED]"));
    let log = home.join("runtime/workbench/herdr-server.log");
    wait("actual native trace", || {
        std::fs::read_to_string(&log).is_ok_and(|text| text.contains("api.request"))
    });
    let trace = std::fs::read_to_string(&log).unwrap();
    assert!(!trace.contains(SECRET), "trace leaked configured secret");
    assert!(trace.contains("[REDACTED]"));
    let out = Command::new(binary())
        .args(["brief", task.task_id.as_str()])
        .env_clear()
        .env("HOME", home)
        .env("VIVA_HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains(SECRET));
    let persisted:String=store.connection().query_row("SELECT brief_json FROM task_briefs WHERE task_id=?1 ORDER BY generated_at DESC LIMIT 1",[task.task_id.as_str()],|row|row.get(0)).unwrap();
    assert!(!persisted.contains(SECRET));
    // Original user goal and secret configuration remain untouched.
    assert!(
        TaskRegistry::new(&store)
            .require_task(&task.task_id)
            .unwrap()
            .goal
            .contains(SECRET)
    );
    assert_eq!(
        std::fs::read_to_string(home.join("redaction.secrets")).unwrap(),
        SECRET
    );
    shutdown(home, &mut host);
}
