//! Canonical authority fixtures, NOT native-process dispatch evidence.
//! The native transport broker/actuator must still connect these capabilities.
use viva::{
    authority::{AuthorityEngine, DenialReason, Grant, GrantMode},
    foundation::{
        envelope::{CallerIdentity, CallerRole, ChannelRegistry},
        ids::{ChannelId, MemberId, TaskId},
        records::{AttributionSnapshot, ExecutionRecord, ExecutionStatus, insert_execution},
        store::Store,
    },
    members::MemberRegistry,
    tasks::TaskRegistry,
};
struct Fixture {
    store: Store,
    channels: ChannelRegistry,
    caller: CallerIdentity,
    credential: String,
    member: MemberId,
    task: TaskId,
    grant: Grant,
}
fn fixture() -> Fixture {
    let store = Store::open_in_memory(viva::server::migrations()).unwrap();
    let member = MemberRegistry::new(&store)
        .register("worker binding fixture")
        .unwrap()
        .member_id;
    let task = TaskRegistry::new(&store)
        .create_task(
            "authority fixture, no process launched",
            vec![],
            Some(member.clone()),
            None,
            None,
        )
        .unwrap()
        .task_id;
    let mut execution = ExecutionRecord::start(
        task.clone(),
        AttributionSnapshot::capture(
            member.clone(),
            "fixture role",
            "fixture engine",
            "fixture tool",
        ),
    );
    execution.status = ExecutionStatus::Running;
    insert_execution(&store, &execution).unwrap();
    let mut channels = ChannelRegistry::new();
    let caller = channels.issue(CallerRole::MemberExecution(execution.execution_id.clone()));
    let authority = AuthorityEngine::new(&store);
    let credential = authority
        .issue_worker_credential(
            &caller.channel_id,
            Some(execution.execution_id),
            member.clone(),
        )
        .unwrap();
    let grant = authority
        .issue_root_grant(
            Some(member.clone()),
            Some(task.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    Fixture {
        store,
        channels,
        caller,
        credential,
        member,
        task,
        grant,
    }
}
fn check(
    f: &Fixture,
    caller: &CallerIdentity,
    grant: Option<viva::foundation::ids::GrantId>,
    task: &TaskId,
) -> Result<MemberId, DenialReason> {
    AuthorityEngine::new(&f.store)
        .check_bound_worker_action(
            &f.channels,
            caller,
            &f.credential,
            grant,
            "dispatch_delegated",
            Some(task),
        )
        .unwrap()
}
#[test]
fn host_channel_and_canonical_execution_resolve_member_then_require_live_grant() {
    let f = fixture();
    assert_eq!(
        check(&f, &f.caller, Some(f.grant.grant_id.clone()), &f.task),
        Ok(f.member.clone())
    );
    assert_eq!(
        check(&f, &f.caller, None, &f.task),
        Err(DenialReason::NoGrant)
    );
    assert_eq!(
        AuthorityEngine::new(&f.store)
            .check_bound_worker_action(
                &f.channels,
                &f.caller,
                &f.credential,
                Some(f.grant.grant_id.clone()),
                "merge_pull_request",
                Some(&f.task)
            )
            .unwrap(),
        Err(DenialReason::ProtectedAction)
    );
    AuthorityEngine::new(&f.store)
        .revoke(&f.grant.grant_id, "fixture revoked")
        .unwrap();
    assert_eq!(
        check(&f, &f.caller, Some(f.grant.grant_id.clone()), &f.task),
        Err(DenialReason::GrantNotLive)
    );
}
#[test]
fn forged_user_unknown_channel_and_cross_channel_credential_never_become_owner() {
    let mut f = fixture();
    let mut forged = f.caller.clone();
    forged.claimed_role = CallerRole::User;
    assert_eq!(
        check(&f, &forged, Some(f.grant.grant_id.clone()), &f.task),
        Err(DenialReason::UntrustedWorker)
    );
    let unknown = CallerIdentity {
        channel_id: ChannelId::new(),
        claimed_role: CallerRole::User,
    };
    assert_eq!(
        check(&f, &unknown, Some(f.grant.grant_id.clone()), &f.task),
        Err(DenialReason::UntrustedWorker)
    );
    for role in [CallerRole::User, CallerRole::AgentCli, CallerRole::TestRun] {
        let non_worker = f.channels.issue(role);
        assert_eq!(
            check(&f, &non_worker, Some(f.grant.grant_id.clone()), &f.task),
            Err(DenialReason::UntrustedWorker)
        );
    }
    let another = f.channels.issue(f.caller.claimed_role.clone());
    assert_eq!(
        check(&f, &another, Some(f.grant.grant_id.clone()), &f.task),
        Err(DenialReason::UntrustedWorker)
    );
    f.channels.revoke(&f.caller.channel_id).unwrap();
    assert_eq!(
        check(&f, &f.caller, Some(f.grant.grant_id.clone()), &f.task),
        Err(DenialReason::UntrustedWorker)
    );
}
#[test]
fn wrong_principal_cross_task_expired_grant_and_revoked_credential_refuse() {
    let f = fixture();
    let authority = AuthorityEngine::new(&f.store);
    let bob = MemberRegistry::new(&f.store)
        .register("another fixture member")
        .unwrap();
    let wrong = authority
        .issue_root_grant(
            Some(bob.member_id),
            Some(f.task.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    assert_eq!(
        check(&f, &f.caller, Some(wrong.grant_id), &f.task),
        Err(DenialReason::NotPrincipal)
    );
    assert_eq!(
        check(
            &f,
            &f.caller,
            Some(f.grant.grant_id.clone()),
            &TaskId::new()
        ),
        Err(DenialReason::CrossTask)
    );
    let expired = authority
        .issue_root_grant(
            Some(f.member.clone()),
            Some(f.task.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            Some("1970-01-01T00:00:00Z".into()),
        )
        .unwrap();
    assert_eq!(
        check(&f, &f.caller, Some(expired.grant_id), &f.task),
        Err(DenialReason::GrantExpired)
    );
    authority.revoke_worker_credential(&f.credential).unwrap();
    assert_eq!(
        check(&f, &f.caller, Some(f.grant.grant_id.clone()), &f.task),
        Err(DenialReason::UntrustedWorker)
    );
}
#[test]
fn ended_execution_or_mismatched_member_is_not_a_live_worker_binding() {
    let f = fixture();
    f.store
        .connection()
        .execute("UPDATE executions SET status='stopped'", [])
        .unwrap();
    assert_eq!(
        check(&f, &f.caller, Some(f.grant.grant_id.clone()), &f.task),
        Err(DenialReason::UntrustedWorker)
    );
    let f = fixture();
    let other = MemberRegistry::new(&f.store)
        .register("credential mismatch fixture")
        .unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE worker_credentials SET member_id=?1",
            [other.member_id.as_str()],
        )
        .unwrap();
    assert_eq!(
        check(&f, &f.caller, Some(f.grant.grant_id.clone()), &f.task),
        Err(DenialReason::UntrustedWorker)
    );
}
