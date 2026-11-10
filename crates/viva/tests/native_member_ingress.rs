//! #191 native member ingress: a host-issued MemberExecution credential bound
//! to the confirmed pane process, with canonical authority and fixed-marker
//! refusals. Runs only with the explicit herdr-runtime feature; not
//! installation, cross-backend, default-entry or human UI acceptance.
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

fn member_call(home: &Path, credential: &str, status: &str, detail: &str) -> Value {
    call(
        home,
        "viva.member.report",
        json!({"credential": credential, "status": status, "detail": detail}),
    )
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
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_execution_running(home: &Path, execution_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status: String = db(home)
            .query_row(
                "SELECT status FROM executions WHERE execution_id=?1",
                [execution_id],
                |row| row.get(0),
            )
            .unwrap();
        if status == "running" {
            return;
        }
        assert!(Instant::now() < deadline, "execution never started");
        thread::sleep(Duration::from_millis(25));
    }
}

struct FixtureData {
    member: viva::members::MemberRecord,
    task: viva::tasks::TaskRecord,
    grant: viva::authority::Grant,
}

fn seed(home: &Path, grant_actions: &[&str]) -> FixtureData {
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let members = MemberRegistry::new(&store);
    let member = members.register("native member ingress").unwrap();
    members.set_binding(&binding(&member.member_id)).unwrap();
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task(
            "native member ingress task",
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
            grant_actions.iter().map(|a| a.to_string()).collect(),
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

fn start_execution(home: &Path, data: &FixtureData, request_key: &str, command: &str) -> String {
    authenticate(home);
    let response = call(
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
    );
    assert_eq!(
        response["result"]["type"], "viva_execution_started",
        "{response}"
    );
    response["result"]["execution_id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn credential_for(home: &Path, execution_id: &str) -> String {
    db(home)
        .query_row(
            "SELECT credential_id FROM worker_credentials WHERE execution_id=?1",
            [execution_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn bound_process_report_is_canonical_attributed_and_redacted() {
    let temp = tempfile::Builder::new()
        .prefix("vn-member-report-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let data = seed(home, &["dispatch_delegated", "member_report"]);
    let _fixture = start_fixture(home);
    let bin = env!("CARGO_BIN_EXE_viva");
    let command = format!(
        "sleep 1; exec '{bin}' runtime member report --status blocked --detail \"credential=$VIVA_MEMBER_CREDENTIAL\""
    );
    let execution_id = start_execution(home, &data, "member-report", &command);
    let (status, process_exit) = wait_execution_exit(home, &execution_id);
    assert_eq!(status, "stopped", "a report-only child exits 0");
    assert_eq!(process_exit, Some(0));

    let credential = credential_for(home, &execution_id);
    let conn = db(home);
    let payload: String = conn
        .query_row(
            "SELECT payload FROM events WHERE kind='native_member_reported' AND subject_id=?1 ORDER BY seq DESC LIMIT 1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["status"], "blocked");
    assert_eq!(payload["member_id"], data.member.member_id.as_str());
    assert_eq!(payload["task_id"], data.task.task_id.as_str());
    assert_eq!(payload["grant_id"], data.grant.grant_id.as_str());
    assert_eq!(payload["source"], "native_member_process");
    let detail = payload["detail"].as_str().unwrap();
    assert!(detail.contains("[REDACTED]"), "{detail}");
    assert!(!detail.contains(&credential), "credential leaked: {detail}");
    let credential_status: String = conn
        .query_row(
            "SELECT status FROM worker_credentials WHERE credential_id=?1",
            [&credential],
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
    assert_ne!(
        task_status, "done",
        "a member report never completes a task"
    );
}

#[test]
fn external_process_with_a_valid_credential_is_refused() {
    let temp = tempfile::Builder::new()
        .prefix("vn-member-unbound-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let data = seed(home, &["dispatch_delegated", "member_report"]);
    let _fixture = start_fixture(home);
    let execution_id = start_execution(home, &data, "member-unbound", "sleep 5");
    wait_execution_running(home, &execution_id);
    let credential = credential_for(home, &execution_id);

    let refused = member_call(home, &credential, "progress", "from an unrelated process");
    assert_eq!(refused["error"]["code"], "member_refused", "{refused}");
    let conn = db(home);
    let reports: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_member_reported' AND subject_id=?1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reports, 0);
    let audit: String = conn
        .query_row(
            "SELECT payload FROM events WHERE kind='native_member_report_refused' AND subject_id=?1 ORDER BY seq DESC LIMIT 1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(audit.contains("peer_not_bound_to_execution"), "{audit}");
    assert!(!audit.contains(&credential));
}

#[test]
fn report_outside_the_grant_actions_is_refused_and_audited() {
    let temp = tempfile::Builder::new()
        .prefix("vn-member-grant-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let data = seed(home, &["dispatch_delegated"]);
    let _fixture = start_fixture(home);
    let bin = env!("CARGO_BIN_EXE_viva");
    let command = format!("sleep 1; exec '{bin}' runtime member report --status progress");
    let execution_id = start_execution(home, &data, "member-grant", &command);
    let (status, process_exit) = wait_execution_exit(home, &execution_id);
    assert_eq!(status, "failed", "the refused report client exits non-zero");
    assert_eq!(process_exit, Some(1));

    let conn = db(home);
    let reports: i64 = conn
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_member_reported' AND subject_id=?1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reports, 0);
    let audit: String = conn
        .query_row(
            "SELECT payload FROM events WHERE kind='native_member_report_refused' AND subject_id=?1 ORDER BY seq DESC LIMIT 1",
            [&execution_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(audit.contains("authority_refused"), "{audit}");
    assert!(!audit.contains("dispatch_delegated"), "{audit}");
}

#[test]
fn a_revoked_credential_cannot_report_after_exit() {
    let temp = tempfile::Builder::new()
        .prefix("vn-member-revoked-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let data = seed(home, &["dispatch_delegated", "member_report"]);
    let _fixture = start_fixture(home);
    let execution_id = start_execution(home, &data, "member-revoked", "exit 0");
    wait_execution_exit(home, &execution_id);
    let credential = credential_for(home, &execution_id);
    let refused = member_call(home, &credential, "progress", "after exit");
    assert_eq!(refused["error"]["code"], "member_refused", "{refused}");
    let audits: i64 = db(home)
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_member_report_refused' AND payload LIKE '%unknown_credential%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(audits >= 1);
}

#[test]
fn foreign_active_credentials_still_block_the_owner_gate() {
    let temp = tempfile::Builder::new()
        .prefix("vn-member-foreign-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let members = MemberRegistry::new(&store);
    let member = members.register("foreign credential member").unwrap();
    AuthorityEngine::new(&store)
        .issue_worker_credential(
            &viva::foundation::ids::ChannelId::new(),
            Some(ExecutionId::new()),
            member.member_id.clone(),
        )
        .unwrap();
    // The gate is exercised through the real host: it must refuse to start.
    let runtime = home.join("runtime/workbench");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(
        runtime.join("config.toml"),
        "[terminal]\ndefault_shell = '/bin/sh'\nshell_mode = 'non_login'\n[update]\nversion_check = false\nmanifest_check = false\n[experimental]\npane_history = false\n",
    )
    .unwrap();
    let log = std::fs::File::create(home.join("native.log")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_viva"))
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
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let exited = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "host did not fail closed");
        thread::sleep(Duration::from_millis(25));
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        !exited.success(),
        "foreign active credentials must refuse the host"
    );
    let log = std::fs::read_to_string(home.join("native.log")).unwrap_or_default();
    assert!(
        log.contains("worker credentials outside this host"),
        "{log}"
    );
}
