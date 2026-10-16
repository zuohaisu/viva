//! Engagement state on one execution (issue #118).

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use viva::engagement::{
    EngagementDomain, EngagementService, EngagementSource, EngagementStatus, FixtureModel,
    FixtureReply, PendingReason, ResolveInput,
};
use viva::foundation::ids::{ExecutionId, MemberId, TaskId};
use viva::foundation::records::{AttributionSnapshot, ExecutionRecord, insert_execution};
use viva::foundation::store::Store;
use viva::knowledge::{KnowledgeRegistry, Scope, SkillSetService};
use viva::memory::{KnowledgeKind, MemorySearch, MemoryService};

fn store() -> Store {
    Store::open_in_memory(viva::server::migrations()).expect("store")
}

fn execution(store: &Store, member: &MemberId, task: &TaskId) -> ExecutionId {
    let record = ExecutionRecord::start(
        task.clone(),
        AttributionSnapshot::capture(member.clone(), "role", "model", "tool"),
    );
    insert_execution(store, &record).expect("execution");
    record.execution_id
}

fn admit(service: &MemoryService, member: &MemberId, content: &str, kind: KnowledgeKind) {
    let proposed = service
        .remember_with_options(
            member, None, content, "chat", "general", "", kind, true, None, None,
        )
        .expect("propose");
    let admitted = service
        .admit_candidate(&proposed.candidate_id)
        .expect("admit");
    assert!(
        admitted.fact_id.is_some(),
        "admission rejected: {admitted:?}"
    );
}

fn input(execution: ExecutionId) -> ResolveInput {
    ResolveInput {
        execution_id: execution,
        repository: None,
        task_id: None,
        entry: None,
        utterance: None,
        explicit_domain: None,
        project_id: None,
        interlocutor: None,
        activity: None,
    }
}

fn repo_task(execution: ExecutionId, task: TaskId) -> ResolveInput {
    ResolveInput {
        repository: Some("https://github.com/zuohaisu/viva".into()),
        task_id: Some(task),
        ..input(execution)
    }
}

fn titles_of(search: &MemorySearch) -> Vec<String> {
    match search {
        MemorySearch::Fetched { facts, .. } => {
            facts.iter().map(|fact| fact.content.clone()).collect()
        }
        MemorySearch::Unavailable { reason } => panic!("recall unavailable: {reason}"),
    }
}

#[test]
fn seeded_threshold_is_the_configured_initial_value() {
    let store = store();
    let raw: String = store
        .connection()
        .query_row(
            "SELECT value FROM engagement_settings WHERE key = 'confidence_threshold'",
            [],
            |row| row.get(0),
        )
        .expect("threshold");
    assert_eq!(raw, "0.8");
}

#[test]
fn explicit_beats_entry_and_entry_beats_the_model() {
    let store = store();
    let member = MemberId::new();
    let task = TaskId::new();
    let model = FixtureModel::new(
        "jev",
        FixtureReply::Decision {
            domain: EngagementDomain::Personal,
            confidence: 1.0,
        },
    );
    let service = EngagementService::new(&store, model.clone());

    let explicit_exec = execution(&store, &member, &task);
    let mut explicit = repo_task(explicit_exec, task.clone());
    explicit.utterance = Some("今天聊点私事".into());
    let explicit = service.resolve(explicit).expect("explicit");
    assert!(!explicit.model_called);
    assert_eq!(explicit.view.domain, Some(EngagementDomain::Personal));
    assert_eq!(explicit.view.source, Some(EngagementSource::Explicit));
    assert_eq!(model.calls(), 0);

    let work_exec = execution(&store, &member, &task);
    let work = service
        .resolve(repo_task(work_exec, task.clone()))
        .expect("entry");
    assert!(!work.model_called);
    assert_eq!(work.view.domain, Some(EngagementDomain::Work));
    assert_eq!(work.view.source, Some(EngagementSource::Entry));
    assert!(work.view.interlocutor.is_none());
    assert!(work.view.activity.is_none());
    assert_eq!(model.calls(), 0);

    let uncertain = execution(&store, &member, &task);
    let mut uncertain_input = input(uncertain);
    uncertain_input.utterance = Some("not sure what this is".into());
    let judged = service.resolve(uncertain_input).expect("model");
    assert!(judged.model_called);
    assert_eq!(judged.view.domain, Some(EngagementDomain::Personal));
    assert_eq!(judged.view.source, Some(EngagementSource::Model));
    assert_eq!(judged.view.model_binding.as_deref(), Some("jev"));
    assert_eq!(model.calls(), 1);
}

#[test]
fn repository_task_omits_personal_from_opening_projection_and_recall() {
    let store = store();
    let dir = TempDir::new().expect("dir");
    let member = MemberId::new();
    let other = MemberId::new();
    let task = TaskId::new();
    let execution_id = execution(&store, &member, &task);
    let memory = MemoryService::new(&store);
    admit(
        &memory,
        &member,
        "alphamarker personal note: prefers morning deep work before meetings",
        KnowledgeKind::PersonalMemory,
    );
    admit(
        &memory,
        &member,
        "alphamarker team note: ship through pull requests and keep main protected",
        KnowledgeKind::TeamKnowledge,
    );
    admit(
        &memory,
        &other,
        "alphamarker other personal note: a private preference of someone else",
        KnowledgeKind::PersonalMemory,
    );

    let knowledge = KnowledgeRegistry::new(&store);
    knowledge
        .record(
            "personal preference",
            Scope::Personal,
            dir.path().join("personal.md"),
            "owner",
            Some(member.clone()),
            None,
            vec![],
            None,
            viva::knowledge::ExternalState::None,
        )
        .expect("personal knowledge");
    knowledge
        .record(
            "team practice",
            Scope::Team,
            dir.path().join("team.md"),
            "team",
            None,
            None,
            vec![],
            None,
            viva::knowledge::ExternalState::None,
        )
        .expect("team knowledge");
    let personal_skill = write_skill(dir.path(), "personal-skill.md", "personal skill body");
    let team_skill = write_skill(dir.path(), "team-skill.md", "team skill body");
    let personal_reg = knowledge
        .register_skill(
            "personal skill",
            personal_skill,
            "owner",
            Some(member.clone()),
            vec![viva::engagement::PERSONAL_SKILL_TAG.into()],
        )
        .expect("personal skill");
    let team_reg = knowledge
        .register_skill(
            "team skill",
            team_skill,
            "team",
            None,
            vec!["delivery".into()],
        )
        .expect("team skill");
    let sets = SkillSetService::new(&store);
    sets.claim(&member, &personal_reg.skill_id, Some(&execution_id))
        .expect("claim personal skill");
    sets.claim(&member, &team_reg.skill_id, Some(&execution_id))
        .expect("claim team skill");

    let model = FixtureModel::new(
        "jev",
        FixtureReply::Decision {
            domain: EngagementDomain::Personal,
            confidence: 1.0,
        },
    );
    let service = EngagementService::new(&store, model.clone());
    let resolved = service
        .resolve(repo_task(execution_id.clone(), task))
        .expect("resolve");
    assert!(!resolved.model_called);
    assert_eq!(model.calls(), 0);
    assert_eq!(resolved.view.status, EngagementStatus::Adopted);
    assert_eq!(resolved.view.domain, Some(EngagementDomain::Work));

    let opening = service.opening_projection(&execution_id).expect("opening");
    let knowledge_titles: Vec<_> = opening
        .knowledge
        .iter()
        .map(|item| item.title.as_str())
        .collect();
    assert!(
        !knowledge_titles.contains(&"personal preference"),
        "{knowledge_titles:?}"
    );
    assert!(knowledge_titles.contains(&"team practice"));
    assert!(
        opening
            .memories
            .iter()
            .all(|fact| fact.knowledge_kind != "personal_memory")
    );
    assert!(
        opening
            .memories
            .iter()
            .any(|fact| fact.content.contains("team note"))
    );
    let skill_ids: Vec<_> = opening
        .skills
        .iter()
        .map(|skill| skill.skill_id.as_str())
        .collect();
    assert!(!skill_ids.contains(&personal_reg.skill_id.as_str()));
    assert!(skill_ids.contains(&team_reg.skill_id.as_str()));

    let recalled = service
        .recall(&execution_id, "alphamarker")
        .expect("recall");
    assert!(!recalled.include_personal);
    let contents = titles_of(&recalled.search);
    assert!(
        contents
            .iter()
            .all(|content| !content.contains("personal note"))
    );
    assert!(contents.iter().any(|content| content.contains("team note")));
}

#[test]
fn low_confidence_unavailable_and_unknown_do_not_store_a_guess() {
    let store = store();
    let member = MemberId::new();
    let task = TaskId::new();
    let model = FixtureModel::new(
        "jev",
        FixtureReply::Decision {
            domain: EngagementDomain::Personal,
            confidence: 0.4,
        },
    );
    let service = EngagementService::new(&store, model.clone());
    service.set_confidence_threshold(0.4).expect("threshold");

    let equal = execution(&store, &member, &task);
    let adopted = service.resolve(input(equal.clone())).expect("equal");
    assert!(adopted.model_called);
    assert_eq!(adopted.view.domain, Some(EngagementDomain::Personal));
    assert_eq!(adopted.view.confidence, Some(0.4));

    model.set_reply(FixtureReply::Decision {
        domain: EngagementDomain::Personal,
        confidence: 0.39,
    });
    let low = execution(&store, &member, &task);
    let pending = service.resolve(input(low.clone())).expect("low");
    assert!(pending.model_called);
    assert_eq!(pending.view.status, EngagementStatus::Pending);
    assert!(pending.view.domain.is_none());
    assert!(pending.view.interlocutor.is_none());
    assert_eq!(
        pending.view.pending_reason,
        Some(PendingReason::BelowThreshold)
    );
    let stored_domain: Option<String> = store
        .connection()
        .query_row(
            "SELECT domain FROM engagement_states WHERE execution_id = ?1",
            [low.as_str()],
            |row| row.get(0),
        )
        .expect("row");
    assert!(stored_domain.is_none());
    let fields = service.experiences(&low).expect("experience");
    assert!(
        fields
            .last()
            .expect("row")
            .fields_sent
            .iter()
            .any(|field| field == "utterance")
    );

    model.set_reply(FixtureReply::Unavailable);
    let down = execution(&store, &member, &task);
    let unavailable = service.resolve(input(down.clone())).expect("down");
    assert_eq!(unavailable.view.status, EngagementStatus::Pending);
    assert!(unavailable.view.domain.is_none());
    assert_eq!(
        unavailable.view.pending_reason,
        Some(PendingReason::ModelUnavailable)
    );

    let fresh = execution(&store, &member, &task);
    memory_pair(&store, &member);
    let recalled = service
        .recall(&fresh, "alphamarker")
        .expect("unknown recall");
    assert!(!recalled.include_personal);
    assert!(
        titles_of(&recalled.search)
            .iter()
            .all(|content| !content.contains("personal"))
    );
    let rows: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM engagement_states WHERE execution_id = ?1",
            [fresh.as_str()],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(rows, 0);
}

#[test]
fn swapped_judgment_binding_keeps_order_and_threshold() {
    let store = store();
    let member = MemberId::new();
    let task = TaskId::new();
    for name in ["jev", "other-judge"] {
        let model = FixtureModel::new(
            name,
            FixtureReply::Decision {
                domain: EngagementDomain::Personal,
                confidence: 0.2,
            },
        );
        let service = EngagementService::new(&store, model.clone());
        service.set_confidence_threshold(0.4).expect("threshold");
        let before = model.calls();

        let mut explicit = repo_task(execution(&store, &member, &task), task.clone());
        explicit.utterance = Some("state personal".into());
        let explicit = service.resolve(explicit).expect("explicit");
        assert!(!explicit.model_called);
        assert_eq!(explicit.view.domain, Some(EngagementDomain::Personal));

        let entry = service
            .resolve(repo_task(execution(&store, &member, &task), task.clone()))
            .expect("entry");
        assert!(!entry.model_called);
        assert_eq!(entry.view.domain, Some(EngagementDomain::Work));
        assert_eq!(model.calls(), before);

        let low = service
            .resolve(input(execution(&store, &member, &task)))
            .expect("low");
        assert!(low.model_called);
        assert!(low.view.domain.is_none());
        assert_eq!(low.view.model_binding.as_deref(), Some(name));

        model.set_reply(FixtureReply::Decision {
            domain: EngagementDomain::Work,
            confidence: 0.4,
        });
        let edge = service
            .resolve(input(execution(&store, &member, &task)))
            .expect("edge");
        assert_eq!(edge.view.domain, Some(EngagementDomain::Work));
        assert_eq!(edge.view.model_binding.as_deref(), Some(name));
        assert_eq!(edge.view.source, Some(EngagementSource::Model));
    }
}

#[test]
fn concurrent_executions_do_not_inherit_or_overwrite() {
    let store = store();
    let member = MemberId::new();
    let task = TaskId::new();
    let model = FixtureModel::new("jev", FixtureReply::Unavailable);
    let service = EngagementService::new(&store, model);
    let first = execution(&store, &member, &task);
    let second = execution(&store, &member, &task);
    let mut personal = input(first.clone());
    personal.explicit_domain = Some(EngagementDomain::Personal);
    personal.interlocutor = Some("haisu".into());
    personal.activity = Some("chat".into());
    service.resolve(personal).expect("first");

    let other = service
        .resolve(repo_task(second.clone(), task))
        .expect("second");
    assert_eq!(other.view.domain, Some(EngagementDomain::Work));
    assert!(other.view.interlocutor.is_none());
    assert!(other.view.activity.is_none());

    let kept = service.get(&first).expect("first still");
    assert_eq!(kept.domain, Some(EngagementDomain::Personal));
    assert_eq!(kept.interlocutor.as_deref(), Some("haisu"));
    assert_eq!(kept.activity.as_deref(), Some("chat"));
    assert_ne!(kept.execution_id, other.view.execution_id);
}

#[test]
fn correction_changes_later_recall_and_is_recorded() {
    let store = store();
    let member = MemberId::new();
    let task = TaskId::new();
    let execution_id = execution(&store, &member, &task);
    memory_pair(&store, &member);
    let before: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM memory_facts", [], |row| row.get(0))
        .expect("count");
    let model = FixtureModel::new(
        "jev",
        FixtureReply::Decision {
            domain: EngagementDomain::Personal,
            confidence: 0.4,
        },
    );
    let service = EngagementService::new(&store, model);
    service.set_confidence_threshold(0.4).expect("threshold");
    service.resolve(input(execution_id.clone())).expect("adopt");
    let personal = titles_of(
        &service
            .recall(&execution_id, "alphamarker")
            .expect("personal recall")
            .search,
    );
    assert!(
        personal
            .iter()
            .any(|content| content.contains("personal note"))
    );

    let mut correction = input(execution_id.clone());
    correction.explicit_domain = Some(EngagementDomain::Work);
    let corrected = service.resolve(correction).expect("correct");
    assert_eq!(corrected.view.domain, Some(EngagementDomain::Work));
    assert_eq!(corrected.view.source, Some(EngagementSource::Explicit));
    let work = titles_of(
        &service
            .recall(&execution_id, "alphamarker")
            .expect("work recall")
            .search,
    );
    assert!(
        work.iter()
            .all(|content| !content.contains("personal note"))
    );
    assert!(work.iter().any(|content| content.contains("team note")));
    let after: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM memory_facts", [], |row| row.get(0))
        .expect("count");
    assert_eq!(
        before, after,
        "switching domain must not delete memory rows"
    );
    let experiences = service.experiences(&execution_id).expect("experiences");
    assert!(
        experiences.iter().any(|row| row.kind == "corrected"),
        "{experiences:?}"
    );
}

#[test]
fn cli_state_command_resolves_a_repository_task_and_records_a_correction() {
    let home = TempDir::new().expect("home");
    let (ok, _, stderr) = run_viva(home.path(), &["init"]);
    assert!(ok, "init failed: {stderr}");

    let db = viva::foundation::paths::database_path(home.path());
    let store = Store::open(&db, viva::server::migrations()).expect("open");
    let member = MemberId::new();
    let task = TaskId::new();
    let execution_id = execution(&store, &member, &task);
    memory_pair(&store, &member);
    drop(store);

    let execution_arg = execution_id.as_str().to_string();
    let task_arg = task.as_str().to_string();
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "state",
            "resolve",
            "--execution",
            &execution_arg,
            "--repository",
            "https://github.com/zuohaisu/viva",
            "--task",
            &task_arg,
        ],
    );
    assert!(ok, "resolve failed: {stderr}\n{stdout}");
    let resolved: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(resolved["model_called"], false);
    assert_eq!(resolved["view"]["domain"], "work");
    assert_eq!(resolved["view"]["source"], "entry");

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "recall",
            "--execution",
            &execution_arg,
            "--query",
            "alphamarker",
        ],
    );
    assert!(ok, "recall failed: {stderr}\n{stdout}");
    assert!(
        !stdout.contains("personal note"),
        "work recall included personal: {stdout}"
    );
    assert!(stdout.contains("team note"), "{stdout}");

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "state",
            "set",
            "--execution",
            &execution_arg,
            "--domain",
            "personal",
            "--interlocutor",
            "haisu",
        ],
    );
    assert!(ok, "set failed: {stderr}\n{stdout}");
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "state",
            "experiences",
            "--execution",
            &execution_arg,
        ],
    );
    assert!(ok, "experiences failed: {stderr}\n{stdout}");
    assert!(stdout.contains("corrected"), "{stdout}");
}

fn memory_pair(store: &Store, member: &MemberId) {
    let memory = MemoryService::new(store);
    admit(
        &memory,
        member,
        "alphamarker personal note: prefers morning deep work before meetings",
        KnowledgeKind::PersonalMemory,
    );
    admit(
        &memory,
        member,
        "alphamarker team note: ship through pull requests and keep main protected",
        KnowledgeKind::TeamKnowledge,
    );
}

fn write_skill(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("skill file");
    path
}

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
