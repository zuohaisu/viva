//! Member callback CLI (issue #117): recall, remember, dialog state, skill.
//!
//! Identity is the current execution plus a live grant. The real `viva`
//! binary is what Pi and Claude Code invoke; both harness env maps are
//! walked through the same four commands.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr as _;

use tempfile::TempDir;
use viva::authority::{
    ACTION_CALLBACK_RECALL, ACTION_CALLBACK_REMEMBER, ACTION_CALLBACK_SKILL_READ,
    ACTION_CALLBACK_SKILL_WRITE, ACTION_CALLBACK_STATE_READ, ACTION_CALLBACK_STATE_WRITE,
    AuthorityEngine, GrantMode,
};
use viva::callback::open_member_execution;
use viva::foundation::ids::{GrantId, MemberId, ProjectId};
use viva::foundation::store::Store;
use viva::harness::claude::ClaudeHarness;
use viva::harness::pi::{ENV_EXECUTION_ID, ENV_GRANT_ID, PiHarness};
use viva::knowledge::{KnowledgeRegistry, SkillSetService};
use viva::members::MemberRegistry;
use viva::memory::{KnowledgeKind, MemoryService};
use viva::tasks::TaskRegistry;

struct Ids {
    member_a: String,
    member_b: String,
    other_project: String,
    execution: String,
    grant: String,
    read_grant: String,
    expired_grant: String,
    revoked_grant: String,
    skill_a: String,
    skill_b: String,
}

fn run(home: &Path, env: &[(&str, &str)], clear: &[&str], args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_viva"));
    cmd.args(args).env("VIVA_HOME", home);
    for key in clear {
        cmd.env_remove(key);
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("spawn viva");
    (
        output.status.code().unwrap_or(1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn init_home(home: &Path) {
    let (code, _, stderr) = run(home, &[], &[], &["init"]);
    assert_eq!(code, 0, "init: {stderr}");
}

fn open_store(home: &Path) -> Store {
    let db = viva::foundation::paths::database_path(home);
    Store::open(&db, viva::server::migrations()).expect("store")
}

fn admit(
    service: &MemoryService,
    member: &MemberId,
    project: Option<&ProjectId>,
    content: &str,
    kind: KnowledgeKind,
) {
    let proposed = service
        .remember_with_options(
            member, project, content, "seed", "general", "", kind, true, None, None,
        )
        .expect("propose");
    let admitted = service
        .admit_candidate(&proposed.candidate_id)
        .expect("admit");
    assert_eq!(admitted.candidate_status, "admitted", "{content}");
}

fn write_skill(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("skill file");
    path.canonicalize().expect("canonical")
}

fn seed(home: &Path) -> Ids {
    let store = open_store(home);
    let members = MemberRegistry::new(&store);
    let member_a = members.register("Member A").expect("a");
    let member_b = members.register("Member B").expect("b");
    let project = ProjectId::new();
    let other_project = ProjectId::new();
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task(
            "callback session",
            vec![],
            Some(member_a.member_id.clone()),
            None,
            Some(project.clone()),
        )
        .expect("task");
    let execution = open_member_execution(&store, &member_a, &task, None, "pi").expect("execution");
    let authority = AuthorityEngine::new(&store);
    let actions = vec![
        ACTION_CALLBACK_RECALL.to_string(),
        ACTION_CALLBACK_REMEMBER.to_string(),
        ACTION_CALLBACK_STATE_READ.to_string(),
        ACTION_CALLBACK_STATE_WRITE.to_string(),
        ACTION_CALLBACK_SKILL_READ.to_string(),
    ];
    let grant = authority
        .issue_root_grant(
            Some(member_a.member_id.clone()),
            Some(task.task_id.clone()),
            actions,
            GrantMode::Propose,
            None,
        )
        .expect("grant");
    let read_grant = authority
        .issue_root_grant(
            Some(member_a.member_id.clone()),
            Some(task.task_id.clone()),
            vec![
                ACTION_CALLBACK_RECALL.to_string(),
                ACTION_CALLBACK_STATE_READ.to_string(),
                ACTION_CALLBACK_SKILL_READ.to_string(),
            ],
            GrantMode::Read,
            None,
        )
        .expect("read grant");
    let expired_grant = authority
        .issue_root_grant(
            Some(member_a.member_id.clone()),
            Some(task.task_id.clone()),
            vec![ACTION_CALLBACK_RECALL.to_string()],
            GrantMode::Read,
            Some("2000-01-01T00:00:00Z".into()),
        )
        .expect("expired");
    let revoked = authority
        .issue_root_grant(
            Some(member_a.member_id.clone()),
            Some(task.task_id.clone()),
            vec![ACTION_CALLBACK_RECALL.to_string()],
            GrantMode::Read,
            None,
        )
        .expect("revoked");
    authority
        .revoke(&revoked.grant_id, "withdrawn before the call")
        .expect("revoke");

    let memory = MemoryService::new(&store);
    admit(
        &memory,
        &member_b.member_id,
        None,
        "remember this: member B keeps a private tea preference for oolong",
        KnowledgeKind::PersonalMemory,
    );
    admit(
        &memory,
        &member_a.member_id,
        None,
        "remember this: member A keeps a private coffee preference for pour over",
        KnowledgeKind::PersonalMemory,
    );
    admit(
        &memory,
        &member_a.member_id,
        Some(&project),
        "remember this: the project builds recall behind the curation gate",
        KnowledgeKind::ProjectKnowledge,
    );
    admit(
        &memory,
        &member_a.member_id,
        None,
        "remember this: the team writes decisions rather than live status",
        KnowledgeKind::TeamKnowledge,
    );

    let dir = TempDir::new().expect("skill dir");
    let knowledge = KnowledgeRegistry::new(&store);
    let skill_a = knowledge
        .register_skill(
            "callback skill",
            write_skill(dir.path(), "a.md", "callbackskilltoken for member A"),
            "c2",
            None,
            vec![],
        )
        .expect("skill a")
        .skill_id;
    let skill_b = knowledge
        .register_skill(
            "team only skill",
            write_skill(dir.path(), "b.md", "teamlibrarytoken shared with the team"),
            "c2",
            None,
            vec![],
        )
        .expect("skill b")
        .skill_id;
    let sets = SkillSetService::new(&store);
    sets.claim(&member_a.member_id, &skill_a, Some(&execution.execution_id))
        .expect("claim a");
    sets.claim(&member_b.member_id, &skill_b, None)
        .expect("claim b");
    // Skill bodies stay on disk for later CLI reads. keep() disables cleanup.
    let _skill_dir = dir.keep();

    Ids {
        member_a: member_a.member_id.to_string(),
        member_b: member_b.member_id.to_string(),
        other_project: other_project.to_string(),
        execution: execution.execution_id.to_string(),
        grant: grant.grant_id.to_string(),
        read_grant: read_grant.grant_id.to_string(),
        expired_grant: expired_grant.grant_id.to_string(),
        revoked_grant: revoked.grant_id.to_string(),
        skill_a,
        skill_b,
    }
}

fn callback_env<'a>(ids: &'a Ids, grant: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        (ENV_EXECUTION_ID, ids.execution.as_str()),
        (ENV_GRANT_ID, grant),
    ]
}

fn json_result(stdout: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(stdout).unwrap_or_else(|err| {
        panic!("stdout is not JSON ({err}): {stdout}");
    });
    value
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn count(store: &Store, sql: &str) -> i64 {
    store
        .connection()
        .query_row(sql, [], |row| row.get(0))
        .expect(sql)
}

#[test]
fn callback_isolates_members_and_separates_recall_results() {
    let home = TempDir::new().expect("home");
    init_home(home.path());
    let ids = seed(home.path());
    let env = callback_env(&ids, &ids.grant);

    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "recall", "--query", "remember this"],
    );
    assert_eq!(code, 0, "recall: {stderr}");
    assert_eq!(json_result(&stdout), "matched");
    assert!(stdout.contains("curation gate"), "{stdout}");
    assert!(stdout.contains("decisions"), "{stdout}");
    assert!(!stdout.contains("oolong"), "B personal leaked: {stdout}");
    assert!(
        !stdout.contains("pour over"),
        "work session must not include personal: {stdout}"
    );

    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &[
            "memory", "state", "set", "--domain", "personal", "--by", "haisu",
        ],
    );
    assert_eq!(code, 0, "state set: {stderr}");
    assert!(stdout.contains("\"domain\": \"personal\""), "{stdout}");
    assert!(stdout.contains("\"written_by\": \"haisu\""), "{stdout}");

    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "recall", "--query", "remember this"],
    );
    assert_eq!(code, 0, "personal recall: {stderr}");
    assert!(stdout.contains("pour over"), "{stdout}");
    assert!(
        !stdout.contains("oolong"),
        "personal write still hides B: {stdout}"
    );

    let (code, _stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "state", "set", "--domain", "work"],
    );
    assert_eq!(code, 0, "state work: {stderr}");
    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "recall", "--query", "pour over"],
    );
    assert_eq!(code, 0, "work recall: {stderr}");
    assert_eq!(json_result(&stdout), "no_match");

    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &[
            "memory",
            "search",
            "--query",
            "pour over",
            "--member",
            &ids.member_a,
        ],
    );
    assert_eq!(code, 0, "callback search: {stderr}");
    assert_eq!(json_result(&stdout), "no_match");
    assert!(!stdout.contains("pour over"), "{stdout}");
    let (code, _, stderr) = run(
        home.path(),
        &env,
        &[],
        &[
            "memory",
            "search",
            "--query",
            "oolong",
            "--member",
            &ids.member_b,
        ],
    );
    assert_ne!(code, 0, "search must refuse a forged member");
    assert!(stderr.contains("authority.refused"), "{stderr}");

    let (code, _, stderr) = run(
        home.path(),
        &env,
        &[],
        &[
            "memory",
            "state",
            "set",
            "--domain",
            "work",
            "--project",
            &ids.other_project,
        ],
    );
    assert_ne!(code, 0, "foreign project must be refused");
    assert!(stderr.contains("authority.refused"), "{stderr}");

    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "recall", "--query", "no-such-callback-token"],
    );
    assert_eq!(code, 0, "no match: {stderr}");
    assert_eq!(json_result(&stdout), "no_match");

    let store = open_store(home.path());
    let uses_before = count(&store, "SELECT COUNT(*) FROM memory_execution_usages");
    drop(store);
    let (code, _, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "recall", "--query", "curation"],
    );
    assert_eq!(code, 0, "recall again: {stderr}");
    let store = open_store(home.path());
    let uses_after = count(&store, "SELECT COUNT(*) FROM memory_execution_usages");
    assert_eq!(uses_before, uses_after, "recall must not write used_in");
    drop(store);

    let phrase = "candidatephrase keep this proposed and not admitted yet please";
    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "remember", "--content", phrase],
    );
    assert_eq!(code, 0, "remember: {stderr}");
    let remembered: serde_json::Value = serde_json::from_str(&stdout).expect("remember json");
    assert_eq!(remembered["candidate_status"], "proposed");
    assert_eq!(remembered["execution_id"], ids.execution);
    assert!(remembered["fact_id"].is_null());
    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "recall", "--query", "candidatephrase"],
    );
    assert_eq!(code, 0, "recall candidate: {stderr}");
    assert_eq!(json_result(&stdout), "no_match");
    let store = open_store(home.path());
    let facts = count(
        &store,
        "SELECT COUNT(*) FROM memory_facts WHERE content LIKE '%candidatephrase%'",
    );
    let proposed = count(
        &store,
        "SELECT COUNT(*) FROM memory_candidates WHERE status = 'proposed' AND content LIKE '%candidatephrase%' AND provenance_json LIKE '%execution_id%'",
    );
    assert_eq!(facts, 0, "remember must not create knowledge");
    assert_eq!(proposed, 1, "candidate stays proposed with the execution");
    drop(store);

    let (code, stdout, stderr) = run(home.path(), &env, &[], &["skill", "list"]);
    assert_eq!(code, 0, "skill list: {stderr}");
    assert!(stdout.contains(&ids.skill_a), "{stdout}");
    assert!(!stdout.contains(&ids.skill_b), "B's set leaked: {stdout}");
    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["skill", "search", "--query", "callbackskilltoken"],
    );
    assert_eq!(code, 0, "skill search: {stderr}");
    assert!(stdout.contains(&ids.skill_a), "{stdout}");
    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &[
            "skill",
            "search",
            "--query",
            "teamlibrarytoken",
            "--team",
            "yes",
        ],
    );
    assert_eq!(code, 0, "team search: {stderr}");
    assert!(stdout.contains(&ids.skill_b), "{stdout}");
    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["skill", "get", "--skill", &ids.skill_a],
    );
    assert_eq!(code, 0, "skill get: {stderr}");
    assert!(stdout.contains("callbackskilltoken"), "{stdout}");

    let commands: Vec<Vec<&str>> = vec![
        vec![
            "memory",
            "recall",
            "--query",
            "curation",
            "--member",
            &ids.member_b,
        ],
        vec![
            "memory",
            "remember",
            "--content",
            phrase,
            "--member",
            &ids.member_b,
        ],
        vec!["memory", "state", "get", "--member", &ids.member_b],
        vec!["skill", "list", "--member", &ids.member_b],
    ];
    for args in &commands {
        let (code, _, stderr) = run(home.path(), &env, &[], args);
        assert_ne!(code, 0, "forged member must be refused: {args:?} {stderr}");
        assert!(
            stderr.contains("authority.refused"),
            "forged member: {args:?} {stderr}"
        );
    }

    let read_env = callback_env(&ids, &ids.read_grant);
    for args in [
        vec!["memory", "remember", "--content", phrase],
        vec!["memory", "state", "set", "--domain", "personal"],
        vec!["skill", "claim", "--skill", &ids.skill_b],
    ] {
        let (code, _, stderr) = run(home.path(), &read_env, &[], &args);
        assert_ne!(code, 0, "unauthorized write: {args:?} {stderr}");
        assert!(stderr.contains("authority.refused"), "{args:?} {stderr}");
    }

    let bare = vec![(ENV_EXECUTION_ID, ids.execution.as_str())];
    let expired = callback_env(&ids, &ids.expired_grant);
    let revoked = callback_env(&ids, &ids.revoked_grant);
    for (label, env, clear) in [
        ("missing", bare.as_slice(), &["VIVA_GRANT_ID"][..]),
        ("expired", expired.as_slice(), &[][..]),
        ("revoked", revoked.as_slice(), &[][..]),
    ] {
        for args in [
            &["memory", "recall", "--query", "curation"][..],
            &["memory", "remember", "--content", phrase][..],
            &["memory", "state", "get"][..],
            &["skill", "list"][..],
        ] {
            let (code, _, stderr) = run(home.path(), env, clear, args);
            assert_ne!(code, 0, "{label} grant: {args:?} {stderr}");
            assert!(
                stderr.contains("authority.refused"),
                "{label} grant: {args:?} {stderr}"
            );
        }
    }

    let (code, _, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "admit", "--candidate", "nope"],
    );
    assert_ne!(code, 0, "admit is not a member callback: {stderr}");
    assert!(stderr.contains("authority.refused"), "{stderr}");

    let store = open_store(home.path());
    let denials = count(&store, "SELECT COUNT(*) FROM grant_denials");
    let events = count(
        &store,
        "SELECT COUNT(*) FROM events WHERE kind = 'authority.refused'",
    );
    assert!(denials > 0, "refusals land in grant_denials");
    assert_eq!(
        events, denials,
        "each refusal also records authority.refused"
    );
    drop(store);
}

#[test]
fn recall_not_ready_and_backend_error_stay_distinct() {
    let home = TempDir::new().expect("home");
    init_home(home.path());
    let ids = seed(home.path());
    let env = callback_env(&ids, &ids.grant);

    let store = open_store(home.path());
    store
        .connection()
        .execute_batch("DROP TABLE memory_candidates;")
        .expect("drop candidates");
    drop(store);
    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "recall", "--query", "curation"],
    );
    assert_eq!(code, 0, "not_ready exits 0: {stderr}");
    assert_eq!(json_result(&stdout), "not_ready");

    let home = TempDir::new().expect("home");
    init_home(home.path());
    let ids = seed(home.path());
    let env = callback_env(&ids, &ids.grant);
    let store = open_store(home.path());
    store
        .connection()
        .execute_batch("ALTER TABLE memory_facts RENAME TO memory_facts_hidden;")
        .expect("hide facts");
    drop(store);
    let (code, stdout, stderr) = run(
        home.path(),
        &env,
        &[],
        &["memory", "recall", "--query", "curation"],
    );
    assert_ne!(code, 0, "backend_error exits non-zero: {stdout} {stderr}");
    assert_eq!(json_result(&stdout), "backend_error", "{stdout}");
    assert_ne!(json_result(&stdout), "not_ready");
    assert_ne!(json_result(&stdout), "no_match");
}

#[test]
fn pi_and_claude_envs_both_run_the_four_commands() {
    let home = TempDir::new().expect("home");
    init_home(home.path());
    let ids = seed(home.path());
    let store = open_store(home.path());
    let member = MemberRegistry::new(&store)
        .require(&MemberId::from_str(&ids.member_a).expect("member"))
        .expect("member row");
    let task = TaskRegistry::new(&store)
        .require_task(
            &viva::foundation::ids::TaskId::from_str(
                &store
                    .connection()
                    .query_row(
                        "SELECT task_id FROM executions WHERE execution_id = ?1",
                        [&ids.execution],
                        |row| row.get::<_, String>(0),
                    )
                    .expect("task id"),
            )
            .expect("task id"),
        )
        .expect("task");
    let execution = viva::foundation::ids::ExecutionId::from_str(&ids.execution).expect("exec");
    let grant = GrantId::from_str(&ids.grant).expect("grant");
    let repo = home.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo");
    let pi = PiHarness::for_repo(&repo)
        .launch(&member, None, Some(&task), &repo, Some(&grant))
        .expect("pi launch")
        .with_env(ENV_EXECUTION_ID, execution.to_string());
    let claude = ClaudeHarness {
        claude_binary: "claude".into(),
    }
    .launch(&member, Some(&task), &repo, Some(&grant), Some(&execution))
    .expect("claude launch");
    drop(store);

    for (label, spec) in [("pi", &pi), ("claude", &claude)] {
        let env: Vec<(&str, &str)> = spec
            .env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert!(
            env.iter()
                .any(|(k, v)| *k == ENV_EXECUTION_ID && *v == ids.execution),
            "{label} must inject the execution"
        );
        assert!(
            env.iter()
                .any(|(k, v)| *k == ENV_GRANT_ID && *v == ids.grant),
            "{label} must inject the grant"
        );
        let recall = run(
            home.path(),
            &env,
            &[],
            &["memory", "recall", "--query", "curation"],
        );
        assert_eq!(recall.0, 0, "{label} recall: {}", recall.2);
        assert_eq!(json_result(&recall.1), "matched");
        assert!(!recall.1.contains("oolong"), "{label} saw B: {}", recall.1);

        let state = run(home.path(), &env, &[], &["memory", "state", "get"]);
        assert_eq!(state.0, 0, "{label} state: {}", state.2);
        assert!(
            json_result(&state.1) == "unset" || json_result(&state.1) == "set",
            "{label} state: {}",
            state.1
        );

        let skills = run(home.path(), &env, &[], &["skill", "list"]);
        assert_eq!(skills.0, 0, "{label} skill: {}", skills.2);
        assert!(
            skills.1.contains(&ids.skill_a),
            "{label} skill list: {}",
            skills.1
        );

        let phrase = format!("{label} callback candidate stays proposed for this execution alone");
        let remembered = run(
            home.path(),
            &env,
            &[],
            &["memory", "remember", "--content", &phrase],
        );
        assert_eq!(remembered.0, 0, "{label} remember: {}", remembered.2);
        assert!(
            remembered.1.contains("\"candidate_status\": \"proposed\""),
            "{label} remember: {}",
            remembered.1
        );
        assert!(
            remembered.1.contains(&ids.execution),
            "{label} source execution"
        );
    }
}

#[test]
fn skill_write_constant_is_not_a_read() {
    // Keeps the write action out of the read grant used above.
    assert_ne!(ACTION_CALLBACK_SKILL_WRITE, ACTION_CALLBACK_SKILL_READ);
}
