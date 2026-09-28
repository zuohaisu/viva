//! F02 acceptance tests (issue #24): evidence-backed, idempotent review
//! proposals for stale knowledge, idle skills, worktrees and repository
//! cleanliness; grant-gated low-risk execution with a recoverable exit
//! path; human-review-only for anything destructive; restart restores
//! records and replays nothing.

use std::path::{Path, PathBuf};

use viva::authority::{Actor, AuthorityEngine, GrantMode};
use viva::foundation::ids::{ExecutionId, MemberId, WorktreeId};
use viva::foundation::store::{
    DOMAIN_AUTHORITY, DOMAIN_FOUNDATION, DOMAIN_KNOWLEDGE, DOMAIN_MAINTENANCE,
    DOMAIN_TASKS_EXECUTIONS, FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::git::worktrees::{TaskWorktreeRecord, WorktreeSource};
use viva::knowledge::{ExternalState, KnowledgeRegistry, Scope};
use viva::maintenance::{MaintenanceService, MaintenanceSession, ProposalStatus, SuggestedAction};

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
            DOMAIN_KNOWLEDGE,
            1,
            "knowledge v1",
            viva::knowledge::KNOWLEDGE_V1_SQL,
        )
        .register(
            DOMAIN_MAINTENANCE,
            1,
            "maintenance v1",
            viva::maintenance::MAINTENANCE_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

fn store() -> Store {
    Store::open_in_memory(&frozen()).expect("store")
}

fn write_body(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write body");
    path.canonicalize().expect("canonical")
}

fn make_item(registry: &KnowledgeRegistry<'_>, dir: &Path, name: &str, title: &str) -> String {
    let body = write_body(dir, &format!("{name}.md"), "content");
    registry
        .record(
            title,
            Scope::Team,
            body,
            "from the 2026-09 review with Haisu",
            None,
            None,
            vec![],
            None,
            ExternalState::None,
        )
        .expect("item")
        .item_id
}

/// Acceptance: "一次真实复审可重复运行不制造重复提议" — the same stale
/// facts propose exactly once; a second run adds nothing.
#[test]
fn knowledge_review_is_repeatable_without_duplicates() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let maintenance = MaintenanceService::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");

    let stale_item = make_item(&registry, dir.path(), "stale", "Old deployment notes");
    let fresh_item = make_item(&registry, dir.path(), "fresh", "Fresh API notes");
    // Age the first item artificially (simulating an old update).
    store
        .connection()
        .execute(
            "UPDATE knowledge_items SET updated_at = '2020-01-01T00:00:00Z' WHERE item_id = ?1",
            [&stale_item],
        )
        .expect("age item");

    let session = MaintenanceSession::open(&store).expect("session");
    let report = maintenance
        .review_knowledge(&session, &registry, 365)
        .expect("first review");
    assert_eq!(report.proposed.len(), 1, "only the stale item is proposed");
    let proposal = &report.proposed[0];
    assert_eq!(proposal.subject, stale_item);
    assert_eq!(proposal.suggested_action, SuggestedAction::ArchiveKnowledge);
    assert!(
        proposal.evidence.contains("2020-01-01"),
        "evidence carries the facts: {}",
        proposal.evidence
    );
    assert!(
        proposal.evidence.contains("provenance"),
        "evidence keeps provenance: {}",
        proposal.evidence
    );
    let first_id = proposal.proposal_id.clone();

    // Re-run: same facts, no duplicates.
    let again = maintenance
        .review_knowledge(&session, &registry, 365)
        .expect("second review");
    assert!(again.proposed.is_empty(), "no duplicate proposals");
    assert_eq!(again.already_proposed, 1);
    assert_eq!(
        maintenance.open_proposals().expect("open").len(),
        1,
        "still exactly one open proposal"
    );

    // The fresh item is untouched by the stale threshold.
    let item = registry.get_item(&fresh_item).expect("item").expect("exists");
    assert_eq!(item.status, viva::knowledge::KnowledgeStatus::Active);

    // Resolve it, then a re-run still proposes nothing new (the item is no
    // longer active; the resolved proposal keeps its record).
    let actor = Actor::Member {
        member: MemberId::new(),
        grant: None,
    };
    maintenance
        .dismiss_proposal(&first_id, &actor, "owner: still relevant, keep it")
        .expect("dismiss");
    let third = maintenance
        .review_knowledge(&session, &registry, 365)
        .expect("third review");
    assert!(
        third.proposed.is_empty(),
        "dismissed proposals do not resurrect"
    );
    session.end(&store).expect("session ends");
}

/// Acceptance: "知识/技能退出保留来源/理由与可取回路径" — archiving goes
/// through the knowledge lifecycle (reason + actor appended, body file
/// kept, restore path intact); disabling a skill touches no file.
#[test]
fn executed_exits_stay_traceable_and_recoverable() {
    let store = store();
    let authority = AuthorityEngine::new(&store);
    let registry = KnowledgeRegistry::new(&store);
    let maintenance = MaintenanceService::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");

    // A stale knowledge item and an idle skill.
    let item = make_item(&registry, dir.path(), "old", "Outdated runbook");
    store
        .connection()
        .execute(
            "UPDATE knowledge_items SET updated_at = '2019-06-01T00:00:00Z' WHERE item_id = ?1",
            [&item],
        )
        .expect("age item");
    let skill_body = write_body(dir.path(), "skill-dir", "placeholder");
    let _ = skill_body;
    let skill_entry = dir.path().join("skills").join("old-skill");
    std::fs::create_dir_all(&skill_entry).expect("skill dir");
    std::fs::write(skill_entry.join("SKILL.md"), "# old skill").expect("SKILL.md");
    let skill = registry
        .register_skill(
            "Old skill",
            skill_entry.join("SKILL.md"),
            "imported from the old toolbelt",
            None,
            vec![],
        )
        .expect("skill");

    // The office-wide grant that authorizes maintenance execution.
    let maintainer = MemberId::new();
    let grant = authority
        .issue_root_grant(
            Some(maintainer.clone()),
            None,
            vec!["maintain_knowledge".into()],
            GrantMode::ActWithApproval,
            None,
        )
        .expect("grant");
    let actor = Actor::Member {
        member: maintainer,
        grant: Some(grant.grant_id),
    };

    let session = MaintenanceSession::open(&store).expect("session");
    let report = maintenance
        .review_knowledge(&session, &registry, 365)
        .expect("review");
    assert_eq!(report.proposed.len(), 2, "stale item + idle skill");

    // Execute both proposals.
    for proposal in &report.proposed {
        maintenance
            .execute_proposal(&proposal.proposal_id, &actor, &authority, &registry, "review agreed")
            .expect("execute");
    }

    // Knowledge exit: lifecycle reason recorded, body file kept, restore
    // path intact.
    let item_after = registry.get_item(&item).expect("item").expect("exists");
    assert_eq!(item_after.status, viva::knowledge::KnowledgeStatus::Archived);
    let lifecycle = registry.lifecycle(&item).expect("lifecycle");
    assert_eq!(lifecycle.len(), 1);
    assert!(
        lifecycle[0].reason.contains("maintenance"),
        "reason travels: {}",
        lifecycle[0].reason
    );
    assert!(item_after.body_path.is_file(), "the body file survives");
    registry
        .restore(&item, "owner asked to bring it back", "member-owner")
        .expect("archive is recoverable");
    let restored = registry.get_item(&item).expect("item").expect("exists");
    assert_eq!(restored.status, viva::knowledge::KnowledgeStatus::Active);

    // Skill exit: office state only, SKILL.md untouched, re-enableable.
    let skill_after = registry.skill(&skill.skill_id).expect("skill").expect("exists");
    assert!(!skill_after.enabled);
    assert!(skill_after.entry_path.is_file(), "SKILL.md is untouched");
    registry
        .set_skill_enabled(&skill.skill_id, true)
        .expect("reversible");

    assert!(
        maintenance.open_proposals().expect("open").is_empty(),
        "executed proposals leave the open set"
    );
    session.end(&store).expect("ends");
}

/// Acceptance: "维护范围和授权来自用户/grant，未授权删除…均不发生" —
/// execution without a live `maintain_knowledge` grant is denied (and the
/// denial logged); a task-scoped grant is refused; human-review proposals
/// are never executable by maintenance.
#[test]
fn execution_is_grant_gated_and_human_review_stays_human() {
    let store = store();
    let authority = AuthorityEngine::new(&store);
    let registry = KnowledgeRegistry::new(&store);
    let maintenance = MaintenanceService::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");

    let item = make_item(&registry, dir.path(), "x", "Aged item");
    store
        .connection()
        .execute(
            "UPDATE knowledge_items SET updated_at = '2018-01-01T00:00:00Z' WHERE item_id = ?1",
            [&item],
        )
        .expect("age item");

    let session = MaintenanceSession::open(&store).expect("session");
    let report = maintenance
        .review_knowledge(&session, &registry, 30)
        .expect("review");
    let proposal_id = report.proposed[0].proposal_id.clone();

    // No grant at all: denied, and the denial is appended.
    let bare_actor = Actor::Member {
        member: MemberId::new(),
        grant: None,
    };
    let err = maintenance
        .execute_proposal(&proposal_id, &bare_actor, &authority, &registry, "sneak")
        .expect_err("no grant, no execution");
    assert!(err.to_string().contains("not authorized"), "got: {err}");
    assert!(
        authority
            .recent_denials(10)
            .expect("denials")
            .iter()
            .any(|(_, action, _, _)| action == "maintain_knowledge"),
        "the denial is in the audit log"
    );

    // A task-scoped grant does not cover office-wide maintenance.
    let scoped = authority
        .issue_root_grant(
            None,
            Some(viva::foundation::ids::TaskId::new()),
            vec!["maintain_knowledge".into()],
            GrantMode::ActWithApproval,
            None,
        )
        .expect("scoped grant");
    let scoped_actor = Actor::Member {
        member: MemberId::new(),
        grant: Some(scoped.grant_id),
    };
    let err = maintenance
        .execute_proposal(&proposal_id, &scoped_actor, &authority, &registry, "task grant")
        .expect_err("task-scoped grants do not maintain the office");
    assert!(err.to_string().contains("not authorized"), "got: {err}");

    // The item is still active — nothing happened unauthorized.
    let untouched = registry.get_item(&item).expect("item").expect("exists");
    assert_eq!(untouched.status, viva::knowledge::KnowledgeStatus::Active);
    session.end(&store).expect("ends");
}

/// Acceptance: worktree and cleanliness findings are proposals for human
/// review; asking maintenance to execute them is refused, nothing is
/// removed, and the same dirty state does not propose twice.
#[test]
fn worktree_and_cleanliness_proposals_stay_human_review_only() {
    let store = store();
    let authority = AuthorityEngine::new(&store);
    let registry = KnowledgeRegistry::new(&store);
    let maintenance = MaintenanceService::new(&store);
    let maintainer = MemberId::new();
    let grant = authority
        .issue_root_grant(
            Some(maintainer.clone()),
            None,
            vec!["maintain_knowledge".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .expect("grant");
    let actor = Actor::Member {
        member: maintainer,
        grant: Some(grant.grant_id),
    };

    // An active worktree record (as the worktree service would report it).
    let repo = tempfile::TempDir::new().expect("repo dir");
    let wt = tempfile::TempDir::new().expect("wt dir");
    let record = TaskWorktreeRecord {
        worktree_id: WorktreeId::new(),
        task_id: viva::foundation::ids::TaskId::new(),
        repo_root: repo.path().to_path_buf(),
        worktree_path: wt.path().to_path_buf(),
        branch: "agent/feat-something".into(),
        base_sha: "abc1234".into(),
        source: WorktreeSource::Created,
        created_at: "2026-09-01T00:00:00Z".into(),
        released_at: None,
    };

    let session = MaintenanceSession::open(&store).expect("session");
    let report = maintenance
        .review_worktrees(&session, std::slice::from_ref(&record))
        .expect("worktree review");
    assert_eq!(report.proposed.len(), 1);
    assert_eq!(report.proposed[0].suggested_action, SuggestedAction::HumanReview);

    // Maintenance refuses to execute it — and the worktree still exists.
    let err = maintenance
        .execute_proposal(&report.proposed[0].proposal_id, &actor, &authority, &registry, "prune it")
        .expect_err("human review is not executable by maintenance");
    assert!(err.to_string().contains("human review"), "got: {err}");
    assert!(wt.path().exists(), "the worktree directory is untouched");

    // Re-run: no duplicate proposal.
    let again = maintenance
        .review_worktrees(&session, &[record])
        .expect("second review");
    assert!(again.proposed.is_empty());
    assert_eq!(again.already_proposed, 1);

    // Repository cleanliness: an untracked leftover is proposed once.
    let leftover = repo.path().join("leftover.tmp");
    std::fs::write(&leftover, "scratch").expect("leftover");
    let initialized = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(repo.path())
        .output()
        .expect("git init runs");
    assert!(initialized.status.success(), "git init must succeed");
    let first = maintenance
        .review_repo(&session, repo.path())
        .expect("repo review");
    assert_eq!(first.proposed.len(), 1, "the leftover is proposed");
    let second = maintenance
        .review_repo(&session, repo.path())
        .expect("second repo review");
    assert!(second.proposed.is_empty(), "same state proposes once");
    assert!(leftover.exists(), "maintenance removed nothing");
    session.end(&store).expect("ends");
}

/// Acceptance: "关 TUI 后维护停止，重启只恢复未完成记录，不盲重放删除等
/// 外部写" — after the session ends and a new one opens (a restart), the
/// open proposals are restored as records and nothing executes on its own.
#[test]
fn restart_restores_records_and_replays_nothing() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let maintenance = MaintenanceService::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");

    let item = make_item(&registry, dir.path(), "r", "Restart survivor");
    store
        .connection()
        .execute(
            "UPDATE knowledge_items SET updated_at = '2020-05-05T00:00:00Z' WHERE item_id = ?1",
            [&item],
        )
        .expect("age item");

    // First office window.
    let session = MaintenanceSession::open(&store).expect("session");
    maintenance
        .review_knowledge(&session, &registry, 100)
        .expect("review");
    session.end(&store).expect("window closes");

    // "Restart": a new session, new process pid.
    let session2 = MaintenanceSession::open(&store).expect("session 2");
    let restored = maintenance.open_proposals().expect("restore");
    assert_eq!(restored.len(), 1, "the pending record is restored");
    assert_eq!(restored[0].status, ProposalStatus::Open);

    // Nothing happened by itself: the item is still active and the
    // proposal still open — execution exists only as an explicit,
    // authorized call.
    let item_after = registry.get_item(&item).expect("item").expect("exists");
    assert_eq!(item_after.status, viva::knowledge::KnowledgeStatus::Active);
    assert_eq!(
        maintenance.open_proposals().expect("open").len(),
        1,
        "no replay, no auto-execution"
    );
    session2.end(&store).expect("ends");
}

/// Usage-evidenced skills are never flagged idle: the usage row is the
/// proof that silences the proposal.
#[test]
fn skills_with_real_usage_are_not_proposed() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let maintenance = MaintenanceService::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");

    let entry = dir.path().join("used-skill");
    std::fs::create_dir_all(&entry).expect("dir");
    std::fs::write(entry.join("SKILL.md"), "# used").expect("SKILL.md");
    let skill = registry
        .register_skill("Used skill", entry.join("SKILL.md"), "review", None, vec![])
        .expect("skill");

    // A real execution used it: usage evidence exists.
    registry
        .record_usage(&skill.item_id, &ExecutionId::new(), "used in a real run")
        .expect("usage");

    let session = MaintenanceSession::open(&store).expect("session");
    let report = maintenance
        .review_knowledge(&session, &registry, 365)
        .expect("review");
    assert!(
        report
            .proposed
            .iter()
            .all(|p| p.subject != skill.skill_id),
        "a used skill is not idle: {:?}",
        report.proposed
    );
    session.end(&store).expect("ends");
}
