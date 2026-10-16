//! Issue #139 acceptance: member skill sets, claim, restore, sync, overlap
//! hints, and `viva skill weekly` through the real binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use viva::foundation::ids::{ExecutionId, MemberId};
use viva::foundation::store::Store;
use viva::knowledge::{KnowledgeRegistry, SkillSetService};

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

fn write_skill(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write");
    path.canonicalize().expect("canonical")
}

fn register_team_skill(store: &Store, dir: &Path, title: &str, body: &str) -> String {
    let kr = KnowledgeRegistry::new(store);
    kr.register_skill(
        title,
        write_skill(dir, &format!("{title}.md"), body),
        "v139 test",
        None,
        vec![],
    )
    .expect("register")
    .skill_id
}

#[test]
fn sync_to_b_leaves_a_unchanged_and_get_records_used_in() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let skill_id = register_team_skill(
        &store,
        dir.path(),
        "pr-review",
        "review pull requests carefully",
    );
    let a = MemberId::new();
    let b = MemberId::new();
    let svc = SkillSetService::new(&store);
    assert!(svc.list_member_skills(&a).unwrap().is_empty());
    let exec = ExecutionId::new();
    let mid = b.as_str();
    let eid = exec.as_str();
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "sync",
            "--member",
            mid,
            "--skill",
            &skill_id,
            "--execution",
            eid,
        ],
    );
    assert!(ok, "sync b: {stderr}");
    assert!(svc.list_member_skills(&a).unwrap().is_empty());
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
            eid,
        ],
    );
    assert!(ok, "get: {stderr}");
    assert!(stdout.contains("used_in"), "stdout: {stdout}");
    let uses = svc.used_in_for_member_skill(&b, &skill_id).unwrap();
    assert_eq!(uses.len(), 1);
    assert!((uses[0].score - 1.0).abs() < f64::EPSILON);
}

#[test]
fn claim_adds_one_initial_use_and_second_read_in_same_execution_does_not() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let skill_id = register_team_skill(&store, dir.path(), "deploy", "deploy the service safely");
    let member = MemberId::new();
    let exec = ExecutionId::new();
    let mid = member.as_str();
    let eid = exec.as_str();
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "claim",
            "--member",
            mid,
            "--skill",
            &skill_id,
            "--execution",
            eid,
        ],
    );
    assert!(ok, "claim: {stderr}");
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
    assert!(ok, "get: {stderr}");
    let svc = SkillSetService::new(&store);
    let uses = svc.used_in_for_member_skill(&member, &skill_id).unwrap();
    assert_eq!(uses.len(), 1, "claim initial + same-exec read must dedupe");
}

#[test]
fn forgotten_skill_returns_via_restore_or_use_without_second_initial() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let skill_id = register_team_skill(&store, dir.path(), "lint", "run the linter");
    let member = MemberId::new();
    let svc = SkillSetService::new(&store);
    svc.claim(&member, &skill_id, None).expect("claim");
    svc.mark_forgotten(&member, &skill_id).expect("forget");
    assert!(
        KnowledgeRegistry::new(&store)
            .skill(&skill_id)
            .unwrap()
            .is_some()
    );
    let exec = ExecutionId::new();
    svc.get_body(&member, &skill_id, &exec, 0.25).expect("use");
    let uses = svc.used_in_for_member_skill(&member, &skill_id).unwrap();
    let initial = uses.iter().filter(|u| u.note == "initial_on_join").count();
    assert_eq!(initial, 1);
    assert!(
        uses.iter()
            .any(|u| u.note == "read_body" && (u.score - 0.25).abs() < f64::EPSILON)
    );

    let other = MemberId::new();
    let skill2 = register_team_skill(&store, dir.path(), "fmt", "format code");
    svc.claim(&other, &skill2, None).expect("claim other");
    svc.mark_forgotten(&other, &skill2).expect("forget other");
    svc.restore_manual(&other, &skill2).expect("manual restore");
    let uses2 = svc.used_in_for_member_skill(&other, &skill2).unwrap();
    assert_eq!(
        uses2.iter().filter(|u| u.note == "initial_on_join").count(),
        1
    );
}

#[test]
fn derived_sync_records_lineage_without_overwriting_source() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let source = register_team_skill(
        &store,
        dir.path(),
        "test-py",
        "pytest pytest pytest unique py",
    );
    let member = MemberId::new();
    let entry = write_skill(dir.path(), "test-rs.md", "cargo test cargo test unique rs");
    let derived = SkillSetService::new(&store)
        .sync_derived(
            &member,
            &source,
            "test-rs",
            entry.clone(),
            "rewritten for rust tools",
            None,
        )
        .expect("derived");
    let svc = SkillSetService::new(&store);
    assert_eq!(
        svc.skill_derived_from(&derived.skill_id).unwrap(),
        Some(source.clone())
    );
    let source_body = std::fs::read_to_string(
        KnowledgeRegistry::new(&store)
            .skill(&source)
            .unwrap()
            .unwrap()
            .entry_path,
    )
    .unwrap();
    assert!(source_body.contains("pytest"));
    let new_body = std::fs::read_to_string(entry).unwrap();
    assert!(new_body.contains("cargo test"));
}

#[test]
fn overlap_suggested_keeps_both_skills_in_set() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let first = register_team_skill(
        &store,
        dir.path(),
        "alpha-skill",
        "alpha bravo charlie delta echo foxtrot",
    );
    let member = MemberId::new();
    let svc = SkillSetService::new(&store).with_overlap_threshold(0.3);
    svc.claim(&member, &first, None).expect("first");
    let second = register_team_skill(
        &store,
        dir.path(),
        "alpha-copy",
        "alpha bravo charlie delta echo foxtrot golf",
    );
    svc.claim(&member, &second, None).expect("second");
    assert_eq!(svc.list_member_skills(&member).unwrap().len(), 2);
    let (ok, stdout, stderr) = run_viva(home.path(), &["skill", "weekly"]);
    assert!(ok, "weekly: {stderr}");
    assert!(
        stdout.contains("overlap_suggested"),
        "weekly should list overlap: {stdout}"
    );
}

#[test]
fn weekly_includes_events_after_downtime() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let skill_id = register_team_skill(&store, dir.path(), "idle", "idle skill body text");
    let member = MemberId::new();
    let (ok, _, stderr) = run_viva(home.path(), &["skill", "weekly"]);
    assert!(ok, "baseline weekly: {stderr}");
    let mid = member.as_str();
    let (ok, _, stderr) = run_viva(
        home.path(),
        &["skill", "claim", "--member", mid, "--skill", &skill_id],
    );
    assert!(ok, "claim after downtime: {stderr}");
    let (ok, stdout, stderr) = run_viva(home.path(), &["skill", "weekly"]);
    assert!(ok, "weekly after claim: {stderr}");
    assert!(stdout.contains("claimed"), "stdout: {stdout}");
    assert!(
        stdout.contains("尚未启用"),
        "forgetting section pending #119: {stdout}"
    );
}
