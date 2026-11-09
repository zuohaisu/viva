//! Backend-neutral intent/authorization tests. Requested records below are
//! preparation facts, NOT evidence of native spawn, PID continuity or success.
use std::path::Path;
use viva::{
    authority::{AuthorityEngine, GrantMode},
    foundation::{
        ids::utc_now,
        records::{ExecutionStatus, get_execution},
        store::Store,
    },
    members::{MemberBinding, MemberRegistry},
    tasks::{DispatchRequest, ExecutionStart, TaskRegistry},
};
fn binding(member: viva::foundation::ids::MemberId, role: &str) -> MemberBinding {
    MemberBinding {
        member_id: member,
        role: role.into(),
        model_binding: "fixture:model".into(),
        tools: vec!["fixture-tool".into()],
        updated_at: utc_now(),
    }
}
#[test]
fn prepare_without_a_host_preserves_attribution_replay_and_requires_live_authority() {
    let store = Store::open_in_memory(viva::server::migrations()).unwrap();
    let members = MemberRegistry::new(&store);
    let member = members.register("preparation fixture").unwrap();
    members
        .set_binding(&binding(member.member_id.clone(), "original role"))
        .unwrap();
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task(
            "no process launched",
            vec![],
            Some(member.member_id.clone()),
            None,
            None,
        )
        .unwrap();
    let authority = AuthorityEngine::new(&store);
    let grant = authority
        .issue_root_grant(
            Some(member.member_id.clone()),
            Some(task.task_id.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let request = || DispatchRequest {
        task_id: &task.task_id,
        member_id: &member.member_id,
        grant_id: &grant.grant_id,
        request_key: "prepare-key",
        cwd: Path::new("/tmp"),
        worktree_id: None,
    };
    let ExecutionStart::Created(first) = tasks.prepare_member_dispatch(request()).unwrap() else {
        panic!("expected fresh intent")
    };
    let id = first.execution_id.as_ref().unwrap();
    let recorded = get_execution(&store, id).unwrap().unwrap();
    assert_eq!(recorded.status, ExecutionStatus::Requested);
    assert_eq!(recorded.attribution.role, "original role");
    assert_eq!(first.pid, None);
    members
        .set_binding(&binding(member.member_id.clone(), "changed config role"))
        .unwrap();
    assert_eq!(
        tasks.prepare_member_dispatch(request()).unwrap(),
        ExecutionStart::Replayed(first.clone())
    );
    assert_eq!(
        get_execution(&store, id).unwrap().unwrap().attribution,
        recorded.attribution
    );
    assert_eq!(
        store
            .connection()
            .query_row("SELECT count(*) FROM executions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    authority
        .revoke(&grant.grant_id, "revoked before effect")
        .unwrap();
    assert!(
        tasks.prepare_member_dispatch(request()).is_err(),
        "replayed intent is not an authority lease"
    );
}
#[test]
fn replay_never_crosses_task_or_member_even_with_a_separately_valid_grant() {
    let store = Store::open_in_memory(viva::server::migrations()).unwrap();
    let members = MemberRegistry::new(&store);
    let a = members.register("A fixture").unwrap();
    let b = members.register("B fixture").unwrap();
    members
        .set_binding(&binding(a.member_id.clone(), "A role"))
        .unwrap();
    members
        .set_binding(&binding(b.member_id.clone(), "B role"))
        .unwrap();
    let tasks = TaskRegistry::new(&store);
    let t1 = tasks
        .create_task("task1", vec![], None, None, None)
        .unwrap();
    let t2 = tasks
        .create_task("task2", vec![], None, None, None)
        .unwrap();
    let authority = AuthorityEngine::new(&store);
    let prepare = |member: &viva::foundation::ids::MemberId,
                   task: &viva::foundation::ids::TaskId| {
        let grant = authority
            .issue_root_grant(
                Some(member.clone()),
                Some(task.clone()),
                vec!["dispatch_delegated".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .unwrap();
        tasks.prepare_member_dispatch(DispatchRequest {
            task_id: task,
            member_id: member,
            grant_id: &grant.grant_id,
            request_key: "shared-key",
            cwd: Path::new("/tmp"),
            worktree_id: None,
        })
    };
    assert!(matches!(
        prepare(&a.member_id, &t1.task_id).unwrap(),
        ExecutionStart::Created(_)
    ));
    assert!(prepare(&a.member_id, &t2.task_id).is_err());
    assert!(prepare(&b.member_id, &t1.task_id).is_err());
    assert_eq!(
        store
            .connection()
            .query_row("SELECT count(*) FROM launch_intents", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn invalid_location_unknown_worktree_and_finished_task_create_no_intents() {
    let store = Store::open_in_memory(viva::server::migrations()).unwrap();
    let members = MemberRegistry::new(&store);
    let member = members.register("location fixture").unwrap();
    members
        .set_binding(&binding(member.member_id.clone(), "fixture role"))
        .unwrap();
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task("fixture", vec![], None, None, None)
        .unwrap();
    let grant = AuthorityEngine::new(&store)
        .issue_root_grant(
            Some(member.member_id.clone()),
            Some(task.task_id.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let prepare = |cwd: &Path, worktree: Option<&str>| {
        tasks.prepare_member_dispatch(DispatchRequest {
            task_id: &task.task_id,
            member_id: &member.member_id,
            grant_id: &grant.grant_id,
            request_key: "invalid-key",
            cwd,
            worktree_id: worktree,
        })
    };
    assert!(prepare(Path::new("relative"), None).is_err());
    assert!(prepare(Path::new("/tmp"), Some("unknown-worktree")).is_err());
    store
        .connection()
        .execute("UPDATE tasks SET status='done'", [])
        .unwrap();
    assert!(prepare(Path::new("/tmp"), None).is_err());
    assert_eq!(
        store
            .connection()
            .query_row("SELECT count(*) FROM launch_intents", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
