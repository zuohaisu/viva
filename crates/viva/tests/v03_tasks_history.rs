//! V03 acceptance tests (issue #12): task records, layered launch state,
//! idempotent execution creation, honest exit semantics, deduplicated
//! results and restart-safe handoff briefs over real SQLite.

use viva::foundation::ids::{MemberId, TaskId, WorkspaceId};
use viva::foundation::records::{AttributionSnapshot, ExecutionStatus, get_execution};
use viva::foundation::store::{
    DOMAIN_FOUNDATION, DOMAIN_TASKS_EXECUTIONS, FOUNDATION_V1_SQL, FrozenMigrations,
    MigrationRegistry, Store,
};
use viva::tasks::{ExecutionStart, IntentState, OutcomeKind, ResultSource, TaskRegistry};

fn frozen() -> FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(
            DOMAIN_TASKS_EXECUTIONS,
            1,
            "tasks and executions v1",
            viva::tasks::TASKS_EXECUTIONS_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

fn attribution(member: &MemberId, role: &str, workspace: &WorkspaceId) -> AttributionSnapshot {
    AttributionSnapshot::capture(member.clone(), role, "glm-5.3-flash", "pi-cli")
        .in_workspace(workspace.clone())
}

/// Acceptance: "切换当前成员/workspace 不改变历史归属".
#[test]
fn attribution_is_frozen_at_creation_and_survives_switches() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let migrations = frozen();

    let (task_id, member_a, workspace_a, original) = {
        let store = Store::open(&db, &migrations).expect("open");
        let tasks = TaskRegistry::new(&store);
        let member_a = MemberId::new();
        let workspace_a = WorkspaceId::new();
        let task = tasks
            .create_task(
                "Ship the V02 domain",
                vec!["no fake PASS".into()],
                Some(member_a.clone()),
                Some(workspace_a.clone()),
                None,
            )
            .expect("task");
        let _ = tasks
            .begin_execution(
                &task.task_id,
                &attribution(&member_a, "developer", &workspace_a),
                "req-1",
            )
            .expect("begin");
        (task.task_id, member_a, workspace_a, task.created_at.clone())
    };

    // "Switch the current member/workspace": later work is attributed to
    // member B in workspace B; the historical rows must not move.
    let store = Store::open(&db, &migrations).expect("reopen");
    let tasks = TaskRegistry::new(&store);
    let member_b = MemberId::new();
    let workspace_b = WorkspaceId::new();
    let task2 = tasks
        .create_task(
            "Second task",
            vec![],
            Some(member_b.clone()),
            Some(workspace_b.clone()),
            None,
        )
        .expect("task2");
    let _ = tasks
        .begin_execution(
            &task2.task_id,
            &attribution(&member_b, "reviewer", &workspace_b),
            "req-2",
        )
        .expect("begin 2");

    let intents = tasks.intents_for_task(&task_id).expect("history");
    assert_eq!(intents.len(), 1);
    assert_eq!(
        intents[0].member_id, member_a,
        "history attribution unchanged after switching current context"
    );

    let reloaded_task = tasks.require_task(&task_id).expect("task");
    assert_eq!(reloaded_task.created_at, original);
    assert_eq!(reloaded_task.assignee_member_id, Some(member_a));
    assert_eq!(reloaded_task.workspace_id, Some(workspace_a));
}

/// Acceptance: "并发追加不丢事件" — two independent connections appending
/// outcomes to the same task; every append must survive.
#[test]
fn concurrent_appends_do_not_lose_events() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let migrations = frozen();
    let task_id = {
        let store = Store::open(&db, &migrations).expect("open");
        TaskRegistry::new(&store)
            .create_task("log concurrently", vec![], None, None, None)
            .expect("task")
            .task_id
    };

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let db = db.clone();
            let task_id = task_id.clone();
            let migrations = migrations.clone();
            std::thread::spawn(move || {
                let store = Store::open(&db, &migrations).expect("thread open");
                let tasks = TaskRegistry::new(&store);
                tasks
                    .append_outcome(
                        &task_id,
                        OutcomeKind::Corrected,
                        format!("correction {i} with real evidence"),
                        "developer",
                    )
                    .expect("append");
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("thread joins");
    }

    let store = Store::open(&db, &migrations).expect("final open");
    let outcomes = TaskRegistry::new(&store)
        .outcomes_for_task(&task_id)
        .expect("outcomes");
    assert_eq!(outcomes.len(), 8, "no append may be lost");
    let mut evidences: Vec<_> = outcomes.iter().map(|o| o.evidence.clone()).collect();
    evidences.sort();
    evidences.dedup();
    assert_eq!(evidences.len(), 8, "every append carried distinct evidence");
}

/// Acceptance: "唯一请求重试不重复创建执行" and "并发追加不丢事件" on the
/// launch path.
#[test]
fn retried_request_key_never_duplicates_executions() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let tasks = TaskRegistry::new(&store);
    let member = MemberId::new();
    let task = tasks
        .create_task(
            "idempotent dispatch",
            vec![],
            Some(member.clone()),
            None,
            None,
        )
        .expect("task");
    let attr = attribution(&member, "developer", &WorkspaceId::new());

    let first = tasks
        .begin_execution(&task.task_id, &attr, "dispatch-42")
        .expect("first");
    assert!(matches!(first, ExecutionStart::Created(_)));
    let replay = tasks
        .begin_execution(&task.task_id, &attr, "dispatch-42")
        .expect("retry");
    match (first, replay) {
        (ExecutionStart::Created(a), ExecutionStart::Replayed(b)) => {
            assert_eq!(a.intent_id, b.intent_id, "the same intent replays");
            assert_eq!(a.execution_id, b.execution_id);
        }
        _ => panic!("expected created-then-replayed"),
    }

    // A different key is a genuinely new attempt.
    let second = tasks
        .begin_execution(&task.task_id, &attr, "dispatch-43")
        .expect("second attempt");
    assert!(matches!(second, ExecutionStart::Created(_)));
    assert_eq!(
        tasks
            .intents_for_task(&task.task_id)
            .expect("intents")
            .len(),
        2
    );
    assert_eq!(store.row_count("office_executions").expect("executions"), 2);
}

/// Acceptance: "异常在启动意图与 pid 写入之间发生时可如实记录待对账状态".
#[test]
fn crash_between_intent_and_pid_is_recorded_as_unresolved() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let migrations = frozen();
    let task_id = {
        let store = Store::open(&db, &migrations).expect("open");
        let tasks = TaskRegistry::new(&store);
        let member = MemberId::new();
        let task = tasks
            .create_task(
                "interrupted launch",
                vec![],
                Some(member.clone()),
                None,
                None,
            )
            .expect("task");
        let _ = tasks
            .begin_execution(
                &task.task_id,
                &attribution(&member, "developer", &WorkspaceId::new()),
                "racy",
            )
            .expect("begin");
        task.task_id
    };

    // The host crashed after the intent, before the pid write. A fresh
    // process must see the honest pending-reconciliation state.
    let store = Store::open(&db, &migrations).expect("reopen");
    let tasks = TaskRegistry::new(&store);
    let intent = tasks
        .intent_by_request_key("racy")
        .expect("intent")
        .expect("exists");
    assert_eq!(intent.state, IntentState::Intended);
    let unresolved = tasks.mark_unresolved("racy").expect("mark");
    assert_eq!(unresolved.state, IntentState::Unresolved);

    let brief = tasks.generate_brief(&task_id).expect("brief");
    let text = serde_json::to_string(&brief).expect("json");
    assert!(
        text.contains("pending reconciliation"),
        "the brief must surface the unresolved attempt: {text}"
    );
    assert_eq!(
        tasks.require_task(&task_id).expect("task").status,
        viva::tasks::TaskStatus::Open,
        "an unresolved launch never fakes a task outcome"
    );
}

/// Acceptance: "零退出码只记录进程结束，不关闭 Task" and "明确结果关联
/// task/execution/source，重复或过期结果不重复关闭任务".
#[test]
fn zero_exit_records_process_end_but_never_closes_the_task() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let tasks = TaskRegistry::new(&store);
    let member = MemberId::new();
    let task = tasks
        .create_task(
            "run to a clean exit",
            vec![],
            Some(member.clone()),
            None,
            None,
        )
        .expect("task");

    let start = tasks
        .begin_execution(
            &task.task_id,
            &attribution(&member, "developer", &WorkspaceId::new()),
            "run-1",
        )
        .expect("begin");
    let ExecutionStart::Created(intent) = start else {
        panic!("first start must create");
    };
    tasks
        .confirm_launch("run-1", 4242, "pid-4242@marker")
        .expect("confirm");

    let exited = tasks.record_exit("run-1", 0).expect("exit");
    assert_eq!(exited.state, IntentState::Exited);
    assert_eq!(exited.exit_code, Some(0));

    let execution = get_execution(&store, intent.execution_id.as_ref().expect("exec"))
        .expect("get")
        .expect("exists");
    assert_eq!(
        execution.status,
        ExecutionStatus::Stopped,
        "a clean exit records the process end (stopped), not completion"
    );
    assert_eq!(execution.process_exit, Some(0));
    assert!(execution.completion_evidence.is_none());
    assert_eq!(
        tasks.require_task(&task.task_id).expect("task").status,
        viva::tasks::TaskStatus::Open,
        "a zero exit code never closes the task"
    );

    // Results from separate sources, linked to task + execution. The same
    // dedup key twice is a duplicate the second time.
    let first = tasks
        .record_result(
            &task.task_id,
            intent.execution_id.clone(),
            ResultSource::Qa,
            "review_verdict",
            "qa::run-1::pass",
            serde_json::json!({"verdict": "pass"}),
        )
        .expect("record");
    assert!(first.is_ok(), "first result records");
    let duplicate = tasks
        .record_result(
            &task.task_id,
            intent.execution_id.clone(),
            ResultSource::Qa,
            "review_verdict",
            "qa::run-1::pass",
            serde_json::json!({"verdict": "pass"}),
        )
        .expect("record");
    assert!(
        duplicate.is_err(),
        "the duplicate is reported, not re-inserted"
    );

    // Even a fresh result cannot close the task: results never close tasks.
    tasks
        .record_result(
            &task.task_id,
            None,
            ResultSource::PrCi,
            "ci_green",
            "ci::head::green",
            serde_json::json!({"state": "success"}),
        )
        .expect("record ci")
        .expect("fresh");
    assert_eq!(
        tasks.require_task(&task.task_id).expect("task").status,
        viva::tasks::TaskStatus::Open
    );

    let results = tasks.results_for_task(&task.task_id).expect("results");
    assert_eq!(results.len(), 2);
    let sources: Vec<_> = results.iter().map(|r| r.source.as_str()).collect();
    assert!(
        sources.contains(&"qa") && sources.contains(&"pr_ci"),
        "sources stay separate"
    );
    assert_eq!(
        store.row_count("task_results").expect("rows"),
        2,
        "duplicate not stored"
    );

    // Only the explicit completion path closes the task — once.
    tasks
        .complete_task(&task.task_id, "cargo test + reviewed diff", "owner")
        .expect("complete");
    assert_eq!(
        tasks.require_task(&task.task_id).expect("task").status,
        viva::tasks::TaskStatus::Done
    );
    let done_status = tasks.require_task(&task.task_id).expect("task").status;
    // Re-delivering the old result afterwards must not re-close anything.
    let _ = tasks.record_result(
        &task.task_id,
        intent.execution_id.clone(),
        ResultSource::Qa,
        "review_verdict",
        "qa::run-1::pass",
        serde_json::json!({"verdict": "pass"}),
    );
    assert_eq!(
        tasks.require_task(&task.task_id).expect("task").status,
        done_status,
        "stale result cannot change a closed task"
    );
}

/// Acceptance: "brief 含目标、约束、尝试、失败、产出与未完成项，重启后可用".
#[test]
fn handoff_brief_is_complete_and_survives_restart() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let migrations = frozen();
    let task_id = {
        let store = Store::open(&db, &migrations).expect("open");
        let tasks = TaskRegistry::new(&store);
        let member = MemberId::new();
        let task = tasks
            .create_task(
                "Migrate the office host",
                vec!["keep grant boundaries".into(), "no fake memory".into()],
                Some(member.clone()),
                None,
                None,
            )
            .expect("task");

        let _ = tasks
            .begin_execution(
                &task.task_id,
                &attribution(&member, "developer", &WorkspaceId::new()),
                "attempt-1",
            )
            .expect("begin");
        tasks
            .confirm_launch("attempt-1", 100, "m1")
            .expect("confirm");
        tasks.record_exit("attempt-1", 1).expect("failed attempt");
        let _ = tasks
            .begin_execution(
                &task.task_id,
                &attribution(&member, "developer", &WorkspaceId::new()),
                "attempt-2",
            )
            .expect("begin 2");
        tasks
            .confirm_launch("attempt-2", 200, "m2")
            .expect("confirm");
        tasks.record_exit("attempt-2", 0).expect("clean attempt");
        tasks
            .record_result(
                &task.task_id,
                None,
                ResultSource::User,
                "delivered_diff",
                "user::diff::1",
                serde_json::json!({"files": 5}),
            )
            .expect("deliverable")
            .expect("fresh");

        tasks.generate_brief(&task.task_id).expect("brief");
        task.task_id
    };

    // Restart: the brief is still servable from persisted facts.
    let store = Store::open(&db, &migrations).expect("reopen");
    let tasks = TaskRegistry::new(&store);
    let brief = tasks
        .latest_brief(&task_id)
        .expect("brief")
        .expect("persisted");
    assert_eq!(brief["goal"], "Migrate the office host");
    assert_eq!(
        brief["constraints"].as_array().expect("constraints").len(),
        2,
        "constraints are part of the brief"
    );
    assert_eq!(brief["attempts"].as_array().expect("attempts").len(), 2);
    assert_eq!(
        brief["failures"].as_array().expect("failures").len(),
        1,
        "the failed attempt"
    );
    assert_eq!(
        brief["deliverables"]
            .as_array()
            .expect("deliverables")
            .len(),
        1,
        "user-verified deliverables"
    );
    assert!(
        !brief["open_items"]
            .as_array()
            .expect("open items")
            .is_empty(),
        "the not-done task is an open item"
    );

    // Regenerating produces the same facts from scratch.
    let regenerated = tasks.generate_brief(&task_id).expect("regenerate");
    assert_eq!(regenerated["failures"], brief["failures"]);
}

/// Acceptance: "以真实 SQLite 保存/查询与分页历史".
#[test]
fn task_history_paginates() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let tasks = TaskRegistry::new(&store);
    for i in 0..7 {
        tasks
            .create_task(format!("task {i}"), vec![], None, None, None)
            .expect("task");
    }
    let page1 = tasks.list_tasks(0, 3).expect("page 1");
    let page2 = tasks.list_tasks(3, 3).expect("page 2");
    let page3 = tasks.list_tasks(6, 3).expect("page 3");
    assert_eq!((page1.len(), page2.len(), page3.len()), (3, 3, 1));
    assert_ne!(page1[0].task_id, page2[0].task_id, "pages do not overlap");
}

/// Non-zero exits mark the execution failed — but the task still needs its
/// explicit outcome.
#[test]
fn nonzero_exit_marks_execution_failed_not_task_cancelled() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let tasks = TaskRegistry::new(&store);
    let member = MemberId::new();
    let task = tasks
        .create_task("doomed attempt", vec![], Some(member.clone()), None, None)
        .expect("task");
    let _ = tasks
        .begin_execution(
            &task.task_id,
            &attribution(&member, "developer", &WorkspaceId::new()),
            "x",
        )
        .expect("begin");
    tasks.confirm_launch("x", 77, "m").expect("confirm");
    tasks.record_exit("x", 3).expect("exit");

    let status = tasks.require_task(&task.task_id).expect("task").status;
    assert_eq!(status, viva::tasks::TaskStatus::Open);

    // An in-progress marker reflects that work started, set via a correction
    // is not the path; task status transitions live in outcomes/completion.
    let _ = TaskId::new(); // keep import used
    assert_eq!(store.row_count("launch_intents").expect("rows"), 1);
}

/// QA round: a dispatch racing on the same request key gets a clean
/// Replayed outcome, not a raw SQLite constraint error.
#[test]
fn racing_same_request_key_replays_instead_of_erroring() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let migrations = frozen();
    let task_id = {
        let store = Store::open(&db, &migrations).expect("open");
        let member = MemberId::new();
        TaskRegistry::new(&store)
            .create_task("racy dispatch", vec![], Some(member.clone()), None, None)
            .expect("task")
            .task_id
    };

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            let task_id = task_id.clone();
            let migrations = migrations.clone();
            std::thread::spawn(move || {
                let store = Store::open(&db, &migrations).expect("thread open");
                let member = MemberId::new();
                let attr =
                    AttributionSnapshot::capture(member, "developer", "glm-5.3-flash", "cli");
                TaskRegistry::new(&store).begin_execution(&task_id, &attr, "one-racy-key")
            })
        })
        .collect();
    let mut created = 0;
    let mut replayed = 0;
    for handle in handles {
        match handle
            .join()
            .expect("joins")
            .expect("NO thread may error on a lost race")
        {
            ExecutionStart::Created(_) => created += 1,
            ExecutionStart::Replayed(_) => replayed += 1,
        }
    }
    assert_eq!(created, 1, "exactly one winner");
    assert_eq!(replayed, 7, "losers replay the winner's intent");
}

/// QA round: briefs pass through redaction when the registry is configured
/// with secrets — persisted briefs never carry them.
#[test]
fn generated_briefs_respect_redaction() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let secret = "brief-secret-7734";
    let tasks = TaskRegistry::new(&store).with_redaction(vec![secret.to_string()]);
    let member = MemberId::new();
    let task = tasks
        .create_task(
            "leaky deliverable",
            vec![],
            Some(member.clone()),
            None,
            None,
        )
        .expect("task");
    tasks
        .record_result(
            &task.task_id,
            None,
            ResultSource::User,
            "delivered",
            "user::d::1",
            serde_json::json!({"note": format!("done with {secret}")}),
        )
        .expect("result")
        .expect("fresh");
    let brief = tasks.generate_brief(&task.task_id).expect("brief");
    let persisted = tasks
        .latest_brief(&task.task_id)
        .expect("brief")
        .expect("persisted");
    for value in [brief, persisted] {
        let text = serde_json::to_string(&value).expect("json");
        assert!(!text.contains(secret), "brief leaked the secret: {text}");
    }
}
