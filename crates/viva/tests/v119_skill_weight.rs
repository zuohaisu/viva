//! Issue #119 acceptance: per-member skill weight, decay and forgetting,
//! through the real `viva` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use viva::foundation::ids::{ExecutionId, MemberId, SessionId, TaskId};
use viva::foundation::records::{
    AttributionSnapshot, ExecutionRecord, ExecutionStatus, SessionKind, SessionRecord,
    insert_execution, insert_session,
};
use viva::foundation::store::Store;
use viva::knowledge::{KnowledgeRegistry, SkillSetService, SkillWeightService};

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

fn register_team_skill(store: &Store, dir: &Path, title: &str, body: &str) -> (String, PathBuf) {
    let path = write_skill(dir, &format!("{title}.md"), body);
    let skill = KnowledgeRegistry::new(store)
        .register_skill(title, path.clone(), "v119 test", None, vec![])
        .expect("register");
    (skill.skill_id, path)
}

fn claim(home: &Path, member: &str, skill_id: &str, execution: Option<&str>) {
    let mut args = vec!["skill", "claim", "--member", member, "--skill", skill_id];
    if let Some(execution) = execution {
        args.extend(["--execution", execution]);
    }
    let (ok, _, stderr) = run_viva(home, &args);
    assert!(ok, "claim {skill_id}: {stderr}");
}

fn weight_of(home: &Path, member: &str, skill_id: &str) -> serde_json::Value {
    let (ok, stdout, stderr) = run_viva(
        home,
        &["skill", "weight", "--member", member, "--skill", skill_id],
    );
    assert!(ok, "weight: {stderr}");
    serde_json::from_str(&stdout).unwrap_or_else(|err| panic!("json {err}: {stdout}"))
}

fn write_config(home: &Path, body: &str) {
    std::fs::write(home.join("config.toml"), body).expect("config");
}

fn execution(
    store: &Store,
    member: &MemberId,
    status: ExecutionStatus,
    session: Option<SessionId>,
) -> ExecutionId {
    let mut record = ExecutionRecord::start(
        TaskId::new(),
        AttributionSnapshot::capture(member.clone(), "dev", "model", "pi"),
    );
    if let Some(session) = session {
        record = record.with_session(session);
    }
    match status {
        ExecutionStatus::Completed => record.complete("done"),
        ExecutionStatus::Failed | ExecutionStatus::Stopped | ExecutionStatus::Running => {
            record.mark(status).expect("mark");
        }
        ExecutionStatus::Requested => {}
    }
    let id = record.execution_id.clone();
    insert_execution(store, &record).expect("insert execution");
    id
}

#[test]
fn projection_does_not_score_and_one_execution_scores_once() {
    let home = tempfile::TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let (skill_id, _) = register_team_skill(&store, dir.path(), "review", "review the change");
    let member = MemberId::new();
    let mid = member.as_str();
    let first = ExecutionId::new();
    claim(home.path(), mid, &skill_id, Some(first.as_str()));
    let sets = SkillSetService::new(&store);
    assert_eq!(
        sets.used_in_for_member_skill(&member, &skill_id)
            .unwrap()
            .len(),
        1
    );
    SkillWeightService::open(&store, home.path())
        .unwrap()
        .observe_projection(&member, &skill_id, &ExecutionId::new())
        .unwrap();
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "project",
            "--skill",
            &skill_id,
            "--out",
            dir.path().join("projected.md").to_str().unwrap(),
        ],
    );
    // Team-body projection is a different store; a miss must still leave weight alone.
    let _ = (ok, stderr);
    let after_project = weight_of(home.path(), mid, &skill_id);
    assert!((after_project["weight"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    assert_eq!(
        sets.used_in_for_member_skill(&member, &skill_id)
            .unwrap()
            .len(),
        1,
        "projection must not write used_in"
    );

    let read_exec = execution(&store, &member, ExecutionStatus::Completed, None);
    let read_twice = |exec: &ExecutionId| {
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
                exec.as_str(),
            ],
        );
        assert!(ok, "get: {stderr}");
    };
    read_twice(&read_exec);
    read_twice(&read_exec);
    let uses = sets.used_in_for_member_skill(&member, &skill_id).unwrap();
    assert_eq!(uses.len(), 2, "initial plus one read");
    assert!(
        uses.iter()
            .any(|row| row.execution_id == read_exec && (row.score - 1.0).abs() < 1e-9)
    );
    let view = weight_of(home.path(), mid, &skill_id);
    assert!((view["weight"].as_f64().unwrap() - 2.0).abs() < 1e-6);
}

#[test]
fn failure_is_quarter_success_is_one_and_correct_revokes() {
    let home = tempfile::TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let (skill_id, _) = register_team_skill(&store, dir.path(), "ship", "ship the service");
    let member = MemberId::new();
    let mid = member.as_str();
    claim(home.path(), mid, &skill_id, None);

    let failed = execution(&store, &member, ExecutionStatus::Failed, None);
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
            failed.as_str(),
        ],
    );
    assert!(ok, "failed get: {stderr}");
    let uses = SkillSetService::new(&store)
        .used_in_for_member_skill(&member, &skill_id)
        .unwrap();
    let failed_row = uses
        .iter()
        .find(|row| row.execution_id == failed)
        .expect("failed use");
    assert!((failed_row.score - 0.25).abs() < 1e-9);
    let view = weight_of(home.path(), mid, &skill_id);
    assert!((view["weight"].as_f64().unwrap() - 1.25).abs() < 1e-6);

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &["skill", "correct", failed.as_str(), &skill_id],
    );
    assert!(ok, "correct: {stderr} {stdout}");
    let corrected: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(corrected["review"], true);
    assert!((corrected["weight"].as_f64().unwrap() - 1.0).abs() < 1e-6);

    let stopped = execution(&store, &member, ExecutionStatus::Stopped, None);
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
            stopped.as_str(),
        ],
    );
    assert!(ok, "stopped get: {stderr}");
    let success = execution(&store, &member, ExecutionStatus::Completed, None);
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
            success.as_str(),
        ],
    );
    assert!(ok, "success get: {stderr}");
    let uses = SkillSetService::new(&store)
        .used_in_for_member_skill(&member, &skill_id)
        .unwrap();
    assert!(
        uses.iter()
            .any(|row| { row.execution_id == stopped && (row.score - 0.25).abs() < 1e-9 })
    );
    assert!(
        uses.iter()
            .any(|row| { row.execution_id == success && (row.score - 1.0).abs() < 1e-9 })
    );
}

#[test]
fn recovered_session_read_matches_cli_and_does_not_double_count() {
    let home = tempfile::TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let (skill_id, path) = register_team_skill(&store, dir.path(), "lint", "run the linter");
    let member = MemberId::new();
    let mid = member.as_str();
    claim(home.path(), mid, &skill_id, None);
    let exec = execution(&store, &member, ExecutionStatus::Completed, None);
    let transcript_path = dir.path().join("session.json");
    let body = format!(
        r#"{{"projected_skills":["{skill_id}"]}}
{{"name":"Read","input":{{"file_path":"{}"}}}}"#,
        path.display()
    );
    std::fs::write(&transcript_path, &body).unwrap();
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "ingest-session",
            "--member",
            mid,
            "--execution",
            exec.as_str(),
            "--file",
            transcript_path.to_str().unwrap(),
        ],
    );
    assert!(ok, "ingest: {stderr} {stdout}");
    assert!(stdout.contains(&skill_id), "{stdout}");
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
            exec.as_str(),
        ],
    );
    assert!(ok, "get after ingest: {stderr}");
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "ingest-session",
            "--member",
            mid,
            "--execution",
            exec.as_str(),
            "--file",
            transcript_path.to_str().unwrap(),
        ],
    );
    assert!(ok, "second ingest: {stderr}");
    let uses = SkillSetService::new(&store)
        .used_in_for_member_skill(&member, &skill_id)
        .unwrap();
    assert_eq!(uses.len(), 2, "initial plus one harness/cli read");
    let view = weight_of(home.path(), mid, &skill_id);
    assert!((view["weight"].as_f64().unwrap() - 2.0).abs() < 1e-6);
}

#[test]
fn session_track_does_not_decay_over_days_and_haisu_time_track_sticks() {
    let home = tempfile::TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let (session_skill, _) = register_team_skill(&store, dir.path(), "daily", "daily skill body");
    let (time_skill, _) = register_team_skill(&store, dir.path(), "payroll", "monthly payroll");
    let member = MemberId::new();
    let mid = member.as_str();
    claim(home.path(), mid, &session_skill, None);
    claim(home.path(), mid, &time_skill, None);
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "track",
            "--member",
            mid,
            "--skill",
            &time_skill,
            "--track",
            "time",
        ],
    );
    assert!(ok, "track: {stderr}");
    let past = OffsetDateTime::now_utc()
        .saturating_sub(time::Duration::days(60))
        .format(&Rfc3339)
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE member_skill_weights SET updated_at = ?1
             WHERE member_id = ?2 AND skill_id IN (?3, ?4)",
            rusqlite::params![past, mid, session_skill, time_skill],
        )
        .unwrap();
    let session_view = weight_of(home.path(), mid, &session_skill);
    let time_view = weight_of(home.path(), mid, &time_skill);
    assert_eq!(session_view["track"], "session");
    assert!(
        (session_view["weight"].as_f64().unwrap() - 1.0).abs() < 1e-9,
        "session weight {}",
        session_view["weight"]
    );
    assert_eq!(time_view["track"], "time");
    assert_eq!(time_view["track_source"], "haisu");
    let decayed = time_view["weight"].as_f64().unwrap();
    assert!((decayed - 0.5).abs() < 0.01, "lazy time decay {decayed}");
    for n in 0..30 {
        let (ok, _, stderr) = run_viva(
            home.path(),
            &[
                "skill",
                "session-end",
                "--member",
                mid,
                "--session",
                &format!("s{n}"),
            ],
        );
        assert!(ok, "session {n}: {stderr}");
    }
    let session_after = weight_of(home.path(), mid, &session_skill);
    let time_after = weight_of(home.path(), mid, &time_skill);
    assert!(
        (session_after["weight"].as_f64().unwrap() - 0.5).abs() < 1e-6,
        "session decay {}",
        session_after["weight"]
    );
    assert_eq!(time_after["track"], "time");
    assert_eq!(time_after["track_source"], "haisu");
    assert!(
        (time_after["weight"].as_f64().unwrap() - decayed).abs() < 1e-6,
        "time track moved with sessions: {}",
        time_after["weight"]
    );
}

#[test]
fn rebuild_restores_weight_from_events() {
    let home = tempfile::TempDir::new().unwrap();
    init_home(home.path());
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let (skill_id, _) = register_team_skill(&store, dir.path(), "notes", "take notes");
    let member = MemberId::new();
    let mid = member.as_str();
    claim(home.path(), mid, &skill_id, None);
    store
        .connection()
        .execute(
            "UPDATE member_skill_weights SET weight = 999 WHERE skill_id = ?1",
            [&skill_id],
        )
        .unwrap();
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "weight",
            "--member",
            mid,
            "--skill",
            &skill_id,
            "--rebuild",
        ],
    );
    assert!(ok, "rebuild: {stderr}");
    let view: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!((view["weight"].as_f64().unwrap() - 1.0).abs() < 1e-6);
}

#[test]
fn trial_reports_without_forgetting_and_real_forget_leaves_the_team_library() {
    let home = tempfile::TempDir::new().unwrap();
    init_home(home.path());
    write_config(
        home.path(),
        "[skill_weight]\nsession_half_life = 1\nsession_unused = 1\nprotection_sessions = 0\n\
         trial = on\nforget_weight = 0.1\ncapacity = 30\n",
    );
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let (skill_id, _) = register_team_skill(
        &store,
        dir.path(),
        "stale",
        "stale skill unique-token-forget",
    );
    let member = MemberId::new();
    let mid = member.as_str();
    claim(home.path(), mid, &skill_id, None);
    for n in 0..4 {
        let (ok, _, stderr) = run_viva(
            home.path(),
            &[
                "skill",
                "session-end",
                "--member",
                mid,
                "--session",
                &format!("trial-{n}"),
            ],
        );
        assert!(ok, "trial session: {stderr}");
    }
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "session-end",
            "--member",
            mid,
            "--session",
            "trial-4",
        ],
    );
    assert!(ok, "last trial: {stderr}");
    assert!(stdout.contains(&skill_id), "would_forget: {stdout}");
    assert!(stdout.contains("\"forgotten\": []"), "{stdout}");
    assert_eq!(
        SkillSetService::new(&store)
            .list_member_skills(&member)
            .unwrap()
            .len(),
        1
    );

    write_config(
        home.path(),
        "[skill_weight]\nsession_half_life = 1\nsession_unused = 1\nprotection_sessions = 0\n\
         trial = off\nforget_weight = 0.1\ncapacity = 30\n",
    );
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "session-end",
            "--member",
            mid,
            "--session",
            "real-forget",
        ],
    );
    assert!(ok, "forget: {stderr} {stdout}");
    assert!(stdout.contains(&skill_id), "{stdout}");
    assert!(
        SkillSetService::new(&store)
            .list_member_skills(&member)
            .unwrap()
            .is_empty()
    );
    assert!(
        KnowledgeRegistry::new(&store)
            .skill(&skill_id)
            .unwrap()
            .is_some()
    );
    let (ok, member_hits, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "search",
            "--member",
            mid,
            "--query",
            "unique-token-forget",
        ],
    );
    assert!(ok, "search: {stderr}");
    assert!(
        !member_hits.contains(&skill_id),
        "forgotten skill still in member search: {member_hits}"
    );
    let (ok, team_hits, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "search",
            "--member",
            mid,
            "--query",
            "unique-token-forget",
            "--team",
            "yes",
        ],
    );
    assert!(ok, "team search: {stderr}");
    assert!(team_hits.contains(&skill_id), "team library: {team_hits}");
}

#[test]
fn innate_and_protected_skills_are_not_forgotten_and_overflow_is_reported() {
    let home = tempfile::TempDir::new().unwrap();
    init_home(home.path());
    write_config(
        home.path(),
        "[skill_weight]\ncapacity = 1\nprotection_sessions = 100\ntrial = off\n\
         forget_weight = 0.1\n",
    );
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let (innate, _) = register_team_skill(&store, dir.path(), "soul-skill", "innate body");
    let (low, _) = register_team_skill(&store, dir.path(), "low-skill", "low body");
    let (high, _) = register_team_skill(&store, dir.path(), "high-skill", "high body");
    let member = MemberId::new();
    let mid = member.as_str();
    let soul = viva::knowledge::member_soul_path(home.path(), &member);
    std::fs::create_dir_all(soul.parent().unwrap()).unwrap();
    std::fs::write(&soul, format!("## 本命技能\n\n- {innate}\n")).unwrap();
    claim(home.path(), mid, &innate, None);
    claim(home.path(), mid, &low, None);
    claim(home.path(), mid, &high, None);
    let extra = execution(&store, &member, ExecutionStatus::Completed, None);
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "get",
            "--member",
            mid,
            "--skill",
            &high,
            "--execution",
            extra.as_str(),
        ],
    );
    assert!(ok, "boost high: {stderr}");
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "session-end",
            "--member",
            mid,
            "--session",
            "protected",
        ],
    );
    assert!(ok, "protected end: {stderr} {stdout}");
    assert!(stdout.contains("\"forgotten\": []"), "{stdout}");
    assert!(
        stdout.contains("\"capacity_excess\": 1") || stdout.contains("\"capacity_excess\": 2"),
        "{stdout}"
    );
    let active: Vec<_> = SkillSetService::new(&store)
        .list_member_skills(&member)
        .unwrap()
        .into_iter()
        .map(|row| row.skill_id)
        .collect();
    assert!(active.contains(&innate));
    assert!(active.contains(&low));
    assert!(active.contains(&high));
    let (ok, weekly, stderr) = run_viva(home.path(), &["skill", "weekly"]);
    assert!(ok, "weekly: {stderr}");
    assert!(weekly.contains("capacity_overflow"), "{weekly}");

    write_config(
        home.path(),
        "[skill_weight]\ncapacity = 1\nprotection_sessions = 0\ntrial = off\n\
         session_half_life = 1000000\nforget_weight = 0.1\n",
    );
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "session-end",
            "--member",
            mid,
            "--session",
            "capacity",
        ],
    );
    assert!(ok, "capacity: {stderr} {stdout}");
    assert!(stdout.contains(&low), "lowest should go: {stdout}");
    assert!(!stdout.contains(&innate), "innate was reported: {stdout}");
    let active: Vec<_> = SkillSetService::new(&store)
        .list_member_skills(&member)
        .unwrap()
        .into_iter()
        .map(|row| row.skill_id)
        .collect();
    assert!(active.contains(&innate), "innate left the set: {active:?}");
    assert!(active.contains(&high), "higher skill left: {active:?}");
    assert!(!active.contains(&low), "lowest stayed: {active:?}");
}

#[test]
fn late_failure_rescores_a_read_that_was_recorded_as_success() {
    let home = tempfile::TempDir::new().unwrap();
    init_home(home.path());
    write_config(
        home.path(),
        "[skill_weight]\nsession_half_life = 1000000\ntrial = off\n",
    );
    let store = open_store(home.path());
    let dir = tempfile::TempDir::new().unwrap();
    let (skill_id, _) = register_team_skill(&store, dir.path(), "retry", "retry the job");
    let member = MemberId::new();
    let mid = member.as_str();
    claim(home.path(), mid, &skill_id, None);
    let session =
        SessionRecord::new(SessionKind::Conversation, "late failure").with_member(member.clone());
    insert_session(&store, &session).expect("session");
    let running = execution(
        &store,
        &member,
        ExecutionStatus::Running,
        Some(session.session_id.clone()),
    );
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
            running.as_str(),
        ],
    );
    assert!(ok, "running get: {stderr}");
    store
        .connection()
        .execute(
            "UPDATE executions SET status = 'failed' WHERE execution_id = ?1",
            [running.as_str()],
        )
        .unwrap();
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "session-end",
            "--member",
            mid,
            "--session",
            session.session_id.as_str(),
        ],
    );
    assert!(ok, "settle: {stderr}");
    let view = weight_of(home.path(), mid, &skill_id);
    assert!(
        (view["weight"].as_f64().unwrap() - 1.25).abs() < 0.01,
        "late failure weight {}",
        view["weight"]
    );
}
