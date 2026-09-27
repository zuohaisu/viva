//! V02 acceptance tests (issue #11): members, model/tool bindings and
//! multi-project context over real SQLite, reloaded across a full reopen.

use std::path::PathBuf;
use std::time::Duration;

use tempfile::TempDir;

use viva::foundation::store::{
    DOMAIN_FOUNDATION, DOMAIN_MEMBERS, DOMAIN_WORKSPACES_PROJECTS, FOUNDATION_V1_SQL,
    MigrationRegistry, Store,
};
use viva::members::{MemberBinding, MemberRegistry, ModelPolicy};
use viva::projects::{ProjectRegistry, ReferenceKind};
use viva::workspaces::WorkspaceRegistry;

fn frozen() -> viva::foundation::store::FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(
            DOMAIN_MEMBERS,
            1,
            "members v1",
            viva::members::MEMBERS_V1_SQL,
        )
        .register(
            DOMAIN_WORKSPACES_PROJECTS,
            1,
            "workspaces and projects v1",
            viva::workspaces::WORKSPACES_PROJECTS_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

/// Acceptance: "改角色/模型/工具并重启后，成员 ID/历史/知识引用不变".
#[test]
fn rebinding_across_restart_keeps_member_identity_stable() {
    let dir = TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let migrations = frozen();

    let (member_id, created_at) = {
        let store = Store::open(&db, &migrations).expect("open");
        let registry = MemberRegistry::new(&store);
        let member = registry.register("Samuel").expect("register");
        registry
            .set_binding(&MemberBinding {
                member_id: member.member_id.clone(),
                role: "developer".into(),
                model_binding: "glm-5.3-flash".into(),
                tools: vec!["pi-cli".into()],
                updated_at: viva::foundation::ids::utc_now(),
            })
            .expect("binding");
        (member.member_id.clone(), member.created_at.clone())
    };

    // Restart: change role/model/tools in a fresh process-shaped store.
    {
        let store = Store::open(&db, &migrations).expect("reopen");
        let registry = MemberRegistry::new(&store);
        registry
            .set_binding(&MemberBinding {
                member_id: member_id.clone(),
                role: "reviewer".into(),
                model_binding: "glm-5.3-air".into(),
                tools: vec![],
                updated_at: viva::foundation::ids::utc_now(),
            })
            .expect("rebinding");
        let member = registry.require(&member_id).expect("member survives");
        assert_eq!(member.created_at, created_at, "creation history unchanged");
        let binding = registry.binding(&member_id).expect("binding").expect("set");
        assert_eq!(
            (binding.role.as_str(), binding.tools.len()),
            ("reviewer", 0)
        );
    }
}

/// Acceptance: "两个不在同一父目录的项目和临时 reference 可选择/停用，停用不删除历史；
/// 普通目录可打开".
#[test]
fn multi_project_context_with_references_and_plain_directories() {
    let dir = TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let workspaces = WorkspaceRegistry::new(&store);
    let projects = ProjectRegistry::new(&store);

    let workspace = workspaces.create("Haisu's office").expect("workspace");
    let repo_a = dir.path().join("code").join("viva");
    let repo_b = dir.path().join("elsewhere").join("VicTrader");
    std::fs::create_dir_all(&repo_a).expect("mkdir a");
    std::fs::create_dir_all(&repo_b).expect("mkdir b");

    let project_a = projects
        .register(Some(workspace.workspace_id.clone()), "viva", &repo_a)
        .expect("project a");
    let project_b = projects
        .register(Some(workspace.workspace_id.clone()), "vic", &repo_b)
        .expect("project b");
    assert_ne!(
        repo_a.parent(),
        repo_b.parent(),
        "the scenario requires projects in different parents"
    );

    // A temporary reference edge is explicit office bookkeeping.
    let reference_target = dir.path().join("shared-asset");
    std::fs::create_dir_all(&reference_target).expect("mkdir ref");
    projects
        .add_reference(
            &project_a.project_id,
            &reference_target,
            ReferenceKind::Reference,
            "temporary scratch reference",
        )
        .expect("add reference");
    assert_eq!(
        projects
            .references(&project_a.project_id)
            .expect("refs")
            .len(),
        1
    );

    // Deactivation keeps the row and its history queryable.
    projects
        .deactivate(&project_b.project_id)
        .expect("deactivate");
    assert!(
        projects.require(&project_b.project_id).is_ok(),
        "history survives"
    );
    let active: Vec<_> = projects
        .list(true)
        .expect("active")
        .into_iter()
        .map(|p| p.project_id)
        .collect();
    assert_eq!(active, vec![project_a.project_id.clone()]);

    // A plain directory opens without any registration.
    let plain = dir.path().join("plain-dir");
    std::fs::create_dir_all(&plain).expect("mkdir plain");
    let opened = projects
        .open_plain_directory(&plain)
        .expect("plain directory opens");
    assert_eq!(opened.path, plain);
    assert_eq!(store.row_count("projects").expect("count"), 2);
}

/// Acceptance: "工具探测采用显式 argv、超时与有界输出；可缓存并刷新" and
/// "缺工具/不支持模型/不支持能力给出可行动失败".
#[test]
fn tool_probes_are_real_cached_and_refreshable() {
    let dir = TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let registry = MemberRegistry::new(&store);
    let member = registry.register("Samuel").expect("register");
    registry
        .set_binding(&MemberBinding {
            member_id: member.member_id.clone(),
            role: "developer".into(),
            model_binding: "glm-5.3-flash".into(),
            tools: vec!["echo".into()],
            updated_at: viva::foundation::ids::utc_now(),
        })
        .expect("binding");

    // Config string ≠ verified capability: a missing tool probes unavailable.
    let missing = registry
        .probe_tool(
            vec!["viva-no-such-tool-xyz".into()],
            Duration::from_secs(2),
            1024,
            false,
        )
        .expect("probe");
    assert!(!missing.available);
    assert!(missing.detail.contains("cannot be launched"));

    // A real tool answers, the result is cached, and refresh re-runs it.
    let ok = registry
        .probe_tool(
            vec!["/bin/echo".into(), "probe".into()],
            Duration::from_secs(5),
            64,
            false,
        )
        .expect("probe");
    assert!(ok.available);
    let cached = registry
        .probe_tool(
            vec!["/bin/echo".into(), "probe".into()],
            Duration::from_secs(5),
            64,
            false,
        )
        .expect("cached");
    assert_eq!(cached, ok);
    let refreshed = registry
        .probe_tool(
            vec!["/bin/echo".into(), "probe".into()],
            Duration::from_secs(5),
            64,
            true,
        )
        .expect("refreshed");
    assert!(refreshed.available);

    // An unsupported model binding fails with an actionable message.
    let err = registry
        .ensure_model_supported(
            &member.member_id,
            &ModelPolicy::new(vec!["glm-5.3-air".into()]),
        )
        .expect_err("unsupported model");
    assert!(err.to_string().contains("glm-5.3-air"), "got: {err}");
}

/// Acceptance: "领域数据读写与导出经真实 SQLite 测试，其他 issue 不需改本目录即可消费查询结果".
#[test]
fn domain_data_exports_and_reloads_from_disk() {
    let dir = TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let exported_json = {
        let store = Store::open(&db, &frozen()).expect("open");
        let projects = ProjectRegistry::new(&store);
        let project = projects
            .register(None, "viva", "/Users/hzuo/Documents/code/viva")
            .expect("register");
        projects
            .add_reference(
                &project.project_id,
                "/tmp/sib",
                ReferenceKind::Dependency,
                "d",
            )
            .expect("ref");
        serde_json::to_string(&projects.export_json().expect("export")).expect("serialize")
    };

    // Reopen from disk: the export reflects persisted state, not memory.
    let store = Store::open(&db, &frozen()).expect("reopen");
    let exported: serde_json::Value = serde_json::from_str(
        &ProjectRegistry::new(&store)
            .export_json()
            .expect("export")
            .to_string(),
    )
    .expect("json");
    let _ = exported_json;
    assert_eq!(
        exported["projects"].as_array().expect("projects").len(),
        1,
        "project row persisted"
    );
    assert_eq!(
        store.row_count("project_references").expect("count"),
        1,
        "reference edge persisted"
    );
    let _ = PathBuf::new(); // keep import used on all platforms
}
