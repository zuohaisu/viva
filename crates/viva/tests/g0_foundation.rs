//! G0 cross-interface acceptance tests (issue #10).
//!
//! These tests cross the foundation interfaces: sessions → executions with
//! attribution → launch specs → terminal events → control envelope → office
//! events, all persisted in SQLite and reloaded after a full store reopen
//! ("restart"), plus the real binary writing and reloading an event in an
//! isolated `VIVA_HOME` across two separate processes.

use std::path::Path;
use std::process::Command;

use serde_json::json;
use tempfile::TempDir;

use viva::foundation::envelope::{
    CallerIdentity, CallerRole, ChannelRegistry, ControlDispatcher, ControlKind, ControlOutcome,
    ControlRequest, RejectionReason,
};
use viva::foundation::events::{self, NewEvent};
use viva::foundation::foundation_migrations;
use viva::foundation::ids::{
    ChannelId, ExecutionId, GrantId, MemberId, ProjectId, TaskId, TerminalId, WorktreeId,
};
use viva::foundation::paths::database_path;
use viva::foundation::records::{
    AttributionSnapshot, ExecutionRecord, ExecutionStatus, HarnessSessionRef, LaunchSpec,
    ProjectSummary, RequestOrigin, SessionKind, SessionRecord, TerminalEvent, TerminalEventKind,
    TerminalOwner, WorkbenchActions, WorkbenchQuery, WorkbenchTerminalSummary, WorktreeSummary,
    get_execution, insert_execution, insert_launch_spec, insert_session, insert_terminal_event,
    list_sessions,
};
use viva::foundation::store::Store;

fn run_viva(home: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .output()
        .expect("spawn viva binary");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Acceptance: "真实 binary 在独立 VIVA_HOME 写入一条 SQLite 事件并在重启后读取".
/// `init` and `event add` run in one process; `event list` runs in a fresh
/// process — the event must be there.
#[test]
fn binary_writes_an_event_and_reloads_it_after_restart() {
    let home = TempDir::new().expect("temp viva home");

    let (ok, stdout, stderr) = run_viva(home.path(), &["init"]);
    assert!(ok, "init failed: {stderr}");
    assert!(stdout.contains("viva home:"), "stdout: {stdout}");
    assert!(
        stdout.contains("schema:    foundation v1"),
        "stdout: {stdout}"
    );

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "event",
            "add",
            "foundation",
            "g0_acceptance_note",
            "user",
            "user-haisu",
            r#"{"hello":"world"}"#,
        ],
    );
    assert!(ok, "event add failed: {stderr}");
    assert!(stdout.contains("recorded"), "stdout: {stdout}");

    // "Restart": a brand-new process reads the same VIVA_HOME.
    let (ok, stdout, stderr) = run_viva(home.path(), &["event", "list"]);
    assert!(ok, "event list failed: {stderr}");
    assert!(
        stdout.contains("g0_acceptance_note") && stdout.contains("user-haisu"),
        "the event written before the restart must be visible after it: {stdout}"
    );

    // A failed transaction must not leave half an event behind.
    let (ok, _stdout, stderr) = run_viva(
        home.path(),
        &[
            "event",
            "add",
            "foundation",
            "tamper_attempt",
            "user",
            "user-x",
            "not-json",
        ],
    );
    assert!(!ok, "malformed payload must fail");
    assert!(stderr.contains("json error"), "stderr: {stderr}");
    let (_, stdout, _) = run_viva(home.path(), &["event", "list"]);
    assert!(
        !stdout.contains("tamper_attempt"),
        "failed write must leave no trace: {stdout}"
    );
}

/// The G0 cross-interface test: one office flow across every frozen
/// interface, reloaded after a full reopen.
#[test]
fn cross_interface_flow_survives_reopen() {
    let dir = TempDir::new().expect("tempdir");
    let db = database_path(dir.path());
    let store = Store::open(&db, foundation_migrations()).expect("open");

    // Sessions: one plain conversation, one task execution.
    let conversation = SessionRecord::new(SessionKind::Conversation, "Morning chat")
        .with_harness_ref(HarnessSessionRef {
            harness: "pi".into(),
            native_session_id: "pi-native-42".into(),
        });
    let task = TaskId::new();
    let execution_session =
        SessionRecord::new(SessionKind::TaskExecution(task.clone()), "Ship V01");
    insert_session(&store, &conversation).expect("insert conversation");
    insert_session(&store, &execution_session).expect("insert execution session");

    // Execution with a full attribution snapshot.
    let attribution =
        AttributionSnapshot::capture(MemberId::new(), "developer", "glm-5.3-flash", "pi-cli")
            .in_project(ProjectId::new());
    let execution = ExecutionRecord::start(task.clone(), attribution)
        .with_session(execution_session.session_id.clone());
    insert_execution(&store, &execution).expect("insert execution");

    // Launch spec: explicit argv/cwd, member dispatch under a grant.
    let spec = LaunchSpec::new(
        vec!["pi".into(), "--prompt".into(), "continue V01".into()],
        dir.path().to_path_buf(),
        RequestOrigin::MemberDispatch {
            grant_id: GrantId::new(),
        },
    )
    .expect("launch spec")
    .for_execution(execution.execution_id.clone())
    .with_member(execution.attribution.member_id.clone());
    insert_launch_spec(&store, &spec).expect("insert launch spec");

    // Terminal owned by the member execution; a user shell is a different owner.
    let terminal = TerminalId::new();
    insert_terminal_event(
        &store,
        &TerminalEvent::new(
            terminal.clone(),
            TerminalOwner::MemberExecution(execution.execution_id.clone()),
            TerminalEventKind::Spawned,
            json!({"argv0": "pi"}),
        ),
    )
    .expect("terminal event");

    // Control channels issued by the host before any dispatcher exists: one
    // bound to the member execution, one to the user.
    let mut registry = ChannelRegistry::new();
    let identity = registry.issue(CallerRole::MemberExecution(execution.execution_id.clone()));
    let user_identity = registry.issue(CallerRole::User);
    let dispatcher = ControlDispatcher::new(&registry, &store);
    let request = ControlRequest::new(ControlKind::Ping, identity.clone());
    assert_eq!(
        dispatcher.submit(&request).expect("submit"),
        ControlOutcome::Accepted {
            request_id: request.request_id.to_string()
        }
    );

    // A worker over a *user* channel cannot claim to be a member execution.
    let forged = CallerIdentity {
        channel_id: user_identity.channel_id.clone(),
        claimed_role: CallerRole::MemberExecution(ExecutionId::new()),
    };
    assert!(matches!(
        dispatcher
            .submit(&ControlRequest::new(ControlKind::Ping, forged))
            .expect("submit"),
        ControlOutcome::Rejected {
            reason: RejectionReason::UntrustedCaller { .. },
            ..
        }
    ));

    // One office event closes the loop.
    let recorded = events::append(
        &store,
        NewEvent {
            domain: viva::foundation::store::DOMAIN_FOUNDATION,
            kind: "g0_flow_completed".into(),
            subject_type: "task".into(),
            subject_id: task.to_string(),
            origin: format!("channel:{}", identity.channel_id),
            payload: json!({"note": "cross-interface flow recorded"}),
        },
    )
    .expect("append event");

    // The workbench contract is implementable against frozen signatures.
    struct Bench;
    impl WorkbenchQuery for Bench {
        fn projects(&self) -> viva::foundation::OfficeResult<Vec<ProjectSummary>> {
            Ok(Vec::new())
        }
        fn worktrees(
            &self,
            _project: &ProjectId,
        ) -> viva::foundation::OfficeResult<Vec<WorktreeSummary>> {
            Ok(Vec::new())
        }
        fn terminals(
            &self,
            _worktree: &WorktreeId,
        ) -> viva::foundation::OfficeResult<Vec<WorkbenchTerminalSummary>> {
            Ok(Vec::new())
        }
    }
    impl WorkbenchActions for Bench {
        fn open_terminal(
            &mut self,
            _worktree: &WorktreeId,
            owner: TerminalOwner,
        ) -> viva::foundation::OfficeResult<viva::foundation::records::WorkbenchTerminalSummary>
        {
            Ok(viva::foundation::records::WorkbenchTerminalSummary {
                terminal_id: TerminalId::new(),
                worktree_id: WorktreeId::new(),
                owner,
            })
        }
        fn stop_terminal(
            &mut self,
            _terminal_id: &TerminalId,
        ) -> viva::foundation::OfficeResult<()> {
            Ok(())
        }
    }
    let mut bench = Bench;
    let opened = bench
        .open_terminal(&WorktreeId::new(), TerminalOwner::UserShell)
        .expect("workbench open_terminal");
    bench
        .stop_terminal(&opened.terminal_id)
        .expect("workbench stop_terminal");

    // ---- "Restart": drop everything and reopen from disk. ----
    let execution_id = execution.execution_id.clone();
    let conversation_id = conversation.session_id.clone();
    drop(store);

    let reopened = Store::open(&db, foundation_migrations()).expect("reopen");
    assert_eq!(
        reopened
            .schema_versions()
            .get(&viva::foundation::store::DOMAIN_FOUNDATION),
        Some(&1),
        "no migration re-runs on reopen"
    );

    let sessions = list_sessions(&reopened).expect("sessions");
    assert_eq!(sessions.len(), 2, "both sessions reload");
    assert!(
        sessions
            .iter()
            .any(|s| s.session_id == conversation_id && s.task_id().is_none())
    );
    let reloaded_execution_session = sessions
        .iter()
        .find(|s| s.task_id() == Some(&task))
        .expect("task execution session reloads");
    assert_eq!(reloaded_execution_session.title, "Ship V01");

    let reloaded = get_execution(&reopened, &execution_id)
        .expect("get")
        .expect("execution reloads");
    assert_eq!(
        reloaded.status,
        ExecutionStatus::Requested,
        "status reloads unchanged"
    );
    assert_eq!(reloaded.attribution.role, "developer");
    assert!(reloaded.process_exit.is_none(), "nothing faked an exit");

    assert_eq!(reopened.row_count("launch_specs").expect("specs"), 1);
    assert_eq!(reopened.row_count("terminal_events").expect("terminals"), 1);
    assert_eq!(
        reopened.row_count("control_requests").expect("control"),
        2,
        "accepted + rejected"
    );
    assert_eq!(reopened.row_count("office_events").expect("events"), 1);

    let reloaded_event = events::get(&reopened, &recorded.event_id)
        .expect("get")
        .expect("event reloads");
    assert_eq!(reloaded_event.kind, "g0_flow_completed");
    assert_eq!(
        reloaded_event.payload,
        json!({"note": "cross-interface flow recorded"})
    );

    // Terminal ids seen above stay distinct from channel ids by type.
    let _distinct_types: (TerminalId, ChannelId) = (terminal, identity.channel_id);
}
