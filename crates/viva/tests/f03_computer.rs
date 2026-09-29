//! F03 acceptance tests (issue #25): reuse-first computer operations —
//! capability audit with honest availability, task-scoped grant gating,
//! locate→act→verify with pre/post evidence, target-drift refusal,
//! permission-gap honesty, and foreground input serialization that never
//! queues independent read contexts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use viva::authority::{Actor, AuthorityEngine, GrantMode};
use viva::foundation::ids::{MemberId, TaskId};
use viva::foundation::store::{
    DOMAIN_AUTHORITY, DOMAIN_FOUNDATION, DOMAIN_TASKS_EXECUTIONS, DOMAIN_TOOLS_COMPUTER,
    FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::tools::computer::{
    AUDITED_TOOLS, ActionSpec, ActionState, ComputerEngine, ForegroundCoordinator, ToolFailure,
    ToolRunner, smoke_specs,
};

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
            DOMAIN_TOOLS_COMPUTER,
            1,
            "tools computer v1",
            viva::tools::computer::TOOLS_COMPUTER_V1_SQL,
        )
        .register(
            DOMAIN_TOOLS_COMPUTER,
            2,
            "tools computer v2 foreground lease",
            viva::tools::computer::TOOLS_COMPUTER_V2_SQL,
        )
        .freeze()
        .expect("registry")
}

fn store() -> Store {
    Store::open_in_memory(&frozen()).expect("store")
}

/// A scripted runner: answers by argv marker, counts real action
/// invocations, and can delay the action (to hold the foreground lane).
struct ScriptedRunner {
    responses: Mutex<Vec<(String, String)>>,
    /// Responses served only after a real action ran (simulating that the
    /// action changed the world).
    after_action: Mutex<Vec<(String, String)>>,
    failures: Mutex<Vec<(String, String)>>,
    action_runs: AtomicUsize,
    action_delay: std::time::Duration,
}

impl ScriptedRunner {
    fn new() -> Self {
        Self {
            responses: Mutex::new(Vec::new()),
            after_action: Mutex::new(Vec::new()),
            failures: Mutex::new(Vec::new()),
            action_runs: AtomicUsize::new(0),
            action_delay: std::time::Duration::ZERO,
        }
    }

    fn respond(self, marker: &str, output: &str) -> Self {
        self.responses
            .lock()
            .expect("lock")
            .push((marker.to_string(), output.to_string()));
        self
    }

    fn respond_after_action(self, marker: &str, output: &str) -> Self {
        self.after_action
            .lock()
            .expect("lock")
            .push((marker.to_string(), output.to_string()));
        self
    }

    fn fail_on(self, marker: &str, detail: &str) -> Self {
        self.failures
            .lock()
            .expect("lock")
            .push((marker.to_string(), detail.to_string()));
        self
    }

    fn with_action_delay(mut self, delay: std::time::Duration) -> Self {
        self.action_delay = delay;
        self
    }

    fn action_runs(&self) -> usize {
        self.action_runs.load(Ordering::SeqCst)
    }
}

impl ToolRunner for ScriptedRunner {
    fn run(&self, program: &str, argv: &[String]) -> Result<String, ToolFailure> {
        let joined = argv.join(" ");
        {
            let failures = self.failures.lock().expect("lock");
            for (marker, detail) in failures.iter() {
                if joined.contains(marker.as_str()) {
                    return Err(ToolFailure {
                        program: program.to_string(),
                        detail: detail.clone(),
                    });
                }
            }
        }
        if !joined.contains("list-apps") {
            self.action_runs.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.action_delay);
        }
        let acted = self.action_runs.load(Ordering::SeqCst) > 0;
        let after = self.after_action.lock().expect("lock");
        if acted {
            for (marker, output) in after.iter() {
                if joined.contains(marker.as_str()) {
                    return Ok(output.clone());
                }
            }
        }
        drop(after);
        let responses = self.responses.lock().expect("lock");
        for (marker, output) in responses.iter() {
            if joined.contains(marker.as_str()) {
                return Ok(output.clone());
            }
        }
        // Unscripted argv is a failure — a probe the fake host cannot run
        // is absent, never silently present.
        Err(ToolFailure {
            program: program.to_string(),
            detail: format!("not scripted: {joined}"),
        })
    }
}

fn actor_with_grant(
    authority: &AuthorityEngine<'_>,
    task: Option<&TaskId>,
    mode: GrantMode,
) -> Actor {
    let member = MemberId::new();
    let grant = authority
        .issue_root_grant(
            Some(member.clone()),
            task.cloned(),
            vec!["computer_input".into()],
            mode,
            None,
        )
        .expect("grant");
    Actor::Member {
        member,
        grant: Some(grant.grant_id),
    }
}

fn spec(target: &str, fg: bool) -> ActionSpec {
    ActionSpec {
        tool: "orca-computer",
        program: "orca",
        locator: vec!["computer".into(), "list-apps".into(), "--json".into()],
        target: target.to_string(),
        argv: vec![
            "computer".into(),
            "get-app-state".into(),
            "--app".into(),
            target.into(),
        ],
        // Verify markers on BOTH axes: the action's own output must show
        // the observation (the app state JSON), and the post locator must
        // still show the target.
        expect_post: target.to_string(),
        expect_action_output: Some("windows".into()),
        foreground: fg,
    }
}

const FINDER_STATE: &str = r#"{"windows":[{"id":1,"title":"Home"}]}"#;
const APP_LIST: &str = r#"{"apps":[{"name":"Finder"},{"name":"Google Chrome"}]}"#;

/// Acceptance: "权限缺失/目标漂移拒绝或暂停，不模拟成功" — a target not
/// visible in the pre-state is refused before anything runs; a broken
/// tool or missing permission fails honestly; a post-state without the
/// expected marker is recorded as failed.
#[test]
fn target_drift_and_permission_gaps_never_fake_success() {
    let store = store();
    let authority = AuthorityEngine::new(&store);
    let task = TaskId::new();
    let actor = actor_with_grant(&authority, Some(&task), GrantMode::ActAutonomously);

    // Pre-state does not mention the target: refusal, zero action runs.
    let runner =
        Arc::new(ScriptedRunner::new().respond("list-apps", r#"{"apps":[{"name":"Finder"}]}"#));
    {
        let engine =
            ComputerEngine::with_runner(&store, Box::new(SharedRunner(Arc::clone(&runner))));
        let record = engine
            .execute(&authority, &actor, &task, &spec("Google Chrome", false))
            .expect("the refusal is recorded");
        assert_eq!(record.state, ActionState::Refused);
        assert!(
            record
                .reason
                .as_deref()
                .unwrap_or("")
                .contains("not visible"),
            "drift refusal explains itself: {:?}",
            record.reason
        );
        assert!(record.pre_state.is_some(), "pre-state is kept as evidence");
    }
    assert_eq!(runner.action_runs(), 0, "no action ran on refusal");

    // Locator failure (missing permission, broken tool): honest failure.
    let runner =
        Arc::new(ScriptedRunner::new().fail_on("list-apps", "accessibility permission missing"));
    let engine = ComputerEngine::with_runner(&store, Box::new(SharedRunner(runner)));
    let record = engine
        .execute(&authority, &actor, &task, &spec("Finder", false))
        .expect("the failure is recorded");
    assert_eq!(record.state, ActionState::Failed);
    assert!(
        record
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("locate failed"),
        "got: {:?}",
        record.reason
    );

    // Post-state missing the expected marker: failed, never "executed".
    let runner = Arc::new(
        ScriptedRunner::new()
            .respond("list-apps", APP_LIST)
            .respond("get-app-state", FINDER_STATE)
            .respond_after_action("list-apps", r#"{"apps":[]}"#),
    );
    let engine = ComputerEngine::with_runner(&store, Box::new(SharedRunner(runner)));
    let record = engine
        .execute(&authority, &actor, &task, &spec("Finder", false))
        .expect("recorded");
    assert_eq!(record.state, ActionState::Failed);
    assert!(
        record
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("recorded as failed"),
        "got: {:?}",
        record.reason
    );

    // Verification axis two: the action's OWN output lacking the expected
    // observation also fails, even when the world state looks unchanged —
    // a tautological locator check can no longer pass alone (QA finding).
    let runner = Arc::new(
        ScriptedRunner::new()
            .respond("list-apps", APP_LIST)
            .respond("get-app-state", r#"{"note":"no observation here"}"#),
    );
    let engine = ComputerEngine::with_runner(&store, Box::new(SharedRunner(runner)));
    let record = engine
        .execute(&authority, &actor, &task, &spec("Finder", false))
        .expect("recorded");
    assert_eq!(record.state, ActionState::Failed);
    assert!(
        record
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("own output does not show"),
        "got: {:?}",
        record.reason
    );
}

/// Acceptance: "任务授权不被扩展为全电脑无限权" — without a grant nothing
/// runs; a grant scoped to another task is refused (cross-task); the
/// matching task grant lets the action through with full evidence.
#[test]
fn actions_are_task_scoped_and_grant_gated() {
    let store = store();
    let authority = AuthorityEngine::new(&store);
    let task = TaskId::new();
    let other = TaskId::new();
    let runner = Arc::new(
        ScriptedRunner::new()
            .respond("list-apps", APP_LIST)
            .respond("get-app-state", FINDER_STATE),
    );
    let engine = ComputerEngine::with_runner(&store, Box::new(SharedRunner(Arc::clone(&runner))));

    // No grant: refused, nothing executed, denial logged.
    let bare = Actor::Member {
        member: MemberId::new(),
        grant: None,
    };
    let record = engine
        .execute(&authority, &bare, &task, &spec("Finder", false))
        .expect("refusal recorded");
    assert_eq!(record.state, ActionState::Refused);
    assert_eq!(runner.action_runs(), 0);
    assert!(
        authority
            .recent_denials(10)
            .expect("denials")
            .iter()
            .any(|(_, action, _, _)| action == "computer_input"),
        "the refusal is in the audit log"
    );

    // A grant for a different task does not become a machine-wide grant.
    let foreign_actor = actor_with_grant(&authority, Some(&other), GrantMode::ActAutonomously);
    let record = engine
        .execute(&authority, &foreign_actor, &task, &spec("Finder", false))
        .expect("recorded");
    assert_eq!(record.state, ActionState::Refused);
    assert_eq!(
        runner.action_runs(),
        0,
        "cross-task is refused before acting"
    );

    // The matching task-scoped grant: locate → act → verify with evidence.
    let actor = actor_with_grant(&authority, Some(&task), GrantMode::ActWithApproval);
    let record = engine
        .execute(&authority, &actor, &task, &spec("Finder", false))
        .expect("executed");
    assert_eq!(record.state, ActionState::Executed);
    assert!(record.pre_state.expect("pre").contains("Finder"));
    assert!(record.post_state.expect("post").contains("Finder"));
    assert!(
        record
            .action_output
            .expect("action output")
            .contains("windows"),
        "the action's own observation is kept as evidence"
    );
    assert!(runner.action_runs() >= 1, "the action really ran");

    let history = engine.actions_for_task(&task).expect("history");
    assert_eq!(history.len(), 3, "refusals are evidence too");
}

/// Acceptance: "全局键鼠/焦点任务不能交叉破坏；独立 API/浏览器context不
/// 因此全串行" — two office processes (here: two connections sharing one
/// coordinator) cannot hold the foreground lane at once; a foreground
/// action waits its turn; an independent read context never waits.
#[test]
fn foreground_lane_serializes_but_reads_run_free() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let task_a = TaskId::new();
    let task_b = TaskId::new();

    // The office store exists once; grants are visible to both threads.
    {
        let store = Store::open(&db, &frozen()).expect("store");
        let authority = AuthorityEngine::new(&store);
        actor_with_grant(&authority, Some(&task_a), GrantMode::ActAutonomously);
        actor_with_grant(&authority, Some(&task_b), GrantMode::ActAutonomously);
    }

    // Task B: a foreground action whose tool call is slow — it holds the
    // lane for a while. Its own CONNECTION to the same file shares the
    // lease row, exactly like two separate `viva` processes would.
    let task_for_b = task_b.clone();
    let db_for_b = db.clone();
    let handle = std::thread::spawn(move || {
        let store = Store::open(&db_for_b, &frozen()).expect("store b");
        let authority = AuthorityEngine::new(&store);
        let member = MemberId::new();
        let grant = authority
            .issue_root_grant(
                Some(member.clone()),
                Some(task_for_b.clone()),
                vec!["computer_input".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .expect("grant b");
        let actor = Actor::Member {
            member,
            grant: Some(grant.grant_id),
        };
        let runner = ScriptedRunner::new()
            .respond("list-apps", APP_LIST)
            .respond("get-app-state", FINDER_STATE)
            .with_action_delay(std::time::Duration::from_millis(250));
        let engine = ComputerEngine::with_runner(&store, Box::new(SharedRunner(Arc::new(runner))));
        let holder = engine.holder().expect("holder");
        assert!(holder.is_none(), "the lane starts free");
        engine
            .execute(&authority, &actor, &task_for_b, &spec("Finder", true))
            .expect("b executed")
    });

    // While B is mid-action, the lane is B's — visible from A's own
    // separate connection.
    std::thread::sleep(std::time::Duration::from_millis(80));
    {
        let store = Store::open(&db, &frozen()).expect("store check");
        let coordinator = ForegroundCoordinator::new(&store);
        assert_eq!(
            coordinator.holder().expect("holder").as_deref(),
            Some(task_b.as_str()),
            "B holds the foreground lane while its action runs (cross-connection)"
        );
    }

    // A's independent read context does not wait for the lane.
    {
        let store = Store::open(&db, &frozen()).expect("store a");
        let authority = AuthorityEngine::new(&store);
        let member = MemberId::new();
        let grant = authority
            .issue_root_grant(
                Some(member.clone()),
                Some(task_a.clone()),
                vec!["computer_input".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .expect("grant a");
        let actor = Actor::Member {
            member,
            grant: Some(grant.grant_id),
        };
        let runner = ScriptedRunner::new()
            .respond("list-apps", APP_LIST)
            .respond("get-app-state", FINDER_STATE);
        let engine = ComputerEngine::with_runner(&store, Box::new(SharedRunner(Arc::new(runner))));
        let started = std::time::Instant::now();
        let record = engine
            .execute(&authority, &actor, &task_a, &spec("Finder", false))
            .expect("read ran without waiting for the lane");
        assert_eq!(record.state, ActionState::Executed);
        assert!(
            !record.foreground,
            "the read context took no foreground lane"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "the read context was not serialized behind B's foreground work"
        );

        // A's foreground task must wait: B still holds the lane when A
        // starts, so A's action finishes only after B releases.
        let started = std::time::Instant::now();
        let record = engine
            .execute(&authority, &actor, &task_a, &spec("Finder", true))
            .expect("a eventually got the lane");
        assert_eq!(record.state, ActionState::Executed);
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(100),
            "the foreground action really waited for the lane"
        );
        let coordinator = ForegroundCoordinator::new(&store);
        assert_eq!(
            coordinator.holder().expect("holder"),
            None,
            "the lane frees afterwards"
        );
    }

    let record_b = handle.join().expect("b joins");
    assert_eq!(record_b.state, ActionState::Executed);
}

/// Acceptance: "核查当前可调用工具与权限" — the audit probes the reused
/// tools and records availability honestly; a failing probe is
/// unavailable, never assumed. The shipped smoke specs stay read-only and
/// cover one browser and one native target.
#[test]
fn audit_records_what_it_could_really_probe() {
    let store = store();

    // orca probes fine; the other tool is missing on this fake host.
    let runner = ScriptedRunner::new().respond("capabilities", "orca-computer-use-macos");
    let engine = ComputerEngine::with_runner(&store, Box::new(SharedRunner(Arc::new(runner))));
    let reports = engine.audit().expect("audit");
    assert_eq!(reports.len(), AUDITED_TOOLS.len());

    let orca = engine
        .capability("orca-computer")
        .expect("row")
        .expect("recorded");
    assert!(orca.0, "orca-computer present");
    assert_eq!(orca.2, "available");
    assert!(orca.1.contains("orca-computer-use-macos"));

    let missing = engine
        .capability("osascript")
        .expect("row")
        .expect("recorded");
    assert!(!missing.0, "the missing tool is recorded absent");
    assert_eq!(missing.2, "unavailable");

    let specs = smoke_specs();
    assert_eq!(specs.len(), 2);
    assert!(
        specs.iter().all(|(_, s)| !s.foreground),
        "smoke is read-only"
    );
    assert!(
        specs.iter().any(|(_, s)| s.target.contains("Chrome")),
        "one browser task"
    );
    assert!(
        specs.iter().any(|(_, s)| s.target == "Finder"),
        "one native app task"
    );
}

/// Wraps a scripted runner so an Arc passes through the engine seam.
struct SharedRunner(Arc<ScriptedRunner>);

impl ToolRunner for SharedRunner {
    fn run(&self, program: &str, argv: &[String]) -> Result<String, ToolFailure> {
        self.0.run(program, argv)
    }
}

/// QA N4: a computer_input grant WITHOUT a task scope is refused even
/// though the authority engine's cross-task check passes NULL-scope grants
/// through — an unscoped input grant is not a whole-machine license.
#[test]
fn unscoped_computer_input_grants_are_refused() {
    let store = store();
    let authority = AuthorityEngine::new(&store);
    let task = TaskId::new();
    let runner = Arc::new(
        ScriptedRunner::new()
            .respond("list-apps", APP_LIST)
            .respond("get-app-state", FINDER_STATE),
    );
    let engine = ComputerEngine::with_runner(&store, Box::new(SharedRunner(Arc::clone(&runner))));

    let member = MemberId::new();
    let unscoped = authority
        .issue_root_grant(
            Some(member.clone()),
            None,
            vec!["computer_input".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .expect("unscoped grant");
    let actor = Actor::Member {
        member,
        grant: Some(unscoped.grant_id),
    };
    let record = engine
        .execute(&authority, &actor, &task, &spec("Finder", false))
        .expect("refusal recorded");
    assert_eq!(record.state, ActionState::Refused);
    assert!(
        record
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("whole-machine license"),
        "got: {:?}",
        record.reason
    );
    assert_eq!(runner.action_runs(), 0, "no action ran");
}

/// QA N3: a lease row left by a DEAD process reconciles itself on the
/// next acquire — a crash cannot wedge the input lane until a manual
/// DELETE.
#[test]
fn stale_lease_from_a_dead_process_self_heals() {
    let store = store();
    let coordinator = ForegroundCoordinator::new(&store);
    store
        .connection()
        .execute(
            "INSERT INTO foreground_leases(task_id, host_pid, acquired_at)
             VALUES ('ghost-task', ?1, '2020-01-01T00:00:00Z')",
            [65543], // effectively never a live pid of ours
        )
        .expect("seed stale lease");
    assert_eq!(
        coordinator.holder().expect("holder").as_deref(),
        Some("ghost-task"),
        "the stale row is visible before reconciliation"
    );

    // A live task acquires: the dead holder's row is removed first.
    let lease = coordinator
        .acquire_foreground("task-live")
        .expect("the lane self-heals");
    assert_eq!(lease.task_id, "task-live");
    assert_eq!(
        coordinator.holder().expect("holder").as_deref(),
        Some("task-live")
    );

    // A row held by THIS live process is respected (not reconciled away).
    drop(lease);
    let _lease2 = coordinator
        .acquire_foreground("task-live2")
        .expect("acquire");
    let other = coordinator.try_acquire_foreground("task-b");
    assert!(
        other.expect("try").is_none(),
        "a live holder keeps the lane"
    );
}
