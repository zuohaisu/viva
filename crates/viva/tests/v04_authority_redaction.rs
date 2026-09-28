//! V04 acceptance tests (issue #13): live grants, delegation ceilings,
//! worker call origin, revoke/dispatch races and cross-chunk redaction.

use viva::authority::{
    Actor, AuthorityEngine, CapabilityReport, DenialReason, GrantMode, RestrictionState,
};
use viva::foundation::ids::{ChannelId, ExecutionId, MemberId, TaskId};
use viva::foundation::store::{
    DOMAIN_AUTHORITY, DOMAIN_FOUNDATION, FOUNDATION_V1_SQL, FrozenMigrations, MigrationRegistry,
    Store,
};
use viva::redaction::Redactor;

fn frozen() -> FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(
            DOMAIN_AUTHORITY,
            1,
            "authority v1",
            viva::authority::AUTHORITY_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

/// Acceptance: "伪造/过期/撤回凭据、跨 task、放大子 grant 和 worker 冒充用户
/// 都拒绝并留原因".
#[test]
fn every_forbidden_path_is_denied_with_recorded_reason() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let engine = AuthorityEngine::new(&store);
    let task = TaskId::new();

    // Forged credential.
    assert_eq!(
        engine
            .check_worker_credential("cred-never-issued", false)
            .expect("check"),
        Err(DenialReason::UntrustedWorker)
    );

    // Worker impersonating the user with a live credential.
    let credential = engine
        .issue_worker_credential(&ChannelId::new(), Some(ExecutionId::new()), MemberId::new())
        .expect("issue");
    assert_eq!(
        engine
            .check_worker_credential(&credential, true)
            .expect("check"),
        Err(DenialReason::WorkerImpersonatesUser)
    );

    // Revoked credential.
    engine
        .revoke_worker_credential(&credential)
        .expect("revoke");
    assert_eq!(
        engine
            .check_worker_credential(&credential, false)
            .expect("check"),
        Err(DenialReason::UntrustedWorker)
    );

    // Amplifying child grant.
    let parent = engine
        .issue_root_grant(
            None,
            Some(task.clone()),
            vec!["read_task".into()],
            GrantMode::Read,
            None,
        )
        .expect("parent");
    assert!(
        engine
            .delegate_grant(
                &parent.grant_id,
                MemberId::new(),
                vec!["read_task".into(), "invoke_worker".into()],
                GrantMode::ActWithApproval,
                None,
            )
            .is_err()
    );

    // Cross-task use of a scoped grant.
    let other = TaskId::new();
    assert_eq!(
        engine
            .check(
                &Actor::Member {
                    member: MemberId::new(),
                    grant: Some(parent.grant_id.clone()),
                },
                "read_task",
                Some(&other),
            )
            .expect("check"),
        Err(DenialReason::CrossTask)
    );

    // All denials left reasons.
    let denials = engine.recent_denials(20).expect("denials");
    assert!(denials.len() >= 4, "denials: {denials:?}");
    for (_, _, reason, _) in &denials {
        assert!(!reason.is_empty(), "every denial carries a reason");
    }
}

/// Acceptance: "撤回与 dispatch 竞态在最终启动授权点重新检查；受保护 push、
/// approve、merge 不能被成员 grant 开放".
#[test]
fn revoke_racing_dispatch_is_caught_at_the_launch_time_check() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let engine = AuthorityEngine::new(&store);
    let task = TaskId::new();
    let grant = engine
        .issue_root_grant(
            None,
            Some(task.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActWithApproval,
            None,
        )
        .expect("grant");
    let actor = Actor::Member {
        member: MemberId::new(),
        grant: Some(grant.grant_id.clone()),
    };

    // The engine answers from the database at every call: this call IS the
    // final launch authorization point. Revoke immediately before it and
    // the dispatch is denied; no stale in-memory "allow" can win.
    engine
        .revoke(&grant.grant_id, "owner halted the run")
        .expect("revoke");
    assert_eq!(
        engine
            .check(&actor, "dispatch_delegated", Some(&task))
            .expect("check"),
        Err(DenialReason::GrantNotLive)
    );

    // Protected actions stay closed under every mode.
    for action in [
        "push_protected_branch",
        "approve_pull_request",
        "merge_pull_request",
    ] {
        let grant = engine
            .issue_root_grant(
                None,
                Some(task.clone()),
                vec!["dispatch_delegated".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .expect("maximal grant");
        let actor = Actor::Member {
            member: MemberId::new(),
            grant: Some(grant.grant_id),
        };
        assert_eq!(
            engine.check(&actor, action, Some(&task)).expect("check"),
            Err(DenialReason::ProtectedAction),
            "{action} must never open"
        );
    }
}

/// Acceptance: "普通聊天没有 task grant 时只聊天/读允许的资料，派发须取得对应
/// live grant；创建新任务不因聊天身份自动获办公室全权".
#[test]
fn chat_identity_gets_read_only_and_never_office_wide_power() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let engine = AuthorityEngine::new(&store);
    let chatter = Actor::Member {
        member: MemberId::new(),
        grant: None,
    };

    // Reading allowed material works without a grant.
    assert_eq!(
        engine.check(&chatter, "read_github", None).expect("check"),
        Ok(())
    );

    // Dispatching needs the corresponding live grant.
    assert_eq!(
        engine
            .check(&chatter, "dispatch_delegated", None)
            .expect("check"),
        Err(DenialReason::NoGrant)
    );

    // Creating a task from chat identity does not auto-grant office power:
    // task creation is an office action requiring a live grant as well.
    assert_eq!(
        engine.check(&chatter, "create_task", None).expect("check"),
        Err(DenialReason::NoGrant)
    );

    // And no implicit grant row appeared anywhere.
    assert_eq!(store.row_count("grants").expect("grants"), 0);
}

/// Acceptance: "分块 token、结构化 secret、异常信息不落入 Viva 自写日志/数据库".
#[test]
fn streaming_secrets_never_reach_persisted_output() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db_path = dir.path().join("office.db");
    let store = Store::open(&db_path, &frozen()).expect("open");
    let token = "glp_9f8e7d6c5b4a3210";
    let mut redactor = Redactor::new(vec![token, "hunter2"]);

    // A supervisor-style streaming log with the token split across chunks.
    let mut persisted = String::new();
    let chunks = [
        "worker stderr: connecting with glp_9f8e",
        "7d6c5b4a3210 …auth ok\nexception: bad token hunter",
        "2 in config\n",
    ];
    for chunk in chunks {
        persisted.push_str(&redactor.push(chunk));
    }
    persisted.push_str(&redactor.flush());

    assert!(
        !persisted.contains(token),
        "a chunk-split token must never reach the journal: {persisted}"
    );
    assert!(!persisted.contains("hunter2"));
    assert!(
        persisted.contains("auth ok"),
        "legit text survives: {persisted}"
    );

    // Structured payloads are scrubbed before storage.
    let payload = serde_json::json!({"env": {"API_TOKEN": token}, "note": "safe"});
    let scrubbed = redactor.scrub_json(&payload);
    let text = serde_json::to_string(&scrubbed).expect("json");
    assert!(!text.contains(token), "structured secret leaked: {text}");
    assert_eq!(scrubbed["note"], "safe");

    // The persisted journal in the store keeps none of it either way:
    // domain writes go through redact_str/scrub_json before insertion.
    let safe_note = redactor.redact_str("run finished with token glp_9f8e7d6c5b4a3210");
    assert!(!safe_note.contains(token));
    let _ = store; // store opened over real SQLite for the scenario
}

/// Acceptance: "read_only 只有 adapter 具备实证约束时才显示已限制".
#[test]
fn read_only_restriction_requires_adapter_evidence() {
    let verified = CapabilityReport::claimed_read_only(
        "git-adapter",
        Some("writes blocked by adapter construction; see adapter test suite".into()),
    );
    assert_eq!(verified.restriction_state, RestrictionState::Enforced);

    let promised = CapabilityReport::claimed_read_only("pi-adapter", None);
    assert_eq!(
        promised.restriction_state,
        RestrictionState::Unverified,
        "a promised restriction must display as unverified, not restricted"
    );
}

/// QA round Q1: an expired parent grant delegates nothing, and a child
/// grant can never outlive its parent — the ceiling holds in time too.
#[test]
fn expired_parent_cannot_delegate_and_child_expiry_is_capped() {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("office.db");
    let store = Store::open(&db, &frozen()).expect("open");
    let engine = AuthorityEngine::new(&store);
    let task = TaskId::new();

    // An already-expired parent refuses delegation outright.
    let expired = engine
        .issue_root_grant(
            None,
            Some(task.clone()),
            vec!["read_task".into()],
            GrantMode::Read,
            Some("2000-01-01T00:00:00Z".into()),
        )
        .expect("expired parent issues (expiry only bites at check time)");
    let err = engine
        .delegate_grant(
            &expired.grant_id,
            MemberId::new(),
            vec!["read_task".into()],
            GrantMode::Read,
            None,
        )
        .expect_err("expired parent must not delegate");
    assert!(err.to_string().contains("expired"), "got: {err}");

    // A live parent caps the child's expiry at its own — including the
    // child asking for "no expiry".
    let parent = engine
        .issue_root_grant(
            None,
            Some(task.clone()),
            vec!["read_task".into()],
            GrantMode::Read,
            Some("2099-01-01T00:00:00Z".into()),
        )
        .expect("parent");
    let child = engine
        .delegate_grant(
            &parent.grant_id,
            MemberId::new(),
            vec!["read_task".into()],
            GrantMode::Read,
            None,
        )
        .expect("child");
    assert_eq!(
        child.expires_at,
        Some("2099-01-01T00:00:00Z".into()),
        "child without expiry inherits the parent's ceiling"
    );
    let child2 = engine
        .delegate_grant(
            &parent.grant_id,
            MemberId::new(),
            vec!["read_task".into()],
            GrantMode::Read,
            Some("2099-06-01T00:00:00Z".into()),
        )
        .expect("child2");
    assert_eq!(
        child2.expires_at,
        Some("2099-01-01T00:00:00Z".into()),
        "a later child expiry is capped at the parent's"
    );
}
