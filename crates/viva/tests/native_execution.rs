//! #191 native execution lifecycle: canonical grant/task/member checks,
//! a real pane-bound process, and honest exit facts. Runs only with the
//! explicit herdr-runtime feature and never claims installation/UI acceptance.
#![cfg(all(feature = "herdr-runtime", unix))]
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    str::FromStr,
    thread,
    time::{Duration, Instant},
};
use viva::{
    authority::{AuthorityEngine, GrantMode},
    foundation::{
        ids::{ExecutionId, utc_now},
        paths,
        store::Store,
    },
    members::{MemberBinding, MemberRegistry},
    tasks::TaskRegistry,
};

struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        // Exact task-owned PID. Forced fixture cleanup is not lifecycle PASS.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn db(home: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(paths::database_path(home)).unwrap()
}

fn call(home: &Path, method: &str, params: Value) -> Value {
    let mut socket = UnixStream::connect(home.join("runtime/workbench/herdr.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    writeln!(
        socket,
        "{}",
        json!({"id":"fixture-secret-do-not-persist", "method":method, "params":params,
            "transport_peer":{"pid":1,"uid":0}, "peer_pid":1, "peer_uid":0,
            "claimed_role":"member_execution:forged", "member_id":"forged", "grant_id":"forged"})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn authenticate(home: &Path) {
    let capability: Value = serde_json::from_slice(
        &std::fs::read(home.join("runtime/workbench/owner-channel.json")).unwrap(),
    )
    .unwrap();
    let peer = herdr::embedding::TransportPeer {
        pid: Some(std::process::id()),
        uid: Some(unsafe { libc::geteuid() }),
    };
    let lifetime = herdr::embedding::peer_lifetime_marker(peer).unwrap();
    let response = call(
        home,
        "viva.client.authenticate",
        json!({"credential": capability["credential"], "process_lifetime": lifetime}),
    );
    assert_eq!(response["result"]["type"], "ok", "{response}");
}

fn binding(member: &viva::foundation::ids::MemberId) -> MemberBinding {
    MemberBinding {
        member_id: member.clone(),
        role: "native fixture role".into(),
        model_binding: "fixture:model".into(),
        tools: vec!["fixture-tool".into()],
        updated_at: utc_now(),
    }
}

fn start_fixture(home: &Path) -> Fixture {
    let runtime = home.join("runtime/workbench");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(
        runtime.join("config.toml"),
        "[terminal]\ndefault_shell = '/bin/sh'\nshell_mode = 'non_login'\n[update]\nversion_check = false\nmanifest_check = false\n[experimental]\npane_history = false\n",
    )
    .unwrap();
    let log = std::fs::File::create(home.join("native.log")).unwrap();
    let mut fixture = Fixture(
        Command::new(env!("CARGO_BIN_EXE_viva"))
            .args(["runtime", "server"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOME", home)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("VIVA_HOME", home)
            .env("XDG_CONFIG_HOME", home.join("xdg-config"))
            .env("XDG_STATE_HOME", home.join("xdg-state"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while !runtime.join("herdr.sock").exists() || !runtime.join("owner-channel.json").exists() {
        assert!(
            fixture.0.try_wait().unwrap().is_none(),
            "native host exited: {}",
            std::fs::read_to_string(home.join("native.log")).unwrap_or_default()
        );
        assert!(Instant::now() < deadline, "native startup deadline");
        thread::sleep(Duration::from_millis(25));
    }
    fixture
}

fn wait_execution_exit(home: &Path, execution_id: &str) -> (String, Option<i64>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let conn = db(home);
        let row: (String, Option<i64>) = conn
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
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn unbound_execution_start_is_refused_before_preparation() {
    let temp = tempfile::Builder::new()
        .prefix("vn-exec-refuse-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let (member, task, grant) = {
        let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
        let members = MemberRegistry::new(&store);
        let member = members.register("native exec member").unwrap();
        members.set_binding(&binding(&member.member_id)).unwrap();
        let tasks = TaskRegistry::new(&store);
        let task = tasks
            .create_task(
                "native exec task",
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
        (member, task, grant)
    };
    let _fixture = start_fixture(home);

    let refused = call(
        home,
        "viva.execution.start",
        json!({
            "task_id": task.task_id.as_str(),
            "member_id": member.member_id.as_str(),
            "grant_id": grant.grant_id.as_str(),
            "request_key": "unbound-key",
            "argv": ["/bin/sh", "-c", "exit 0"],
            "cwd": "/tmp",
        }),
    );
    assert_eq!(
        refused["error"]["code"], "authority_not_integrated",
        "{refused}"
    );
    let conn = db(home);
    let count: i64 = conn
        .query_row("SELECT count(*) FROM executions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let audit: String = conn
        .query_row(
            "SELECT group_concat(payload) FROM events WHERE kind='native_automation_refused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(audit.contains("native.raw_client"), "{audit}");
    assert!(!audit.contains("unbound-key"));
    assert!(!audit.contains("/bin/sh"));
}

#[test]
fn owner_dispatch_spawns_a_canonical_execution_and_records_the_real_exit() {
    let temp = tempfile::Builder::new()
        .prefix("vn-exec-live-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let (member, task, grant) = {
        let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
        let members = MemberRegistry::new(&store);
        let member = members.register("native exec member").unwrap();
        members.set_binding(&binding(&member.member_id)).unwrap();
        let tasks = TaskRegistry::new(&store);
        let task = tasks
            .create_task(
                "native exec task",
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
        (member, task, grant)
    };
    let _fixture = start_fixture(home);
    authenticate(home);

    let response = call(
        home,
        "viva.execution.start",
        json!({
            "task_id": task.task_id.as_str(),
            "member_id": member.member_id.as_str(),
            "grant_id": grant.grant_id.as_str(),
            "request_key": "live-key",
            "argv": ["/bin/sh", "-c", "printf 'native-exec-ok'; sleep 1; exit 7"],
            "cwd": "/tmp",
        }),
    );
    assert_eq!(
        response["result"]["type"], "viva_execution_started",
        "{response}"
    );
    assert_eq!(response["result"]["replayed"], false, "{response}");
    let execution_id = response["result"]["execution_id"]
        .as_str()
        .unwrap()
        .to_string();
    ExecutionId::from_str(&execution_id).unwrap();
    let pid = response["result"]["pid"].as_u64().unwrap() as i64;
    assert!(pid > 1);
    assert!(response["result"]["workspace_id"].is_string());
    assert!(response["result"]["pane_id"].is_string());

    let conn = db(home);
    let (status, process_exit, member_id): (String, Option<i64>, String) = conn
        .query_row(
            "SELECT status, process_exit, member_id FROM executions WHERE execution_id=?1",
            [&execution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "running");
    assert!(process_exit.is_none());
    assert_eq!(member_id, member.member_id.as_str());
    let (intent_pid, marker): (i64, String) = conn
        .query_row(
            "SELECT pid, pid_start_marker FROM launch_intents WHERE request_key='live-key'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(intent_pid, pid);
    assert!(!marker.trim().is_empty());
    let spec: (String, String, String) = conn
        .query_row(
            "SELECT execution_id, member_id, terminal_id FROM launch_specs WHERE execution_id=?1",
            [&execution_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(spec.0, execution_id);
    assert_eq!(spec.1, member.member_id.as_str());
    assert!(!spec.2.is_empty());
    let task_status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE task_id=?1",
            [task.task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(task_status, "done");

    let (status, process_exit) = wait_execution_exit(home, &execution_id);
    assert_eq!(process_exit, Some(7));
    assert_eq!(status, "failed");
    let conn = db(home);
    let task_status: String = conn
        .query_row(
            "SELECT status FROM tasks WHERE task_id=?1",
            [task.task_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(task_status, "done", "a process exit never completes a task");
    let result_payload: String = conn
        .query_row(
            "SELECT payload FROM task_results WHERE dedup_key='live-key:exit'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let result: Value = serde_json::from_str(&result_payload).unwrap();
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["exit_code_known"], true);
    let started: String = conn
        .query_row(
            "SELECT payload FROM events WHERE kind='native_execution_started' AND subject_id=?1 ORDER BY seq DESC LIMIT 1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    let started: Value = serde_json::from_str(&started).unwrap();
    assert_eq!(started["pid"], pid);
    assert_eq!(started["task_id"], task.task_id.as_str());
    assert_eq!(started["member_id"], member.member_id.as_str());
    let exited_event: String = conn
        .query_row(
            "SELECT payload FROM events WHERE kind='native_execution_exited' AND subject_id=?1 ORDER BY seq DESC LIMIT 1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    let exited_event: Value = serde_json::from_str(&exited_event).unwrap();
    assert_eq!(exited_event["exit_code"], 7);
    assert_eq!(exited_event["exit_code_known"], true);
}

#[test]
fn wrong_member_grant_and_replay_do_not_spawn_a_second_process() {
    let temp = tempfile::Builder::new()
        .prefix("vn-exec-authority-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let (member, other, task, grant) = {
        let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
        let members = MemberRegistry::new(&store);
        let member = members.register("native exec owner").unwrap();
        let other = members.register("native exec other").unwrap();
        members.set_binding(&binding(&member.member_id)).unwrap();
        members.set_binding(&binding(&other.member_id)).unwrap();
        let tasks = TaskRegistry::new(&store);
        let task = tasks
            .create_task(
                "native authority task",
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
        (member, other, task, grant)
    };
    let _fixture = start_fixture(home);
    authenticate(home);

    let wrong = call(
        home,
        "viva.execution.start",
        json!({
            "task_id": task.task_id.as_str(),
            "member_id": other.member_id.as_str(),
            "grant_id": grant.grant_id.as_str(),
            "request_key": "wrong-member-key",
            "argv": ["/bin/sh", "-c", "exit 0"],
            "cwd": "/tmp",
        }),
    );
    assert_eq!(wrong["error"]["code"], "execution_refused", "{wrong}");
    let conn = db(home);
    let executions: i64 = conn
        .query_row("SELECT count(*) FROM executions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(executions, 0);
    let refused: String = conn
        .query_row(
            "SELECT payload FROM events WHERE kind='native_execution_refused' ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let refused: Value = serde_json::from_str(&refused).unwrap();
    assert_eq!(refused["reason"], "canonical_preparation_refused");
    assert!(!refused.to_string().contains("exit 0"));

    let first = call(
        home,
        "viva.execution.start",
        json!({
            "task_id": task.task_id.as_str(),
            "member_id": member.member_id.as_str(),
            "grant_id": grant.grant_id.as_str(),
            "request_key": "replay-key",
            "argv": ["/bin/sh", "-c", "sleep 1; exit 0"],
            "cwd": "/tmp",
        }),
    );
    assert_eq!(first["result"]["replayed"], false, "{first}");
    let replay = call(
        home,
        "viva.execution.start",
        json!({
            "task_id": task.task_id.as_str(),
            "member_id": member.member_id.as_str(),
            "grant_id": grant.grant_id.as_str(),
            "request_key": "replay-key",
            "argv": ["/bin/sh", "-c", "exit 9"],
            "cwd": "/tmp",
        }),
    );
    assert_eq!(replay["result"]["replayed"], true, "{replay}");
    assert_eq!(
        replay["result"]["execution_id"],
        first["result"]["execution_id"]
    );
    assert_eq!(replay["result"]["pid"], first["result"]["pid"]);
    let count: i64 = db(home)
        .query_row("SELECT count(*) FROM executions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "replay must not spawn a second execution");
}

#[test]
fn two_members_and_two_tasks_get_distinct_pane_bound_executions() {
    let temp = tempfile::Builder::new()
        .prefix("vn-exec-two-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let (first, second) = {
        let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
        let members = MemberRegistry::new(&store);
        let tasks = TaskRegistry::new(&store);
        let authority = AuthorityEngine::new(&store);
        let mut fixtures = Vec::new();
        for (name, goal) in [
            ("native exec alpha", "alpha task"),
            ("native exec beta", "beta task"),
        ] {
            let member = members.register(name).unwrap();
            members.set_binding(&binding(&member.member_id)).unwrap();
            let task = tasks
                .create_task(goal, vec![], Some(member.member_id.clone()), None, None)
                .unwrap();
            let grant = authority
                .issue_root_grant(
                    Some(member.member_id.clone()),
                    Some(task.task_id.clone()),
                    vec!["dispatch_delegated".into()],
                    GrantMode::ActAutonomously,
                    None,
                )
                .unwrap();
            fixtures.push((member, task, grant));
        }
        let second = fixtures.pop().unwrap();
        let first = fixtures.pop().unwrap();
        (first, second)
    };
    let _fixture = start_fixture(home);
    authenticate(home);

    let dispatch = |member: &viva::members::MemberRecord,
                    task: &viva::tasks::TaskRecord,
                    grant: &viva::authority::Grant,
                    key: &str,
                    argv: Value| {
        call(
            home,
            "viva.execution.start",
            json!({
                "task_id": task.task_id.as_str(),
                "member_id": member.member_id.as_str(),
                "grant_id": grant.grant_id.as_str(),
                "request_key": key,
                "argv": argv,
                "cwd": "/tmp",
            }),
        )
    };
    let alpha = dispatch(
        &first.0,
        &first.1,
        &first.2,
        "two-alpha",
        json!(["/bin/sh", "-c", "sleep 1; exit 0"]),
    );
    let beta = dispatch(
        &second.0,
        &second.1,
        &second.2,
        "two-beta",
        json!(["/bin/sh", "-c", "sleep 1; exit 3"]),
    );
    assert_eq!(alpha["result"]["replayed"], false, "{alpha}");
    assert_eq!(beta["result"]["replayed"], false, "{beta}");
    let alpha_id = alpha["result"]["execution_id"]
        .as_str()
        .unwrap()
        .to_string();
    let beta_id = beta["result"]["execution_id"].as_str().unwrap().to_string();
    assert_ne!(alpha_id, beta_id);
    assert_ne!(alpha["result"]["pid"], beta["result"]["pid"]);
    assert_ne!(alpha["result"]["pane_id"], beta["result"]["pane_id"]);

    let conn = db(home);
    let mut stmt = conn
        .prepare("SELECT execution_id, member_id, status FROM executions ORDER BY execution_id")
        .unwrap();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (execution, member, status) in &rows {
        assert_eq!(status, "running");
        if execution == &alpha_id {
            assert_eq!(member, first.0.member_id.as_str());
        } else if execution == &beta_id {
            assert_eq!(member, second.0.member_id.as_str());
        } else {
            panic!("unexpected execution {execution}");
        }
    }

    let (alpha_status, alpha_exit) = wait_execution_exit(home, &alpha_id);
    let (beta_status, beta_exit) = wait_execution_exit(home, &beta_id);
    assert_eq!((alpha_status.as_str(), alpha_exit), ("stopped", Some(0)));
    assert_eq!((beta_status.as_str(), beta_exit), ("failed", Some(3)));
    let conn = db(home);
    for task in [&first.1.task_id, &second.1.task_id] {
        let status: String = conn
            .query_row(
                "SELECT status FROM tasks WHERE task_id=?1",
                [task.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_ne!(status, "done", "process exits never complete tasks");
    }
}
