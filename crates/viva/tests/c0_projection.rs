//! Issue #114: project one member's soul, principles, knowledge and skills
//! into a directory Pi and Claude Code both read, without touching the
//! work directory's instruction files.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use std::str::FromStr as _;

use viva::foundation::ids::{ExecutionId, MemberId, TaskId};
use viva::foundation::records::AttributionSnapshot;
use viva::foundation::store::Store;
use viva::harness::claude_code::{ClaudeCodeHarness, ClaudeProjection};
use viva::harness::pi::PiHarness;
use viva::knowledge::{ExternalState, KnowledgeRegistry, Scope, SkillSetService};
use viva::members::{MemberBinding, MemberRegistry};
use viva::projection::{self, MAX_PROJECTED_SKILLS, SoulEditor};
use viva::tasks::{ExecutionStart, TaskRegistry};

fn run_viva(home: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .env_remove("VIVA_MEMBER_ID")
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

fn member(store: &Store, name: &str) -> MemberId {
    let record = MemberRegistry::new(store).register(name).expect("member");
    MemberRegistry::new(store)
        .set_binding(&MemberBinding {
            member_id: record.member_id.clone(),
            role: "contributor".into(),
            model_binding: "model-a".into(),
            tools: vec!["pi".into()],
            updated_at: viva::foundation::ids::utc_now(),
        })
        .expect("binding");
    record.member_id
}

fn execution(store: &Store, member_id: &MemberId, goal: &str) -> ExecutionId {
    let task = TaskRegistry::new(store)
        .create_task(goal, vec![], Some(member_id.clone()), None, None)
        .expect("task");
    let attribution =
        AttributionSnapshot::capture(member_id.clone(), "contributor", "model-a", "pi");
    let started = TaskRegistry::new(store)
        .begin_execution(
            &task.task_id,
            &attribution,
            format!("c0-{}", ExecutionId::new().as_str()),
        )
        .expect("begin");
    match started {
        ExecutionStart::Created(intent) | ExecutionStart::Replayed(intent) => {
            intent.execution_id.expect("execution id")
        }
    }
}

fn write_skill(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).expect("skill file");
    path.canonicalize().expect("canonical skill")
}

fn register_skill(store: &Store, dir: &Path, title: &str, body: &str) -> String {
    KnowledgeRegistry::new(store)
        .register_skill(
            title,
            write_skill(dir, &format!("{title}.md"), body),
            "c0 test",
            None,
            vec![],
        )
        .expect("register skill")
        .skill_id
}

fn soul(duties: &str, boundaries: &str, natal: &[&str]) -> String {
    let mut text = format!(
        "## 职责\n{duties}\n\n## 说话方式\n- short\n\n## 边界\n{boundaries}\n\n## 本命技能\n"
    );
    for id in natal {
        text.push_str("- ");
        text.push_str(id);
        text.push('\n');
    }
    text
}

fn project_cli(
    home: &Path,
    member_id: &MemberId,
    execution_id: &ExecutionId,
    harness: &str,
    cwd: &Path,
    state: Option<&str>,
) -> (bool, String, String) {
    let mut args = vec![
        "project",
        "--member",
        member_id.as_str(),
        "--execution",
        execution_id.as_str(),
        "--harness",
        harness,
        "--cwd",
        cwd.to_str().expect("cwd utf8"),
    ];
    if let Some(state) = state {
        args.push("--state");
        args.push(state);
    }
    run_viva(home, &args)
}

#[test]
fn empty_skill_set_projects_soul_and_no_principles() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let member_id = member(&store, "Empty");
    let execution_id = execution(&store, &member_id, "write the empty-set note");
    let soul_file = work.path().join("soul-src.md");
    fs::write(
        &soul_file,
        soul("- keep the work in scope", "- do not merge", &[]),
    )
    .unwrap();
    let (ok, _stdout, stderr) = run_viva(
        home.path(),
        &[
            "member",
            "soul",
            "set",
            "--member",
            member_id.as_str(),
            "--file",
            soul_file.to_str().unwrap(),
        ],
    );
    assert!(ok, "soul set: {stderr}");
    TaskRegistry::new(&store)
        .generate_brief(&task_id_of(&store, &execution_id))
        .unwrap();
    drop(store);
    let (ok, stdout, stderr) = project_cli(
        home.path(),
        &member_id,
        &execution_id,
        "pi",
        work.path(),
        None,
    );
    assert!(ok, "project: {stderr}\n{stdout}");
    let manifest: serde_json::Value = serde_json::from_str(&stdout).expect("manifest json");
    assert!(manifest["skills"].as_array().unwrap().is_empty());
    assert!(manifest["principles"].as_array().unwrap().is_empty());
    assert_eq!(manifest["include_personal"], false);
    assert_eq!(manifest["dialog_state"], "unconfirmed");
    assert_eq!(manifest["task_brief"]["goal"], "write the empty-set note");
    let dir = PathBuf::from(manifest["projection_dir"].as_str().unwrap());
    let principles = fs::read_to_string(dir.join("principles.json")).unwrap();
    assert_eq!(principles.trim(), "[]");
    assert!(
        fs::symlink_metadata(dir.join("soul.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn natal_skills_over_ten_fail_before_any_projection_file() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let member_id = member(&store, "Over");
    let execution_id = execution(&store, &member_id, "should not project");
    let ids: Vec<String> = (0..=MAX_PROJECTED_SKILLS)
        .map(|n| format!("skill-{n}"))
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    projection::write_soul(
        home.path(),
        &member_id,
        &soul("- duty", "- boundary", &refs),
        SoulEditor::Owner,
    )
    .unwrap();
    drop(store);
    let (ok, _stdout, stderr) = project_cli(
        home.path(),
        &member_id,
        &execution_id,
        "claude-code",
        work.path(),
        None,
    );
    assert!(!ok, "over-limit projection must fail");
    assert!(stderr.contains("没有开始"), "{stderr}");
    assert!(stderr.contains("10"), "{stderr}");
    assert!(!projection::projection_dir(home.path(), &member_id, &execution_id).exists());
}

#[test]
fn missing_member_does_not_write_a_projection() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    init_home(home.path());
    let missing = MemberId::new();
    let execution_id = ExecutionId::new();
    let (ok, _stdout, stderr) = project_cli(
        home.path(),
        &missing,
        &execution_id,
        "pi",
        work.path(),
        None,
    );
    assert!(!ok, "missing member must fail");
    assert!(stderr.contains("member"), "{stderr}");
    assert!(
        !home
            .path()
            .join("projections")
            .join(missing.as_str())
            .exists()
    );
}

#[test]
fn a_then_b_share_a_workdir_and_leave_no_residue() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let skills = tempfile::tempdir().unwrap();
    init_home(home.path());
    fs::write(work.path().join("AGENTS.md"), "agents-bytes-v1\n").unwrap();
    fs::write(work.path().join("CLAUDE.md"), "claude-bytes-v1\n").unwrap();
    let agents_before = fs::read(work.path().join("AGENTS.md")).unwrap();
    let claude_before = fs::read(work.path().join("CLAUDE.md")).unwrap();
    let names_before = dir_names(work.path());

    let store = open_store(home.path());
    let a = member(&store, "Member A");
    let b = member(&store, "Member B");
    let skill_a = register_skill(&store, skills.path(), "alpha-skill", "ALPHA-SKILL-BODY");
    let skill_b = register_skill(&store, skills.path(), "beta-skill", "BETA-SKILL-BODY");
    let sets = SkillSetService::new(&store);
    sets.claim(&a, &skill_a, None).unwrap();
    sets.claim(&b, &skill_b, None).unwrap();
    projection::write_soul(
        home.path(),
        &a,
        &soul("- ALPHA-DUTY-ONLY", "- ALPHA-BOUNDARY-ONLY", &[&skill_a]),
        SoulEditor::Owner,
    )
    .unwrap();
    projection::write_soul(
        home.path(),
        &b,
        &soul("- BETA-DUTY-ONLY", "- BETA-BOUNDARY-ONLY", &[&skill_b]),
        SoulEditor::Owner,
    )
    .unwrap();
    let exec_a = execution(&store, &a, "alpha task ranktoken");
    let exec_b = execution(&store, &b, "beta task ranktoken");
    drop(store);

    let (ok, out_a, stderr) = project_cli(home.path(), &a, &exec_a, "pi", work.path(), None);
    assert!(ok, "project A: {stderr}");
    let (ok, out_b, stderr) =
        project_cli(home.path(), &b, &exec_b, "claude-code", work.path(), None);
    assert!(ok, "project B: {stderr}");
    let manifest_a: serde_json::Value = serde_json::from_str(&out_a).unwrap();
    let manifest_b: serde_json::Value = serde_json::from_str(&out_b).unwrap();
    let dir_a = PathBuf::from(manifest_a["projection_dir"].as_str().unwrap());
    let dir_b = PathBuf::from(manifest_b["projection_dir"].as_str().unwrap());
    assert_ne!(dir_a, dir_b);
    let text_b = walk_text(&dir_b);
    assert!(text_b.contains("BETA-DUTY-ONLY"), "{text_b}");
    assert!(text_b.contains("BETA-SKILL-BODY"), "{text_b}");
    assert!(!text_b.contains("ALPHA-DUTY-ONLY"), "{text_b}");
    assert!(!text_b.contains("ALPHA-BOUNDARY-ONLY"), "{text_b}");
    assert!(!text_b.contains("ALPHA-SKILL-BODY"), "{text_b}");
    assert!(!text_b.contains(&skill_a), "{text_b}");
    assert_eq!(
        fs::read(work.path().join("AGENTS.md")).unwrap(),
        agents_before
    );
    assert_eq!(
        fs::read(work.path().join("CLAUDE.md")).unwrap(),
        claude_before
    );
    assert_eq!(dir_names(work.path()), names_before);
    assert!(!dir_b.join("AGENTS.md").exists());
    assert!(!dir_b.join("CLAUDE.md").exists());
}

#[test]
fn unconfirmed_state_omits_personal_knowledge() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let bodies = tempfile::tempdir().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let member_id = member(&store, "Scoped");
    let personal = bodies.path().join("personal.md");
    let team = bodies.path().join("team.md");
    fs::write(&personal, "secretpersonal fact for this member").unwrap();
    fs::write(&team, "visibleteam fact for the team").unwrap();
    let knowledge = KnowledgeRegistry::new(&store);
    knowledge
        .record(
            "personal note",
            Scope::Personal,
            personal.canonicalize().unwrap(),
            "c0 test",
            Some(member_id.clone()),
            None,
            vec!["secretpersonal".into()],
            None,
            ExternalState::None,
        )
        .unwrap();
    knowledge
        .record(
            "team note",
            Scope::Team,
            team.canonicalize().unwrap(),
            "c0 test",
            None,
            None,
            vec!["visibleteam".into()],
            None,
            ExternalState::None,
        )
        .unwrap();
    projection::write_soul(
        home.path(),
        &member_id,
        &soul("- stay on the task", "- do not merge", &[]),
        SoulEditor::Owner,
    )
    .unwrap();
    let restricted = execution(&store, &member_id, "inspect visibleteam and secretpersonal");
    let explicit = execution(&store, &member_id, "inspect visibleteam and secretpersonal");
    drop(store);

    let (ok, stdout, stderr) = project_cli(
        home.path(),
        &member_id,
        &restricted,
        "pi",
        work.path(),
        None,
    );
    assert!(ok, "{stderr}");
    let restricted_text = walk_text(&dir_of(&stdout));
    assert!(restricted_text.contains("visibleteam"), "{restricted_text}");
    assert!(
        !restricted_text.contains("secretpersonal"),
        "{restricted_text}"
    );

    let (ok, stdout, stderr) = project_cli(
        home.path(),
        &member_id,
        &explicit,
        "pi",
        work.path(),
        Some("personal"),
    );
    assert!(ok, "{stderr}");
    let personal_text = walk_text(&dir_of(&stdout));
    assert!(personal_text.contains("secretpersonal"), "{personal_text}");
    assert!(personal_text.contains("visibleteam"), "{personal_text}");
}

#[test]
fn natal_skills_are_kept_and_the_rest_stay_within_ten() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let member_id = member(&store, "Capped");
    let sets = SkillSetService::new(&store);
    let mut natal = Vec::new();
    for name in ["natal-one", "natal-two"] {
        let id = register_skill(&store, files.path(), name, "ranktoken natal");
        natal.push(id);
    }
    let mut low = Vec::new();
    let mut high = Vec::new();
    for index in 0..10 {
        let id = register_skill(
            &store,
            files.path(),
            &format!("extra-{index}"),
            "ranktoken extra",
        );
        sets.claim(&member_id, &id, None).unwrap();
        if index < 8 {
            high.push(id);
        } else {
            low.push(id);
        }
    }
    let bump = execution(&store, &member_id, "bump usage");
    for id in &high {
        sets.get_body(&member_id, id, &bump, 1.0).unwrap();
    }
    projection::write_soul(
        home.path(),
        &member_id,
        &soul(
            "- keep natal skills",
            "- do not drop them",
            &[&natal[0], &natal[1]],
        ),
        SoulEditor::Owner,
    )
    .unwrap();
    let execution_id = execution(&store, &member_id, "ranktoken the session");
    drop(store);

    let (ok, stdout, stderr) = project_cli(
        home.path(),
        &member_id,
        &execution_id,
        "pi",
        work.path(),
        None,
    );
    assert!(ok, "{stderr}");
    let manifest: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let skills = manifest["skills"].as_array().unwrap();
    assert_eq!(skills.len(), MAX_PROJECTED_SKILLS);
    let ids: Vec<&str> = skills
        .iter()
        .map(|skill| skill["skill_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&natal[0].as_str()));
    assert!(ids.contains(&natal[1].as_str()));
    assert_eq!(
        skills
            .iter()
            .filter(|skill| skill["natal"].as_bool() == Some(true))
            .count(),
        2
    );
    for id in &high {
        assert!(ids.contains(&id.as_str()), "missing high-usage {id}");
    }
    for id in &low {
        assert!(!ids.contains(&id.as_str()), "low-usage {id} was projected");
    }
}

#[test]
fn engine_or_harness_change_keeps_soul_and_knowledge() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let bodies = tempfile::tempdir().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let member_id = member(&store, "Stable");
    let body = bodies.path().join("kept.md");
    fs::write(&body, "kept-knowledge-body").unwrap();
    let item = KnowledgeRegistry::new(&store)
        .record(
            "kept knowledge",
            Scope::Team,
            body.canonicalize().unwrap(),
            "c0 test",
            None,
            None,
            vec!["keptknowledge".into()],
            None,
            ExternalState::None,
        )
        .unwrap();
    projection::write_soul(
        home.path(),
        &member_id,
        &soul("- DUTY-STABLE", "- BOUNDARY-STABLE", &[]),
        SoulEditor::Owner,
    )
    .unwrap();
    let soul_file = projection::soul_path(home.path(), &member_id);
    let before = fs::read(&soul_file).unwrap();
    let count_before: i64 = store
        .connection()
        .query_row("SELECT COUNT(1) FROM knowledge_items", [], |row| row.get(0))
        .unwrap();
    let first = execution(&store, &member_id, "mention keptknowledge once");
    let second = execution(&store, &member_id, "mention keptknowledge twice");
    MemberRegistry::new(&store)
        .set_binding(&MemberBinding {
            member_id: member_id.clone(),
            role: "contributor".into(),
            model_binding: "model-b".into(),
            tools: vec!["claude".into()],
            updated_at: viva::foundation::ids::utc_now(),
        })
        .unwrap();
    drop(store);

    let (ok, out_pi, stderr) =
        project_cli(home.path(), &member_id, &first, "pi", work.path(), None);
    assert!(ok, "{stderr}");
    let (ok, out_claude, stderr) = project_cli(
        home.path(),
        &member_id,
        &second,
        "claude-code",
        work.path(),
        None,
    );
    assert!(ok, "{stderr}");
    assert_eq!(fs::read(&soul_file).unwrap(), before);
    let store = open_store(home.path());
    let count_after: i64 = store
        .connection()
        .query_row("SELECT COUNT(1) FROM knowledge_items", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count_before, count_after);
    assert!(
        KnowledgeRegistry::new(&store)
            .get_item(&item.item_id)
            .unwrap()
            .is_some()
    );

    let manifest: serde_json::Value = serde_json::from_str(&out_pi).unwrap();
    let soul_ino = fs::metadata(&soul_file).unwrap().ino();
    let linked = PathBuf::from(manifest["projection_dir"].as_str().unwrap()).join("soul.md");
    assert!(
        fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::metadata(&linked).unwrap().ino(), soul_ino);

    let ext = work.path().join("viva.ts");
    fs::write(&ext, "// extension\n").unwrap();
    let member_record = MemberRegistry::new(&store).require(&member_id).unwrap();
    let task = TaskRegistry::new(&store)
        .get_task(&task_id_of(&store, &first))
        .unwrap()
        .unwrap();
    let pi = PiHarness {
        pi_binary: "pi".into(),
        extension_path: ext.canonicalize().unwrap(),
    };
    let projected: projection::ProjectionManifest = serde_json::from_str(&out_pi).unwrap();
    let pi_spec = projection::inject_projection_env(
        pi.launch(&member_record, None, Some(&task), work.path(), None)
            .unwrap(),
        &projected,
    );
    let claude_manifest: projection::ProjectionManifest =
        serde_json::from_str(&out_claude).unwrap();
    let claude_spec = ClaudeCodeHarness {
        binary: "claude".into(),
    }
    .launch(
        &member_record,
        None,
        Some(&task),
        work.path(),
        &ClaudeProjection {
            soul_path: &claude_manifest.soul_path,
            projection_dir: &claude_manifest.projection_dir,
            execution_id: &claude_manifest.execution_id,
        },
    )
    .unwrap();
    let pi_soul = env_value(&pi_spec.env, "VIVA_SOUL_PATH");
    let claude_soul = env_value(&claude_spec.env, "VIVA_SOUL_PATH");
    assert_eq!(pi_soul, claude_soul);
    assert_eq!(pi_soul, soul_file.display().to_string());
    assert_ne!(
        env_value(&pi_spec.env, "VIVA_PROJECTION_DIR"),
        env_value(&claude_spec.env, "VIVA_PROJECTION_DIR"),
        "different executions stay isolated"
    );
    let flag = claude_spec
        .argv
        .iter()
        .position(|arg| arg == "--append-system-prompt-file")
        .expect("claude reads soul from a file");
    assert_eq!(claude_spec.argv[flag + 1], soul_file.display().to_string());
    assert!(!pi_spec.argv.iter().any(|arg| arg.ends_with("AGENTS.md")));
    assert!(
        !claude_spec
            .argv
            .iter()
            .any(|arg| arg.ends_with("CLAUDE.md"))
    );
}

#[test]
fn member_session_cannot_replace_soul() {
    let home = tempfile::tempdir().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let member_id = member(&store, "Locked");
    drop(store);
    let source = home.path().join("soul-src.md");
    fs::write(&source, soul("- original duty", "- original boundary", &[])).unwrap();
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "member",
            "soul",
            "set",
            "--member",
            member_id.as_str(),
            "--file",
            source.to_str().unwrap(),
        ],
    );
    assert!(ok, "{stderr}");
    let before = fs::read(projection::soul_path(home.path(), &member_id)).unwrap();
    let replacement = home.path().join("soul-new.md");
    fs::write(&replacement, soul("- hijack", "- hijack", &[])).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args([
            "member",
            "soul",
            "set",
            "--member",
            member_id.as_str(),
            "--file",
            replacement.to_str().unwrap(),
        ])
        .env("VIVA_HOME", home.path())
        .env("VIVA_MEMBER_ID", member_id.as_str())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only modified by Haisu"), "{stderr}");
    assert_eq!(
        fs::read(projection::soul_path(home.path(), &member_id)).unwrap(),
        before
    );
}

fn task_id_of(store: &Store, execution_id: &ExecutionId) -> TaskId {
    let raw: String = store
        .connection()
        .query_row(
            "SELECT task_id FROM executions WHERE execution_id = ?1",
            [execution_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    TaskId::from_str(&raw).unwrap()
}

fn dir_of(stdout: &str) -> PathBuf {
    let manifest: serde_json::Value = serde_json::from_str(stdout).unwrap();
    PathBuf::from(manifest["projection_dir"].as_str().unwrap())
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut names = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn walk_text(dir: &Path) -> String {
    let mut out = String::new();
    walk_into(dir, &mut out);
    out
}

fn walk_into(dir: &Path, out: &mut String) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_into(&path, out);
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            out.push_str(&text);
            out.push('\n');
        }
    }
}

fn env_value(env: &[(String, String)], key: &str) -> String {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}
