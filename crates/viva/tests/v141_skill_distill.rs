//! Issue #141: distill team skills from successful executions.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use viva::foundation::ids::{ExecutionId, MemberId};
use viva::foundation::records::{AttributionSnapshot, ExecutionRecord, insert_execution};
use viva::foundation::store::Store;
use viva::knowledge::Scope;
use viva::knowledge::{KnowledgeRegistry, SkillSetService};
use viva::skills::SkillBodyStore;
use viva::tasks::{ResultSource, TaskRegistry};

fn run_viva(home: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .output()
        .expect("spawn viva");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn init_home(home: &Path) {
    let (ok, _, stderr) = run_viva(home, &["init"]);
    assert!(ok, "init: {stderr}");
}

fn open_store(home: &Path) -> Store {
    let db = viva::foundation::paths::database_path(home);
    Store::open(&db, viva::server::migrations()).expect("store")
}

struct CompletedRun {
    member: MemberId,
    execution_id: ExecutionId,
}

fn seed_completed_execution(
    store: &Store,
    member: MemberId,
    goal: &str,
    step_kinds: &[&str],
) -> CompletedRun {
    let tasks = TaskRegistry::new(store);
    let task = tasks
        .create_task(goal, vec![], Some(member.clone()), None, None)
        .expect("task");
    let mut execution = ExecutionRecord::start(
        task.task_id.clone(),
        AttributionSnapshot::capture(member.clone(), "developer", "test-model", "test-tool"),
    );
    execution.complete("acceptance evidence for distill");
    insert_execution(store, &execution).expect("execution");

    for (idx, kind) in step_kinds.iter().enumerate() {
        tasks
            .record_result(
                &task.task_id,
                Some(execution.execution_id.clone()),
                ResultSource::Process,
                *kind,
                format!("step-{idx}-{}", execution.execution_id.as_str()),
                serde_json::json!({"step": idx + 1}),
            )
            .expect("record")
            .expect("fresh");
    }

    CompletedRun {
        member,
        execution_id: execution.execution_id,
    }
}

#[test]
fn distill_creates_version_one_skill_in_member_set_with_source_execution() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let member = MemberId::new();
    let run = seed_completed_execution(
        &store,
        member,
        "Refactor the payment module safely",
        &["plan", "edit", "verify"],
    );

    let eid = run.execution_id.as_str();
    let (ok, stdout, stderr) = run_viva(home.path(), &["skill", "distill", eid]);
    assert!(ok, "distill: {stderr}");
    assert!(stdout.contains("created"), "stdout: {stdout}");

    let bodies = SkillBodyStore::new(&store);
    let skill_id = extract_skill_id(&stdout);
    let current = bodies.current_body(&skill_id).expect("get").expect("body");
    assert_eq!(current.skill.current_version, 1);
    assert_eq!(
        current.version.execution_id.as_ref(),
        Some(&run.execution_id)
    );

    let memberships = SkillSetService::new(&store)
        .list_member_skills(&run.member)
        .expect("list");
    assert_eq!(memberships.len(), 1);
    assert_eq!(memberships[0].skill_id, skill_id);

    let uses = SkillSetService::new(&store)
        .used_in_for_member_skill(&run.member, &skill_id)
        .expect("used_in");
    assert_eq!(uses.len(), 1);
    assert!((uses[0].score - 1.0).abs() < f64::EPSILON);
}

#[test]
fn second_read_in_same_execution_does_not_double_initial_score() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let run = seed_completed_execution(
        &store,
        MemberId::new(),
        "Document the API",
        &["a", "b", "c"],
    );
    let eid = run.execution_id.as_str();
    let (ok, stdout, stderr) = run_viva(home.path(), &["skill", "distill", eid]);
    assert!(ok, "distill: {stderr}");
    let skill_id = extract_skill_id(&stdout);
    let mid = run.member.as_str();

    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "get",
            "--member",
            mid,
            "--skill",
            &skill_id,
            "--execution",
            eid,
        ],
    );
    assert!(ok, "get after distill: {stderr}");

    let uses = SkillSetService::new(&store)
        .used_in_for_member_skill(&run.member, &skill_id)
        .expect("used_in");
    assert_eq!(
        uses.len(),
        1,
        "initial 1.0 must not double on same execution read"
    );
}

#[test]
fn later_execution_read_records_used_in() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let run = seed_completed_execution(
        &store,
        MemberId::new(),
        "Ship the fix",
        &["one", "two", "three"],
    );
    let eid = run.execution_id.as_str();
    let (ok, stdout, stderr) = run_viva(home.path(), &["skill", "distill", eid]);
    assert!(ok, "distill: {stderr}");
    let skill_id = extract_skill_id(&stdout);

    let later = ExecutionId::new();
    let mid = run.member.as_str();
    let lid = later.as_str();
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "get",
            "--member",
            mid,
            "--skill",
            &skill_id,
            "--execution",
            lid,
        ],
    );
    assert!(ok, "later get: {stderr}");
    assert!(stdout.contains("used_in"), "stdout: {stdout}");

    let uses = SkillSetService::new(&store)
        .used_in_for_member_skill(&run.member, &skill_id)
        .expect("used_in");
    assert_eq!(uses.len(), 2);
    assert!(uses.iter().any(|u| u.execution_id == later));
}

#[test]
fn create_without_execution_source_is_rejected_for_distill_path() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "create",
            "--name",
            "orphan",
            "--body",
            "no source",
            "--manual",
        ],
    );
    assert!(ok, "manual create still allowed for Haisu: {stderr}");

    let store = open_store(home.path());
    let run = seed_completed_execution(&store, MemberId::new(), "Only two steps", &["s1", "s2"]);
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &["skill", "distill", run.execution_id.as_str()],
    );
    assert!(ok, "distill returns not eligible: {stderr}");
    assert!(stdout.contains("not_eligible"), "stdout: {stdout}");
}

#[test]
fn repeated_task_type_trigger_works_with_two_successes() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let member = MemberId::new();
    let run1 = seed_completed_execution(&store, member.clone(), "Small fix A", &["only"]);
    let (ok, stdout, _) = run_viva(
        home.path(),
        &[
            "skill",
            "distill",
            run1.execution_id.as_str(),
            "--task-type",
            "hotfix",
        ],
    );
    assert!(ok);
    assert!(stdout.contains("not_eligible"));

    let run2 = seed_completed_execution(&store, member, "Small fix B", &["only"]);
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "distill",
            run2.execution_id.as_str(),
            "--task-type",
            "hotfix",
        ],
    );
    assert!(ok, "second hotfix distill: {stderr}");
    assert!(stdout.contains("created"), "stdout: {stdout}");
}

#[test]
fn distilled_skill_is_registered_as_skill_not_knowledge() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let run = seed_completed_execution(&store, MemberId::new(), "Build feature", &["x", "y", "z"]);
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &["skill", "distill", run.execution_id.as_str()],
    );
    assert!(ok, "distill: {stderr}");
    let skill_id = extract_skill_id(&stdout);
    let skill = KnowledgeRegistry::new(&store)
        .skill(&skill_id)
        .expect("lookup")
        .expect("registered");
    assert!(skill.entry_path.ends_with("SKILL.md"));
    let item = KnowledgeRegistry::new(&store)
        .get_item(&skill.item_id)
        .expect("item")
        .expect("exists");
    assert_eq!(item.scope, Scope::Skill);
}

fn extract_skill_id(stdout: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(stdout).expect("json");
    value
        .get("skill_id")
        .and_then(|v| v.as_str())
        .expect("skill_id in distill output")
        .to_string()
}
