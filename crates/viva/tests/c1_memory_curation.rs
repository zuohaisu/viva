//! C1 curation and bounded recall (issue #116).

use viva::foundation::ids::{MemberId, ProjectId};
use viva::foundation::store::{
    DOMAIN_AUTHORITY, DOMAIN_FOUNDATION, DOMAIN_MEMORY, FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::memory::{
    AdmissionDecision, KnowledgeKind, MemorySearch, MemoryService, RecallFilter, SessionTurn,
};

fn store_v4() -> Store {
    let registry = MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(DOMAIN_MEMORY, 1, "memory v1", viva::memory::MEMORY_V1_SQL)
        .register(DOMAIN_MEMORY, 2, "memory v2", viva::memory::MEMORY_V2_SQL)
        .register_fn(
            DOMAIN_MEMORY,
            3,
            "memory v3",
            viva::memory::migrate::apply_memory_v3,
        )
        .register_fn(
            DOMAIN_MEMORY,
            4,
            "memory v4",
            viva::memory::migrate_v4::apply_memory_v4,
        )
        .register(
            DOMAIN_AUTHORITY,
            1,
            "authority v1",
            viva::authority::AUTHORITY_V1_SQL,
        )
        .freeze()
        .expect("registry");
    Store::open_in_memory(&registry).expect("store")
}

fn propose_and_admit(
    service: &MemoryService,
    member: &MemberId,
    project: Option<&ProjectId>,
    content: &str,
    source: &str,
    kind: KnowledgeKind,
    emphasis: bool,
) -> viva::memory::RememberResult {
    let proposed = service
        .remember_with_options(
            member, project, content, source, "general", "", kind, emphasis, None, None,
        )
        .expect("propose");
    assert_eq!(proposed.candidate_status, "proposed");
    service
        .admit_candidate(&proposed.candidate_id)
        .expect("admit")
}

#[test]
fn remember_creates_proposed_before_admission() {
    let store = store_v4();
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let proposed = service
        .remember_with_options(
            &member,
            None,
            "Haisu emphasized: always run fmt before pushing memory changes",
            "chat",
            "general",
            "",
            KnowledgeKind::PersonalMemory,
            true,
            None,
            None,
        )
        .expect("propose");
    assert_eq!(proposed.candidate_status, "proposed");
    assert!(proposed.decision.is_none());
    let status: String = store
        .connection()
        .query_row(
            "SELECT status FROM memory_candidates WHERE candidate_id = ?1",
            [&proposed.candidate_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "proposed");
}

#[test]
fn admission_rejects_one_off_and_dynamic_pr_facts() {
    let store = store_v4();
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let thin = service
        .remember_with_options(
            &member,
            None,
            "just once",
            "chat",
            "general",
            "",
            KnowledgeKind::PersonalMemory,
            false,
            None,
            None,
        )
        .expect("propose");
    let thin_admit = service.admit_candidate(&thin.candidate_id).expect("admit");
    assert!(matches!(
        thin_admit.decision,
        Some(AdmissionDecision::Reject { .. })
    ));

    let dynamic = service
        .remember_with_options(
            &member,
            None,
            "For the record PR #99 has been merged into main yesterday",
            "chat",
            "general",
            "",
            KnowledgeKind::PersonalMemory,
            false,
            None,
            None,
        )
        .expect("propose");
    let dynamic_admit = service
        .admit_candidate(&dynamic.candidate_id)
        .expect("admit");
    assert!(matches!(
        dynamic_admit.decision,
        Some(AdmissionDecision::Reject { .. })
    ));
}

#[test]
fn explicit_emphasis_admits_decision_about_open_issue() {
    let store = store_v4();
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let admitted = propose_and_admit(
        &service,
        &member,
        None,
        "We decided to keep the issue of cache invalidation strategy open for next quarter review",
        "chat",
        KnowledgeKind::PersonalMemory,
        true,
    );
    assert!(matches!(
        admitted.decision,
        Some(AdmissionDecision::Admit { .. })
    ));
    assert!(admitted.fact_id.is_some());
}

#[test]
fn search_does_not_write_execution_usage() {
    let store = store_v4();
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let remembered = propose_and_admit(
        &service,
        &member,
        None,
        "Haisu emphasized: independent QA must precede merge for Viva delivery",
        "emphasis test",
        KnowledgeKind::PersonalMemory,
        true,
    );
    let fact_id = remembered.fact_id.expect("fact");

    let _ = service
        .search(&member, None, "independent QA", 16 * 1024)
        .expect("search");
    let n: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM memory_execution_usages WHERE fact_ref = ?1",
            [&fact_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0, "retrieval alone must not record used_in");

    service
        .record_execution_usage(&fact_id, &member, "exe-test-1", "applied in task")
        .expect("record use");
    let n: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM memory_execution_usages WHERE fact_ref = ?1",
            [&fact_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn team_knowledge_visible_across_members_personal_isolated() {
    let store = store_v4();
    let service = MemoryService::new(&store);
    let alice = MemberId::new();
    let bob = MemberId::new();
    let team_fact = "Team policy: always run cargo test before opening a Resident memory PR";

    propose_and_admit(
        &service,
        &alice,
        None,
        team_fact,
        "alice session",
        KnowledgeKind::TeamKnowledge,
        true,
    );

    propose_and_admit(
        &service,
        &alice,
        None,
        "Alice personal note: prefers morning deep work before meetings",
        "alice",
        KnowledgeKind::PersonalMemory,
        true,
    );

    let filter = RecallFilter::default();
    let bob_team = service
        .search_filtered(&bob, None, "Resident memory PR", 16 * 1024, &filter)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = bob_team else {
        panic!("fetched");
    };
    assert_eq!(facts.len(), 1, "team knowledge crosses members");

    let bob_personal = service
        .search_filtered(&bob, None, "morning deep work", 16 * 1024, &filter)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = bob_personal else {
        panic!("fetched");
    };
    assert!(
        facts.is_empty(),
        "personal memory must not leak to another member under default filter"
    );
}

#[test]
fn kind_upgrade_personal_to_team_on_same_content() {
    let store = store_v4();
    let service = MemoryService::new(&store);
    let alice = MemberId::new();
    let content = "Shared norm: document admission gate outcomes in GitHub issue comments";

    propose_and_admit(
        &service,
        &alice,
        None,
        content,
        "first",
        KnowledgeKind::PersonalMemory,
        true,
    );
    propose_and_admit(
        &service,
        &alice,
        None,
        content,
        "second",
        KnowledgeKind::TeamKnowledge,
        true,
    );

    let scope: String = store
        .connection()
        .query_row(
            "SELECT link_scope FROM memory_links WHERE member_id = ?1 LIMIT 1",
            [alice.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(scope, "team");
}

#[test]
fn curate_session_proposes_and_admits_with_emphasis() {
    let store = store_v4();
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let turns = vec![SessionTurn {
        role: "user".into(),
        content: "记住：我们决定 TFHS 任务先独立 QA 再合并，不要开发自己改 in_review。".into(),
    }];
    let outcome = service
        .curate_session(&member, None, &turns, "{}", true)
        .expect("curate");
    assert!(outcome.admitted >= 1, "emphasis curations admit");
}

#[test]
fn project_knowledge_requires_project_scope() {
    let store = store_v4();
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let project = ProjectId::new();
    propose_and_admit(
        &service,
        &member,
        Some(&project),
        "Project rule: recall tests must scope-filter before FTS ranking",
        "spec",
        KnowledgeKind::ProjectKnowledge,
        true,
    );

    let filter = RecallFilter {
        include_personal: false,
        include_project: true,
        include_team: true,
        ..RecallFilter::default()
    };
    let hit = service
        .search_filtered(&member, Some(&project), "scope-filter", 16 * 1024, &filter)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = hit else {
        panic!("fetched");
    };
    assert_eq!(facts.len(), 1);
}
