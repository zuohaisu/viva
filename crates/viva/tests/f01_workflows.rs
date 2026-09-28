//! F01 acceptance tests (issue #23): configurable delivery workflows with
//! evidence-gated advancement, head-bound evidence, retry budgets that
//! pause instead of faking success, structural rejection of protected
//! actions, owner-granted delivery authorization, and the read-only review
//! shape proving workflow roles/order are data.

use std::str::FromStr as _;

use viva::authority::{AuthorityEngine, GrantMode};
use viva::foundation::ids::{MemberId, TaskId, utc_now};
use viva::foundation::store::{
    DOMAIN_AUTHORITY, DOMAIN_FOUNDATION, DOMAIN_TASKS_EXECUTIONS, DOMAIN_WORKFLOWS,
    FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::tasks::TaskRegistry;
use viva::workflows::{
    RunStatus, StepAdvance, StepEvidence, WorkflowConfig, WorkflowEngine, WorkflowStep,
};

const HEAD_A: &str = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
const HEAD_B: &str = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";

fn frozen() -> viva::foundation::store::FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(
            DOMAIN_TASKS_EXECUTIONS,
            1,
            "tasks and executions v1",
            viva::tasks::TASKS_EXECUTIONS_V1_SQL,
        )
        .register(
            DOMAIN_AUTHORITY,
            1,
            "authority v1",
            viva::authority::AUTHORITY_V1_SQL,
        )
        .register(
            DOMAIN_WORKFLOWS,
            1,
            "workflows v1",
            viva::workflows::WORKFLOWS_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

fn store() -> Store {
    Store::open_in_memory(&frozen()).expect("store")
}

fn a_task(store: &Store) -> TaskId {
    let tasks = TaskRegistry::new(store);
    tasks
        .create_task(
            "Fix the flaky ordering assertion in the export test",
            vec!["low-risk: test-only change".to_string()],
            None,
            None,
            None,
        )
        .expect("task")
        .task_id
}

fn evidence(kind: &str, head: &str, note: &str) -> StepEvidence {
    StepEvidence::new(kind, note).with_head(head)
}

/// Acceptance: "一张真实低风险任务走完实现→验证→独立复核→必要修复→授权 PR，
/// 过程记录可恢复". The verify step really fails once, routes through the
/// bounded fix step, re-earns verification, passes independent review, and
/// delivers only under a live owner grant. Nothing merges.
#[test]
fn delivery_walk_with_real_fix_and_authorized_delivery() {
    let store = store();
    let engine = WorkflowEngine::new(&store);
    let authority = AuthorityEngine::new(&store);
    let task_id = a_task(&store);
    let actor = MemberId::new();
    let implementer = MemberId::new();
    let reviewer = MemberId::new();
    let deliverer = MemberId::new();

    engine
        .register_config(&WorkflowConfig::delivery_default())
        .expect("delivery config registers");

    let run = engine
        .start_run(&task_id, "delivery", HEAD_A)
        .expect("run starts");
    assert_eq!(run.status, RunStatus::Running);

    // Implement passes on head A.
    let advance = engine
        .record_step_result(
            &run.run_id,
            &implementer,
            true,
            vec![evidence("implementation", HEAD_A, "change committed")],
        )
        .expect("implement");
    assert_eq!(advance, StepAdvance::Passed { next_step: Some("verify".into()) });

    // Verify really fails: a bug is found on head A. The run routes to the
    // configured fix step — evidence records the failure, honestly.
    let advance = engine
        .record_step_result(
            &run.run_id,
            &actor,
            false,
            vec![evidence("verification", HEAD_A, "cargo test: ordering assertion failed")],
        )
        .expect("verify fail recorded");
    assert_eq!(advance, StepAdvance::RoutedTo { step_id: "fix".into() });

    // Fix passes and routes back to verification (fixed work re-earns it).
    let advance = engine
        .record_step_result(
            &run.run_id,
            &actor,
            true,
            vec![evidence("fix", HEAD_A, "ordering made deterministic")],
        )
        .expect("fix");
    assert_eq!(advance, StepAdvance::Passed { next_step: Some("verify".into()) });

    // Verification passes on the same head.
    let advance = engine
        .record_step_result(
            &run.run_id,
            &actor,
            true,
            vec![evidence("verification", HEAD_A, "cargo test --workspace: pass")],
        )
        .expect("verify pass");
    assert_eq!(advance, StepAdvance::Passed { next_step: Some("review".into()) });

    // Independent review passes.
    let advance = engine
        .record_step_result(
            &run.run_id,
            &reviewer,
            true,
            vec![evidence("review", HEAD_A, "independent QA review: no blocking findings")],
        )
        .expect("review pass");
    assert_eq!(advance, StepAdvance::Passed { next_step: Some("deliver".into()) });

    // Delivery cannot pass on a promise: missing authorization evidence.
    let err = engine
        .record_step_result(
            &run.run_id,
            &deliverer,
            true,
            vec![evidence("delivery", HEAD_A, "PR opened")],
        )
        .expect_err("authorization evidence is required");
    assert!(err.to_string().contains("authorization"), "got: {err}");

    // Delivery under a PROPOSE-mode grant is refused: a remote write needs
    // an ACT_* owner grant.
    let weak_grant = authority
        .issue_root_grant(
            Some(deliverer.clone()),
            Some(task_id.clone()),
            vec!["deliver_pr".into()],
            GrantMode::Propose,
            None,
        )
        .expect("weak grant");
    let err = engine
        .record_step_result(
            &run.run_id,
            &deliverer,
            true,
            vec![
                evidence("delivery", HEAD_A, "PR opened"),
                StepEvidence::new("authorization", "owner said ok earlier")
                    .with_grant(weak_grant.grant_id.as_str()),
            ],
        )
        .expect_err("PROPOSE grant must not deliver");
    assert!(
        err.to_string().contains("delivery authorization rejected"),
        "got: {err}"
    );
    let denials = authority.recent_denials(10).expect("denials");
    assert!(
        denials.iter().any(|(_, action, reason, _)| action == "deliver_pr"
            && reason.contains("insufficient")),
        "the mode denial is appended for audit: {denials:?}"
    );

    // The real delivery: a live owner-rooted grant covering `deliver_pr`,
    // scoped to this task.
    let grant = authority
        .issue_root_grant(
            Some(deliverer.clone()),
            Some(task_id.clone()),
            vec!["deliver_pr".into()],
            GrantMode::ActWithApproval,
            Some(format!("2099-{}", &utc_now()[5..])),
        )
        .expect("owner grant");
    let advance = engine
        .record_step_result(
            &run.run_id,
            &deliverer,
            true,
            vec![
                evidence("delivery", HEAD_A, "PR opened from the task worktree"),
                StepEvidence::new("authorization", "owner grant issued for this task")
                    .with_grant(grant.grant_id.as_str()),
            ],
        )
        .expect("authorized delivery");
    assert_eq!(advance, StepAdvance::Passed { next_step: None });

    let finished = engine.get_run(&run.run_id).expect("run").expect("run exists");
    assert_eq!(finished.status, RunStatus::Completed);

    // The workflow never closes the task: completion is a separate
    // appended outcome with evidence referencing the run.
    let tasks = TaskRegistry::new(&store);
    tasks
        .complete_task(
            &task_id,
            format!("workflow run {} completed; PR opened under grant {}", run.run_id, grant.grant_id),
            "owner-authorized delivery",
        )
        .expect("task outcome");

    // The whole process record is recoverable: six attempts, each with its
    // actor, outcome and head binding.
    let history = engine.run_history(&run.run_id).expect("history");
    assert_eq!(history.records.len(), 6);
    let outcomes: Vec<(&str, &str)> = history
        .records
        .iter()
        .map(|r| (r.step_id.as_str(), r.outcome.as_str()))
        .collect();
    assert_eq!(
        outcomes,
        vec![
            ("implement", "passed"),
            ("verify", "failed"),
            ("fix", "passed"),
            ("verify", "passed"),
            ("review", "passed"),
            ("deliver", "passed"),
        ]
    );
    assert!(history
        .records
        .iter()
        .all(|r| r.evidence.iter().all(|e| e.head_sha.as_deref() != Some("stale-head"))));
    assert_eq!(history.config.steps.len(), 5, "config round-trips as data");
}

/// Acceptance: "CI结果只适用相同 head". Evidence bound to a different head
/// is rejected and nothing is recorded.
#[test]
fn evidence_for_another_head_is_rejected() {
    let store = store();
    let engine = WorkflowEngine::new(&store);
    let task_id = a_task(&store);
    engine
        .register_config(&WorkflowConfig::delivery_default())
        .expect("config");
    let run = engine.start_run(&task_id, "delivery", HEAD_A).expect("run");

    let err = engine
        .record_step_result(
            &run.run_id,
            &MemberId::new(),
            true,
            vec![evidence("implementation", HEAD_B, "CI green — but on another head")],
        )
        .expect_err("cross-head evidence must be rejected");
    assert!(err.to_string().contains("only apply to the head"), "got: {err}");

    // Head-bound requirements also refuse headless evidence.
    let err = engine
        .record_step_result(
            &run.run_id,
            &MemberId::new(),
            true,
            vec![StepEvidence::new("implementation", "trust me")],
        )
        .expect_err("head-bound evidence must carry a head");
    assert!(err.to_string().contains("head SHA"), "got: {err}");

    let history = engine.run_history(&run.run_id).expect("history");
    assert!(history.records.is_empty(), "rejected results leave no record");
    assert_eq!(history.run.current_step, 0);
}

/// Acceptance: "失败与 pending 不改成PASS" — a step that keeps failing
/// exhausts its budget and the run PAUSES with the reason; resuming after a
/// fix rebinds the head and demands fresh evidence on the new head.
#[test]
fn exhausted_budget_pauses_and_resume_rebinds_head() {
    let store = store();
    let engine = WorkflowEngine::new(&store);
    let task_id = a_task(&store);
    engine
        .register_config(&WorkflowConfig::delivery_default())
        .expect("config");
    let run = engine.start_run(&task_id, "delivery", HEAD_A).expect("run");
    let actor = MemberId::new();

    // Implement passes, then verification fails twice (budget 2):
    // first fail routes to fix, the fix also fails… second verify fail
    // exhausts the budget.
    engine
        .record_step_result(
            &run.run_id,
            &actor,
            true,
            vec![evidence("implementation", HEAD_A, "done")],
        )
        .expect("implement");
    let advance = engine
        .record_step_result(
            &run.run_id,
            &actor,
            false,
            vec![evidence("verification", HEAD_A, "still failing")],
        )
        .expect("verify fail 1");
    assert_eq!(advance, StepAdvance::RoutedTo { step_id: "fix".into() });

    // The fix itself fails twice → its budget (2) is exhausted → pause.
    engine
        .record_step_result(
            &run.run_id,
            &actor,
            false,
            vec![evidence("fix", HEAD_A, "attempt 1 failed")],
        )
        .expect("fix fail 1");
    let advance = engine
        .record_step_result(
            &run.run_id,
            &actor,
            false,
            vec![evidence("fix", HEAD_A, "attempt 2 failed")],
        )
        .expect("fix fail 2");
    let StepAdvance::Paused { reason } = advance else {
        panic!("budget exhaustion must pause, got {advance:?}");
    };
    assert!(reason.contains("fix"), "reason names the step: {reason}");

    let paused = engine.get_run(&run.run_id).expect("run").expect("run");
    assert_eq!(paused.status, RunStatus::Paused);
    assert_eq!(paused.pause_reason.as_deref(), Some(reason.as_str()));

    // A paused run records nothing further.
    let err = engine
        .record_step_result(
            &run.run_id,
            &actor,
            true,
            vec![evidence("fix", HEAD_A, "late claim")],
        )
        .expect_err("paused runs take no results");
    assert!(err.to_string().contains("running runs only"), "got: {err}");

    // Resume after a real fix commit: the run rebinds to head B. The old
    // attempt keeps its head-A binding; the new pass must be bound to B.
    engine
        .resume_paused(&run.run_id, Some(HEAD_B.into()))
        .expect("resume");
    let err = engine
        .record_step_result(
            &run.run_id,
            &actor,
            true,
            vec![evidence("fix", HEAD_A, "old head evidence after rebind")],
        )
        .expect_err("stale-head evidence is refused after a rebind");
    assert!(err.to_string().contains("only apply to the head"), "got: {err}");
    let advance = engine
        .record_step_result(
            &run.run_id,
            &actor,
            true,
            vec![evidence("fix", HEAD_B, "real fix committed")],
        )
        .expect("fix on new head");
    assert_eq!(advance, StepAdvance::Passed { next_step: Some("verify".into()) });

    let history = engine.run_history(&run.run_id).expect("history");
    let fix_records: Vec<&str> = history
        .records
        .iter()
        .filter(|r| r.step_id == "fix")
        .filter_map(|r| r.evidence.first().and_then(|e| e.head_sha.as_deref()))
        .collect();
    assert_eq!(
        fix_records,
        vec![HEAD_A, HEAD_A, HEAD_B],
        "history preserves exactly which head each attempt belonged to"
    );
}

/// Acceptance: "workflow角色与顺序是数据" — the read-only review shape has
/// no dispatch-class step and no fix routing, and walks to completion on
/// the same engine.
#[test]
fn read_only_review_shape_is_a_different_walk() {
    let store = store();
    let engine = WorkflowEngine::new(&store);
    let task_id = a_task(&store);

    let config = WorkflowConfig::read_only_review_default();
    assert!(config.steps.iter().all(|s| {
        !matches!(s.action.as_deref(), Some("deliver_pr") | Some("dispatch_delegated"))
    }));
    assert!(config.steps.iter().all(|s| s.on_fail.is_none()));
    engine.register_config(&config).expect("config");

    let run = engine.start_run(&task_id, "read-only-review", HEAD_A).expect("run");
    let actor = MemberId::new();
    let mut next = None;
    for (kind, head) in [
        ("triage", Some(HEAD_A)),
        ("review", Some(HEAD_A)),
        ("summary", None),
    ] {
        let mut item = StepEvidence::new(kind, "recorded");
        if let Some(head) = head {
            item = item.with_head(head);
        }
        let advance = engine
            .record_step_result(&run.run_id, &actor, true, vec![item])
            .expect("step");
        match advance {
            StepAdvance::Passed { next_step } => next = next_step,
            other => panic!("unexpected advance {other:?}"),
        }
    }
    assert_eq!(next, None, "the review walk ends completed");
    let finished = engine.get_run(&run.run_id).expect("run").expect("run");
    assert_eq!(finished.status, RunStatus::Completed);
}

/// A workflow can never carry a protected action: such configs are
/// rejected at registration, so self-merge/self-approve is structurally
/// impossible for developer/QA members.
#[test]
fn protected_actions_never_enter_a_workflow() {
    let store = store();
    let engine = WorkflowEngine::new(&store);
    let merge_step = WorkflowStep {
        step_id: "merge".into(),
        role: "merger".into(),
        title: "Merge the PR".into(),
        action: Some("merge_pull_request".into()),
        requires_evidence: vec![],
        max_attempts: 1,
        on_pass: None,
        on_fail: None,
    };
    let err = engine
        .register_config(&WorkflowConfig {
            name: "self-merge".into(),
            steps: vec![merge_step],
        })
        .expect_err("protected action must be rejected");
    assert!(err.to_string().contains("protected"), "got: {err}");

    // Approvals are equally out of reach.
    let mut approve_config = WorkflowConfig::delivery_default();
    approve_config.name = "self-approve".into();
    approve_config.steps[2].action = Some("approve_pull_request".into());
    let err = engine
        .register_config(&approve_config)
        .expect_err("approve action must be rejected");
    assert!(err.to_string().contains("protected"), "got: {err}");
}

/// Config sanity: transitions must target known steps, on_fail must differ
/// from the step itself, budgets must be positive.
#[test]
fn malformed_configs_are_rejected() {
    let store = store();
    let engine = WorkflowEngine::new(&store);

    let mut config = WorkflowConfig::delivery_default();
    config.steps[1].on_fail = Some("no-such-step".into());
    assert!(engine.register_config(&config).is_err());

    let mut config = WorkflowConfig::delivery_default();
    config.steps[0].on_fail = Some("implement".into());
    assert!(engine.register_config(&config).is_err());

    let mut config = WorkflowConfig::delivery_default();
    config.steps[0].max_attempts = 0;
    assert!(engine.register_config(&config).is_err());
}

/// The task is the single delivery state source: at most one active run
/// per task; a new run is possible only after the old one completes or is
/// aborted.
#[test]
fn one_active_run_per_task() {
    let store = store();
    let engine = WorkflowEngine::new(&store);
    let task_id = a_task(&store);
    engine
        .register_config(&WorkflowConfig::read_only_review_default())
        .expect("config");

    let first = engine.start_run(&task_id, "read-only-review", HEAD_A).expect("run");
    let err = engine
        .start_run(&task_id, "read-only-review", HEAD_A)
        .expect_err("second active run must be refused");
    assert!(err.to_string().contains("active workflow run"), "got: {err}");

    engine
        .abort_run(&first.run_id, "superseded by a re-scope")
        .expect("abort");
    let err = engine
        .record_step_result(
            &first.run_id,
            &MemberId::new(),
            true,
            vec![StepEvidence::new("triage", "late").with_head(HEAD_A)],
        )
        .expect_err("aborted runs take no results");
    assert!(err.to_string().contains("running runs only"), "got: {err}");

    let second = engine
        .start_run(&task_id, "read-only-review", HEAD_B)
        .expect("a fresh run after abort");
    assert_eq!(second.status, RunStatus::Running);

    // Runs are bound to real tasks.
    let missing = TaskId::from_str("task-nope").expect("well-formed id");
    assert!(engine.start_run(&missing, "read-only-review", HEAD_A).is_err());
}
