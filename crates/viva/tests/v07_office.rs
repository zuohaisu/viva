//! V07 acceptance tests (issue #16): the active Office control plane over a
//! real Unix-socket channel — CLI dispatch/observe/stop, single active host
//! per VIVA_HOME, honest rejection without a daemon, graceful shutdown, and
//! crash-restart reconciliation that never re-runs finished work.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use viva::authority::{AuthorityEngine, GrantMode};
use viva::foundation::ids::MemberId;
use viva::members::{MemberBinding, MemberRegistry};
use viva::office::{self, OfficeHost, OfficeRequestKind};
use viva::tasks::TaskRegistry;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn wait_until(what: &str, timeout: Duration, mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

fn socket_ready(home: &std::path::Path) -> bool {
    // A real connect, not a file check: the file can exist while the host
    // is still claiming (or after it died), and only a live listener makes
    // the channel usable.
    std::os::unix::net::UnixStream::connect(home.join(office::OFFICE_SOCKET_NAME)).is_ok()
}

fn send(home: &std::path::Path, kind: OfficeRequestKind) -> Result<Value, String> {
    let response =
        office::send_request(home, office::new_request(kind)).map_err(|e| e.to_string())?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(response.error.unwrap_or_else(|| "rejected".into()))
    }
}

/// Seed one member (with binding), one task and one live dispatch grant.
struct Seed {
    member_id: MemberId,
    task_id: String,
    grant_id: String,
}

fn seed(home: &std::path::Path, task_goal: &str) -> Seed {
    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(home),
        office::office_migrations(),
    )
    .expect("seed store");
    let members = MemberRegistry::new(&store);
    let member = members.register("Samuel").expect("member");
    members
        .set_binding(&MemberBinding {
            member_id: member.member_id.clone(),
            role: "developer".into(),
            model_binding: "glm-5.3-flash".into(),
            tools: vec!["pi-cli".into()],
            updated_at: viva::foundation::ids::utc_now(),
        })
        .expect("binding");
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task(
            task_goal,
            vec![],
            Some(member.member_id.clone()),
            None,
            None,
        )
        .expect("task");
    let authority = AuthorityEngine::new(&store);
    let grant = authority
        .issue_root_grant(
            Some(member.member_id.clone()),
            Some(task.task_id.clone()),
            vec!["dispatch_delegated".into(), "stop_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .expect("grant");
    Seed {
        member_id: member.member_id,
        task_id: task.task_id.to_string(),
        grant_id: grant.grant_id.to_string(),
    }
}

fn dispatch_request(seed: &Seed, request_key: &str, argv: Vec<String>) -> OfficeRequestKind {
    OfficeRequestKind::Dispatch {
        task_id: seed.task_id.clone(),
        member_id: seed.member_id.to_string(),
        grant_id: seed.grant_id.clone(),
        request_key: request_key.into(),
        argv,
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        worktree_id: None,
    }
}

fn spawn_host_thread(home: std::path::PathBuf) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let host = OfficeHost::open(&home).expect("host opens");
        host.serve().expect("serve");
    })
}

fn count_executions(home: &std::path::Path) -> i64 {
    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(home),
        office::office_migrations(),
    )
    .expect("count store");
    store.row_count("office_executions").expect("count")
}

// ---------------------------------------------------------------------------
// The real control-channel loop
// ---------------------------------------------------------------------------

#[test]
fn cli_dispatches_observes_and_stops_over_the_real_channel() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().to_path_buf();
    let seed_a = seed(&home, "write the V07 loop");
    let seed_b = seed(&home, "write the V07 probe");

    let host = spawn_host_thread(home.clone());
    wait_until("socket", Duration::from_secs(10), || socket_ready(&home));

    // Dispatch two real terminals for two tasks of the same member.
    let first = send(
        &home,
        dispatch_request(
            &seed_a,
            "req-key-a",
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo v07-ready-a; sleep 30".into(),
            ],
        ),
    )
    .expect("dispatch a");
    assert_eq!(first["replayed"], false, "first dispatch is not a replay");
    let terminal_a = first["terminal_id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    let execution_a = first["execution_id"]
        .as_str()
        .expect("execution")
        .to_string();

    let second = send(
        &home,
        dispatch_request(
            &seed_b,
            "req-key-b",
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo v07-ready-b; sleep 30".into(),
            ],
        ),
    )
    .expect("dispatch b");
    let terminal_b = second["terminal_id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    assert_ne!(terminal_a, terminal_b, "two dispatches, two terminals");

    // Observe: the snapshot comes from the real PTY of terminal A.
    wait_until("terminal a output", Duration::from_secs(10), || {
        matches!(
            send(&home, OfficeRequestKind::TerminalSnapshot { terminal_id: terminal_a.clone() })
                .ok()
                .and_then(|s| s["visible"].as_array().cloned()),
            Some(rows) if rows.iter().any(|r| r.as_str().unwrap_or("").contains("v07-ready-a"))
        )
    });

    // A retry with the same request key replays: one execution, not two.
    let replay = send(
        &home,
        dispatch_request(
            &seed_a,
            "req-key-a",
            vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo v07-ready-a; sleep 30".into(),
            ],
        ),
    )
    .expect("replayed dispatch");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["execution_id"], Value::String(execution_a.clone()));

    // Stop terminal A; terminal B keeps running (neighbor isolation).
    send(
        &home,
        OfficeRequestKind::TerminalStop {
            terminal_id: terminal_a.clone(),
        },
    )
    .expect("stop a");
    wait_until("execution a exited", Duration::from_secs(10), || {
        // The exit watcher records through the launch-intent protocol.
        let store = viva::foundation::store::Store::open(
            &viva::foundation::paths::database_path(&home),
            office::office_migrations(),
        )
        .expect("store");
        let state: String = store
            .connection()
            .query_row(
                "SELECT state FROM launch_intents WHERE request_key = 'req-key-a'",
                [],
                |row| row.get(0),
            )
            .unwrap_or_default();
        state == "exited"
    });
    let b_alive = send(
        &home,
        OfficeRequestKind::TerminalSnapshot {
            terminal_id: terminal_b.clone(),
        },
    )
    .is_ok();
    assert!(b_alive, "stopping terminal A must not touch terminal B");

    // Status is visible over the channel, with honest ownership labels.
    let status = send(&home, OfficeRequestKind::Status).expect("status");
    let terminals = status["terminals"].as_array().expect("terminals");
    assert_eq!(terminals.len(), 2);
    assert!(
        terminals
            .iter()
            .any(|t| t["terminal_id"] == Value::String(terminal_b.clone()))
    );
    assert!(
        terminals.iter().all(|t| t["owner"]
            .as_str()
            .unwrap_or("")
            .starts_with("member_execution:")),
        "ownership labels match the executions: {terminals:?}"
    );

    // Results recorded for the task include the process-exit fact.
    wait_until("task results", Duration::from_secs(10), || {
        send(
            &home,
            OfficeRequestKind::TaskResults {
                task_id: seed_a.task_id.clone(),
            },
        )
        .map(|r| r.as_array().map(|a| !a.is_empty()).unwrap_or(false))
        .unwrap_or(false)
    });

    office::send_request(&home, office::new_request(OfficeRequestKind::Shutdown))
        .expect("shutdown");
    host.join().expect("host thread ends after shutdown");
}

#[test]
fn revoked_grant_denies_dispatch_and_creates_no_execution() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().to_path_buf();
    let seed = seed(&home, "revoked grant task");

    // Revoke the grant before any dispatch.
    {
        let store = viva::foundation::store::Store::open(
            &viva::foundation::paths::database_path(&home),
            office::office_migrations(),
        )
        .expect("store");
        let authority = AuthorityEngine::new(&store);
        authority
            .revoke(
                &viva::foundation::ids::GrantId::from_str(&seed.grant_id).expect("grant id"),
                "authority withdrawn before launch",
            )
            .expect("revoke");
    }

    let host = spawn_host_thread(home.clone());
    wait_until("socket", Duration::from_secs(10), || socket_ready(&home));

    let err = send(
        &home,
        dispatch_request(&seed, "req-revoked", vec!["/bin/sleep".into(), "30".into()]),
    )
    .expect_err("revoked grant must be denied");
    assert!(err.contains("denied"), "got: {err}");
    assert_eq!(count_executions(&home), 0, "no execution may be created");

    office::send_request(&home, office::new_request(OfficeRequestKind::Shutdown)).ok();
    host.join().ok();
}

#[test]
fn mutation_without_active_office_is_rejected_and_starts_no_daemon() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().to_path_buf();
    let seed = seed(&home, "nobody home");

    let err = send(
        &home,
        dispatch_request(&seed, "req-no-host", vec!["/bin/sleep".into(), "1".into()]),
    )
    .expect_err("no host listening");
    assert!(err.contains("no active office"), "got: {err}");
    assert!(
        !home.join(office::OFFICE_SOCKET_NAME).exists(),
        "the client must not have conjured a socket/daemon into existence"
    );
}

#[test]
fn second_host_refuses_and_names_the_running_one() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().to_path_buf();
    seed(&home, "single host");

    let host = spawn_host_thread(home.clone());
    wait_until("socket", Duration::from_secs(10), || socket_ready(&home));

    let status = send(&home, OfficeRequestKind::Status).expect("status");
    let pid = status["pid"].as_u64().expect("host pid");

    let err = match OfficeHost::open(&home) {
        Err(err) => err,
        Ok(_second) => panic!("second host must refuse to start"),
    };
    let message = err.to_string();
    assert!(message.contains("already active"), "got: {message}");
    assert!(
        message.contains(&pid.to_string()),
        "the refusal should name the running host's pid {pid}: {message}"
    );

    office::send_request(&home, office::new_request(OfficeRequestKind::Shutdown)).ok();
    host.join().ok();
}

#[test]
fn graceful_shutdown_stops_owned_terminals_and_persists_the_handoff() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().to_path_buf();
    let seed = seed(&home, "shutdown with a live terminal");

    let host = spawn_host_thread(home.clone());
    wait_until("socket", Duration::from_secs(10), || socket_ready(&home));

    send(
        &home,
        dispatch_request(
            &seed,
            "req-shutdown",
            vec!["/bin/sleep".into(), "30".into()],
        ),
    )
    .expect("dispatch");

    office::send_request(&home, office::new_request(OfficeRequestKind::Shutdown))
        .expect("shutdown accepted");
    host.join().expect("serve ends");
    assert!(
        !home.join(office::OFFICE_SOCKET_NAME).exists(),
        "the channel is released on exit"
    );

    // The owned terminal was stopped and its exit recorded through the
    // launch-intent protocol (no orphan process left behind).
    wait_until(
        "intent exited after shutdown",
        Duration::from_secs(15),
        || {
            let store = viva::foundation::store::Store::open(
                &viva::foundation::paths::database_path(&home),
                office::office_migrations(),
            )
            .expect("store");
            let state: String = store
                .connection()
                .query_row(
                    "SELECT state FROM launch_intents WHERE request_key = 'req-shutdown'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or_default();
            state == "exited"
        },
    );

    let offline = office::offline_status(&home).expect("offline status");
    assert_eq!(
        offline["last_host"]["exit_kind"],
        Value::String("graceful".into()),
        "the host recorded its own clean exit: {offline}"
    );
    assert!(
        offline["recovery_events"].as_i64().unwrap_or(0) >= 1,
        "the handoff is persisted as a recovery record"
    );
}

/// The real crash path: kill -9 the host binary mid-dispatch, then reopen.
/// Reconciliation marks what the dead host left behind, finishes nothing,
/// re-runs nothing and kills nothing.
#[test]
fn crash_restart_reconciles_without_rerunning_finished_work() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().to_path_buf();
    let running_seed = seed(&home, "orphaned by the crash");
    let done_seed = seed(&home, "completed before the crash");

    // A task that completed before the crash: completion evidence recorded.
    {
        let store = viva::foundation::store::Store::open(
            &viva::foundation::paths::database_path(&home),
            office::office_migrations(),
        )
        .expect("store");
        let tasks = TaskRegistry::new(&store);
        tasks
            .complete_task(
                &viva::foundation::ids::TaskId::from_str(&done_seed.task_id).expect("task"),
                "pr #14 merged by the owner (pre-crash evidence)",
                "owner",
            )
            .expect("complete");
    }

    // Real host binary, really killed.
    let mut child = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(["office", "start"])
        .env("VIVA_HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("host binary");
    wait_until("host binary socket", Duration::from_secs(15), || {
        socket_ready(&home)
    });

    let live = send(
        &home,
        dispatch_request(
            &running_seed,
            "req-orphan",
            vec!["/bin/sleep".into(), "2".into()],
        ),
    )
    .expect("dispatch before the crash");
    assert_eq!(live["replayed"], false);

    let before = count_executions(&home);
    child.kill().expect("kill -9 the host");
    let _ = child.wait();
    // Give the OS a moment; the socket file survives the kill (stale).
    std::thread::sleep(Duration::from_millis(100));

    // Reopen: the stale socket is claimed and reconciliation runs.
    let host = spawn_host_thread(home.clone());
    wait_until("restarted socket", Duration::from_secs(15), || {
        socket_ready(&home)
    });
    let _ = send(&home, OfficeRequestKind::Status).expect("restarted host answers");

    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(&home),
        office::office_migrations(),
    )
    .expect("store");
    let kinds: Vec<String> = {
        let mut stmt = store
            .connection()
            .prepare("SELECT kind FROM office_recovery_events ORDER BY seq")
            .expect("prepare");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query");
        rows.collect::<Result<Vec<_>, _>>().expect("collect")
    };
    assert!(
        kinds.iter().any(|k| k == "previous_host_crashed"),
        "the crashed host is recorded: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "execution_orphaned"),
        "the orphaned execution is recorded: {kinds:?}"
    );

    // The orphaned execution is honestly `stopped`, never `running` forever,
    // never `completed`, and nothing was re-run: execution count unchanged.
    wait_until("orphan marked stopped", Duration::from_secs(10), || {
        let status: String = store
            .connection()
            .query_row(
                "SELECT status FROM office_executions WHERE execution_id = ?1",
                [live["execution_id"].as_str().unwrap_or("")],
                |row| row.get(0),
            )
            .unwrap_or_default();
        status == "stopped"
    });
    assert_eq!(
        count_executions(&home),
        before,
        "reconciliation must not re-run or duplicate anything"
    );

    // The completed task refuses dispatch: finished work is never executed.
    let err = send(
        &home,
        dispatch_request(
            &done_seed,
            "req-after-done",
            vec!["/bin/sleep".into(), "1".into()],
        ),
    )
    .expect_err("completed task must refuse");
    assert!(err.contains("never executed again"), "got: {err}");
    assert_eq!(
        count_executions(&home),
        before,
        "the refusal created no execution"
    );

    office::send_request(&home, office::new_request(OfficeRequestKind::Shutdown)).ok();
    host.join().ok();
}

use std::str::FromStr as _;
