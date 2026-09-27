//! V11 acceptance tests (issue #20): provenance enforcement, scope
//! isolation, exit paths with preserved history, usage-evidence-only
//! reuse claims, skill lifecycle without touching bodies, bounded
//! selection and provenance-preserving export.

use std::path::{Path, PathBuf};

use viva::foundation::ids::{ExecutionId, MemberId, ProjectId};
use viva::foundation::store::{
    DOMAIN_FOUNDATION, DOMAIN_KNOWLEDGE, FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::knowledge::{
    ExternalState, KnowledgeRegistry, Scope, SelectionContext, SelectionOptions,
};

fn frozen() -> viva::foundation::store::FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(
            DOMAIN_KNOWLEDGE,
            1,
            "knowledge v1",
            viva::knowledge::KNOWLEDGE_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

fn store() -> Store {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("store");
    std::mem::forget(dir); // paths live in rows; keep the dir for the test
    store
}

fn write_body(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write body");
    path.canonicalize().expect("canonical")
}

/// Acceptance: "无 provenance 拒绝".
#[test]
fn entries_without_provenance_are_rejected() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");
    let body = write_body(dir.path(), "body.md", "some note");

    assert!(
        registry
            .record(
                "A note",
                Scope::Team,
                &body,
                "   ",
                None,
                None,
                vec![],
                None,
                ExternalState::None
            )
            .is_err(),
        "empty provenance must be rejected"
    );
    assert_eq!(store.row_count("knowledge_items").expect("count"), 0);

    registry
        .record(
            "A note",
            Scope::Team,
            body,
            "from the 2026-09 design review with Haisu",
            None,
            None,
            vec![],
            None,
            ExternalState::None,
        )
        .expect("with provenance it records");
}

/// Acceptance: "个人知识不泄漏给其他成员任务；项目/团队可见范围测试覆盖".
#[test]
fn scope_boundaries_keep_personal_knowledge_private() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");

    let samuel = MemberId::new();
    let rook = MemberId::new();
    let project_viva = ProjectId::new();
    let project_other = ProjectId::new();

    registry
        .record(
            "Samuel's private note",
            Scope::Personal,
            write_body(dir.path(), "private.md", "private thoughts"),
            "Samuel's journal",
            Some(samuel.clone()),
            None,
            vec!["private".into()],
            None,
            ExternalState::None,
        )
        .expect("personal");
    registry
        .record(
            "Viva build steps",
            Scope::Project,
            write_body(dir.path(), "build.md", "cargo build --release"),
            "repo README",
            None,
            Some(project_viva.clone()),
            vec!["build".into()],
            None,
            ExternalState::None,
        )
        .expect("project");
    registry
        .record(
            "Office charter",
            Scope::Team,
            write_body(dir.path(), "charter.md", "session dies, resident persists"),
            "IDEA.md",
            None,
            None,
            vec!["charter".into()],
            None,
            ExternalState::None,
        )
        .expect("team");

    // Samuel's selection sees his personal entry + the team entry, not Rook
    // seeing Samuel's.
    let samuel_view = registry
        .select_for_context(
            &SelectionContext {
                viewer_member_id: samuel.clone(),
                project_id: Some(project_viva.clone()),
            },
            &[],
            SelectionOptions::default(),
        )
        .expect("select");
    let titles: Vec<&str> = samuel_view.iter().map(|e| e.title.as_str()).collect();
    assert!(
        titles.contains(&"Samuel's private note"),
        "own personal entry visible: {titles:?}"
    );
    assert!(titles.contains(&"Office charter"));

    let rook_view = registry
        .select_for_context(
            &SelectionContext {
                viewer_member_id: rook.clone(),
                project_id: Some(project_viva.clone()),
            },
            &[],
            SelectionOptions::default(),
        )
        .expect("select");
    let titles: Vec<&str> = rook_view.iter().map(|e| e.title.as_str()).collect();
    assert!(
        !titles.contains(&"Samuel's private note"),
        "another member's personal entry must never leak: {titles:?}"
    );
    assert!(titles.contains(&"Office charter"), "team scope is shared");
    assert!(
        titles.contains(&"Viva build steps"),
        "matching project scope is shared"
    );

    // A different project does not see the viva project entry.
    let other_view = registry
        .select_for_context(
            &SelectionContext {
                viewer_member_id: rook.clone(),
                project_id: Some(project_other.clone()),
            },
            &[],
            SelectionOptions::default(),
        )
        .expect("select");
    let titles: Vec<&str> = other_view.iter().map(|e| e.title.as_str()).collect();
    assert!(
        !titles.contains(&"Viva build steps"),
        "project scope is scoped: {titles:?}"
    );
}

/// Acceptance: "归档或失效条目默认不进入工作上下文，按明确请求仍能找回；
/// 修正/撤回保留历史与原因".
#[test]
fn exit_paths_remove_from_default_selection_but_history_survives() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");
    let member = MemberId::new();

    let item = registry
        .record(
            "Outdated API notes",
            Scope::Personal,
            write_body(dir.path(), "api.md", "old api"),
            "recorded from the old repo wiki",
            Some(member.clone()),
            None,
            vec![],
            None,
            ExternalState::None,
        )
        .expect("record");

    // Present by default.
    let view = registry
        .select_for_context(
            &SelectionContext {
                viewer_member_id: member.clone(),
                project_id: None,
            },
            &[],
            SelectionOptions::default(),
        )
        .expect("select");
    assert_eq!(view.len(), 1);

    // Archive with a reason.
    registry
        .archive(&item.item_id, "superseded by the new wiki page", "curator")
        .expect("archive");

    // Gone from the default selection…
    let view = registry
        .select_for_context(
            &SelectionContext {
                viewer_member_id: member.clone(),
                project_id: None,
            },
            &[],
            SelectionOptions::default(),
        )
        .expect("select");
    assert!(
        view.is_empty(),
        "archived entries leave the default selection"
    );

    // …but findable by explicit request.
    let view = registry
        .select_for_context(
            &SelectionContext {
                viewer_member_id: member.clone(),
                project_id: None,
            },
            &[],
            SelectionOptions {
                include_inactive: true,
                ..SelectionOptions::default()
            },
        )
        .expect("select");
    assert_eq!(view.len(), 1, "explicit request still finds it");

    // History + reason preserved.
    let lifecycle = registry.lifecycle(&item.item_id).expect("lifecycle");
    assert_eq!(lifecycle.len(), 1);
    assert_eq!(lifecycle[0].action, "archived");
    assert_eq!(lifecycle[0].reason, "superseded by the new wiki page");
    assert_eq!(lifecycle[0].actor, "curator");

    // Restore returns it, and both events remain.
    registry
        .restore(&item.item_id, "the wiki page was reverted", "curator")
        .expect("restore");
    assert_eq!(
        registry.lifecycle(&item.item_id).expect("lifecycle").len(),
        2
    );
}

/// Acceptance: "只有后续实际 execution 使用记录才报告复用；退出 Pi/新 journal
/// 不自动显示'学到'".
#[test]
fn reuse_is_claimed_only_through_real_usage_evidence() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");
    let member = MemberId::new();

    let item = registry
        .record(
            "Deploy checklist",
            Scope::Personal,
            write_body(dir.path(), "deploy.md", "1. build 2. test 3. ship"),
            "compiled by Samuel during V01",
            Some(member.clone()),
            None,
            vec![],
            None,
            ExternalState::None,
        )
        .expect("record");

    // No usage rows exist: nothing may claim reuse. Selecting or archiving
    // does not fabricate usage either.
    assert!(registry.usage_of(&item.item_id).expect("usage").is_empty());
    registry
        .archive(&item.item_id, "temp archive during cleanup", "curator")
        .expect("archive");
    assert!(registry.usage_of(&item.item_id).expect("usage").is_empty());
    registry
        .restore(&item.item_id, "cleanup finished", "curator")
        .expect("restore");

    // A real execution reports usage; only then does reuse exist.
    let execution = ExecutionId::new();
    registry
        .record_usage(
            &item.item_id,
            &execution,
            "checklist applied during the deploy",
        )
        .expect("record usage");
    let usage = registry.usage_of(&item.item_id).expect("usage");
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].execution_id, execution);
}

/// Acceptance: "技能可登记/禁用/归档而不删除用户正文" and "SKILL.md 是正文
/// 权威来源，SQLite 管索引/状态".
#[test]
fn skill_lifecycle_never_touches_the_body_file() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");
    let body_path = write_body(
        dir.path(),
        "SKILL.md",
        "# rust-migration-skill\nHow to move a module safely.",
    );
    let original = std::fs::read_to_string(&body_path).expect("read");

    let member = MemberId::new();
    let skill = registry
        .register_skill(
            "rust-migration-skill",
            body_path.clone(),
            "authored by Haisu, stored in the skills folder",
            Some(member.clone()),
            vec!["rust".into()],
        )
        .expect("register");

    // Enabled by default, listed, then disabled — office state only.
    assert!(registry.list_skills(true).expect("skills").len() == 1);
    registry
        .set_skill_enabled(&skill.skill_id, false)
        .expect("disable");
    assert!(registry.list_skills(true).expect("enabled list").is_empty());
    assert_eq!(
        registry.list_skills(false).expect("all list").len(),
        1,
        "history kept"
    );
    let disabled = registry
        .skill(&skill.skill_id)
        .expect("skill")
        .expect("exists");
    assert!(!disabled.enabled);
    assert!(disabled.disabled_at.is_some());

    // Archive the underlying item; the file is still byte-identical.
    registry
        .archive(&skill.item_id, "skill retired for now", "curator")
        .expect("archive");
    let after = std::fs::read_to_string(&body_path).expect("read");
    assert_eq!(
        after, original,
        "SKILL.md is the content authority; the DB never rewrites it"
    );
}

/// Acceptance: "相关资料选择不全量载入" and external references surface as
/// explicitly unavailable.
#[test]
fn selection_is_budget_bounded_and_external_refs_stay_honest() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");
    let member = MemberId::new();

    // 50 related entries, each ~2KB: full load would be ~100KB.
    for i in 0..50 {
        registry
            .record(
                format!("deploy note {i}"),
                Scope::Personal,
                write_body(dir.path(), &format!("note{i}.md"), &"x".repeat(2048)),
                format!("collected from runbook pass {i}"),
                Some(member.clone()),
                None,
                vec!["deploy".into()],
                None,
                ExternalState::None,
            )
            .expect("record");
    }
    // One entry backed by an unverified external memory reference.
    registry
        .record(
            "holographic memory snapshot",
            Scope::Personal,
            write_body(dir.path(), "external.md", "unused placeholder body"),
            "user-mentioned Hermes holographic handle",
            Some(member.clone()),
            None,
            vec!["memory".into()],
            Some("holo://samuel/2026-09-snapshot".into()),
            ExternalState::Unavailable,
        )
        .expect("record external");

    let selected = registry
        .select_for_context(
            &SelectionContext {
                viewer_member_id: member.clone(),
                project_id: None,
            },
            &["deploy"],
            SelectionOptions {
                max_total_bytes: 10 * 1024,
                include_inactive: false,
            },
        )
        .expect("select");

    let total: usize = selected.iter().map(|e| e.bytes()).sum();
    assert!(
        total <= 10 * 1024,
        "selection respects the byte budget: {total}"
    );
    assert!(
        selected.len() < 50,
        "selection is a minimal relevant subset, not a full load: {}",
        selected.len()
    );
    assert!(
        selected.iter().all(|e| e.title.contains("deploy")),
        "term matching prefers related entries"
    );

    // The external reference, when selected explicitly, comes back with no
    // body and a visible unavailable reason — never as content.
    let selected = registry
        .select_for_context(
            &SelectionContext {
                viewer_member_id: member.clone(),
                project_id: None,
            },
            &["holographic", "memory"],
            SelectionOptions::default(),
        )
        .expect("select");
    let external = selected
        .iter()
        .find(|e| e.title == "holographic memory snapshot")
        .expect("external entry selectable");
    assert!(external.body.is_none());
    assert!(
        external
            .body_unavailable_reason
            .as_deref()
            .is_some_and(|r| r.contains("not verified"))
    );
    assert_eq!(
        external.external_state,
        viva::knowledge::ExternalState::Unavailable
    );
}

/// Acceptance: "导出/备份保留来源与引用".
#[test]
fn export_preserves_provenance_references_and_history() {
    let store = store();
    let registry = KnowledgeRegistry::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");
    let member = MemberId::new();
    let execution = ExecutionId::new();

    let item = registry
        .record(
            "Launch parameters",
            Scope::Personal,
            write_body(dir.path(), "launch.md", "argv not shell strings"),
            "ADR 0011 §2",
            Some(member.clone()),
            None,
            vec!["launch".into()],
            None,
            ExternalState::None,
        )
        .expect("record");
    registry
        .record_usage(&item.item_id, &execution, "used while writing V05")
        .expect("usage");
    registry
        .archive(&item.item_id, "replaced by the ops page", "curator")
        .expect("archive");

    let exported = registry.export_json().expect("export");
    let entry = &exported["entries"][0];
    assert_eq!(
        entry["item"]["provenance"], "ADR 0011 §2",
        "provenance travels"
    );
    assert_eq!(entry["item"]["status"], "archived");
    assert_eq!(
        entry["body"], "argv not shell strings",
        "body comes from the file"
    );
    assert_eq!(entry["lifecycle"].as_array().expect("lifecycle").len(), 1);
    assert_eq!(entry["usage"].as_array().expect("usage").len(), 1);

    // Round-trips through serde (backup file shape).
    let text = serde_json::to_string(&exported).expect("serialize");
    let back: serde_json::Value = serde_json::from_str(&text).expect("deserialize");
    assert_eq!(
        back["entries"][0]["item"]["item_id"],
        entry["item"]["item_id"]
    );
}
