//! #191 native owner control: pause/resume the persisted dispatch gate and
//! stop one canonical execution through its real pane. Runs only with the
//! explicit herdr-runtime feature; not installation/UI acceptance.
#![cfg(all(feature = "herdr-runtime", unix))]
#[path = "support/owner_anchor.rs"]
mod owner_support;
use owner_support::{OwnerHost, authenticate, call, spawn, wait_owner_ready, wait_ready};
use serde_json::{Value, json};
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};
use viva::{
    authority::{AuthorityEngine, GrantMode},
    foundation::{ids::utc_now, paths, store::Store},
    members::{MemberBinding, MemberRegistry},
    tasks::TaskRegistry,
};

fn db(home: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(paths::database_path(home)).unwrap()
}

fn binding(member: &viva::foundation::ids::MemberId) -> MemberBinding {
    MemberBinding {
        member_id: member.clone(),
        role: "native control fixture".into(),
        model_binding: "fixture:model".into(),
        tools: vec!["fixture-tool".into()],
        updated_at: utc_now(),
    }
}

fn start_fixture(home: &Path, tag: &str) -> OwnerHost {
    let host = spawn(home, tag);
    wait_owner_ready(home, &host);
    host
}

struct FixtureData {
    member: viva::members::MemberRecord,
    task: viva::tasks::TaskRecord,
    grant: viva::authority::Grant,
}

fn seed(home: &Path) -> FixtureData {
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let members = MemberRegistry::new(&store);
    let member = members.register("native control member").unwrap();
    members.set_binding(&binding(&member.member_id)).unwrap();
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task(
            "native control task",
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
    FixtureData {
        member,
        task,
        grant,
    }
}

fn try_start(home: &Path, data: &FixtureData, request_key: &str, command: &str) -> Value {
    call(
        home,
        "viva.execution.start",
        json!({
            "task_id": data.task.task_id.as_str(),
            "member_id": data.member.member_id.as_str(),
            "grant_id": data.grant.grant_id.as_str(),
            "request_key": request_key,
            "argv": ["/bin/sh", "-c", command],
            "cwd": "/tmp",
        }),
    )
}

fn wait_execution_exit(home: &Path, execution_id: &str) -> (String, Option<i64>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row: (String, Option<i64>) = db(home)
            .query_row(
                "SELECT status, process_exit FROM executions WHERE execution_id=?1",
                [execution_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        if row.1.is_some() {
            return row;
        }
        assert!(Instant::now() < deadline, "execution never exited");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn pause_blocks_new_dispatch_and_resume_reopens_it() {
    let temp = tempfile::Builder::new()
        .prefix("vn-control-pause-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let data = seed(home);
    let fixture = start_fixture(home, "control-pause");
    authenticate(home, fixture.anchor());

    let paused = call(home, "viva.host.pause", json!({"paused": true}));
    assert_eq!(paused["result"]["type"], "viva_host_paused", "{paused}");
    let refused = try_start(home, &data, "paused-key", "exit 0");
    assert_eq!(refused["error"]["code"], "execution_refused", "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("paused")
    );
    let count: i64 = db(home)
        .query_row("SELECT count(*) FROM executions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);

    let resumed = call(home, "viva.host.pause", json!({"paused": false}));
    assert_eq!(resumed["result"]["type"], "viva_host_paused", "{resumed}");
    let started = try_start(home, &data, "resumed-key", "sleep 0.3; exit 0");
    assert_eq!(
        started["result"]["type"], "viva_execution_started",
        "{started}"
    );
    let execution_id = started["result"]["execution_id"].as_str().unwrap();
    let (status, code) = wait_execution_exit(home, execution_id);
    assert_eq!((status.as_str(), code), ("stopped", Some(0)));
}

#[test]
fn stop_closes_the_pane_and_records_a_deliberate_exit() {
    let temp = tempfile::Builder::new()
        .prefix("vn-control-stop-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let data = seed(home);
    let fixture = start_fixture(home, "control-stop");
    authenticate(home, fixture.anchor());
    let started = try_start(home, &data, "stop-key", "sleep 30");
    assert_eq!(
        started["result"]["type"], "viva_execution_started",
        "{started}"
    );
    let execution_id = started["result"]["execution_id"]
        .as_str()
        .unwrap()
        .to_string();
    let stopped = call(
        home,
        "viva.execution.stop",
        json!({"execution_id": execution_id}),
    );
    assert_eq!(
        stopped["result"]["type"], "viva_execution_stopped",
        "{stopped}"
    );
    let (status, code) = wait_execution_exit(home, &execution_id);
    assert_eq!(status, "stopped");
    assert_eq!(code, Some(-1), "a deliberate close has no exit code");

    let conn = db(home);
    let requested: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_execution_stop_requested' AND subject_id=?1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(requested, 1);
    let exited: String = conn
        .query_row(
            "SELECT payload FROM events WHERE kind='native_execution_exited' AND subject_id=?1 ORDER BY seq DESC LIMIT 1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    let exited: Value = serde_json::from_str(&exited).unwrap();
    assert_eq!(exited["stop_requested"], true);
    assert_eq!(exited["exit_code_known"], false);
    let credential_status: String = conn
        .query_row(
            "SELECT status FROM worker_credentials WHERE execution_id=?1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(credential_status, "revoked");
    let task_status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE task_id=?1",
            [data.task.task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(task_status, "done");
}

#[test]
fn stop_without_owner_capability_is_refused_before_any_state_change() {
    let temp = tempfile::Builder::new()
        .prefix("vn-control-unbound-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let _ = seed(home);
    // Deliberately unbound: never authenticate this process.
    let fixture = spawn(home, "control-unbound");
    wait_ready(home, &fixture);
    let refused = call(
        home,
        "viva.execution.stop",
        json!({"execution_id": "exec-does-not-exist"}),
    );
    assert_eq!(
        refused["error"]["code"], "authority_not_integrated",
        "{refused}"
    );
    let audit: i64 = db(home)
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_automation_refused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(audit >= 1);
}

#[test]
fn stopping_an_unknown_execution_is_refused() {
    let temp = tempfile::Builder::new()
        .prefix("vn-control-unknown-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let _ = seed(home);
    let fixture = start_fixture(home, "control-unknown");
    authenticate(home, fixture.anchor());
    let refused = call(
        home,
        "viva.execution.stop",
        json!({"execution_id": "exec-does-not-exist"}),
    );
    assert_eq!(
        refused["error"]["code"], "execution_stop_refused",
        "{refused}"
    );
}
