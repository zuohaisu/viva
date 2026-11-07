//! #196: actual receiver binary preserves execution facts, not only panes.
use viva::authority::{AuthorityEngine, GrantMode};
use viva::foundation::{records::get_execution, store::Store};
use viva::members::{MemberBinding, MemberRegistry};
use viva::server::{self, Client, Host, RequestKind};
use viva::tasks::TaskRegistry;

#[test]
fn a_member_grant_cannot_restart_the_owner_host_even_with_member_omitted() {
    let home = tempfile::tempdir().unwrap();
    let host = Host::open(home.path()).unwrap();
    let shared = host.shared();
    let (member, grant) = {
        let store = shared.store.lock().unwrap();
        let member = MemberRegistry::new(&store)
            .register("grant caller")
            .unwrap();
        let grant = AuthorityEngine::new(&store)
            .issue_root_grant(
                Some(member.member_id.clone()),
                None,
                vec!["terminal_control".into()],
                GrantMode::ActWithApproval,
                None,
            )
            .unwrap();
        (member, grant)
    };
    let worker = host.serve_background();
    for attribution in [Some(member.member_id.to_string()), None] {
        let mut request = server::new_request(RequestKind::ServerRestart);
        request.member = attribution;
        request.grant = Some(grant.grant_id.to_string());
        let response = server::send_request(home.path(), request).unwrap();
        assert!(!response.ok);
        assert!(response.error.unwrap().contains("owner"));
    }
    let mut client = Client::connect(home.path()).unwrap();
    assert_eq!(client.call(RequestKind::Status).unwrap()["paused"], false);
    client
        .call(RequestKind::Shutdown { close_policy: None })
        .unwrap();
    worker.join().unwrap();
    let store = shared.store.lock().unwrap();
    let denials: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind = 'socket_grant_denied'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(denials, 2, "both refusals leave audit evidence");
}

#[test]
fn prepared_handoff_keeps_running_execution_and_its_attribution() {
    let home = tempfile::tempdir().unwrap();
    let host = Host::open(home.path()).unwrap();
    let shared = host.shared();
    let (task, member, grant) = {
        let store = shared.store.lock().unwrap();
        let members = MemberRegistry::new(&store);
        let member = members.register("handoff member").unwrap();
        members
            .set_binding(&MemberBinding {
                member_id: member.member_id.clone(),
                role: "developer".into(),
                model_binding: "fixture-model".into(),
                tools: vec!["shell".into()],
                updated_at: viva::foundation::ids::utc_now(),
            })
            .unwrap();
        let task = TaskRegistry::new(&store)
            .create_task(
                "keep this execution across a host replacement",
                vec![],
                Some(member.member_id.clone()),
                None,
                None,
            )
            .unwrap();
        let grant = AuthorityEngine::new(&store)
            .issue_root_grant(
                Some(member.member_id.clone()),
                Some(task.task_id.clone()),
                vec!["dispatch_delegated".into()],
                GrantMode::ActWithApproval,
                None,
            )
            .unwrap();
        (task, member, grant)
    };
    let worker = host.serve_background();
    let mut client = Client::connect(home.path()).unwrap();
    let dispatch = client
        .call(RequestKind::Dispatch {
            task_id: task.task_id.to_string(),
            member_id: member.member_id.to_string(),
            grant_id: grant.grant_id.to_string(),
            request_key: "qa196-running-execution".into(),
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "while :; do sleep 1; done".into(),
            ],
            cwd: home.path().to_string_lossy().into_owned(),
            worktree_id: None,
        })
        .unwrap();
    let execution_id = dispatch["execution_id"].as_str().unwrap().parse().unwrap();
    let before = {
        let store = shared.store.lock().unwrap();
        get_execution(&store, &execution_id).unwrap().unwrap()
    };
    assert_eq!(before.status.as_str(), "running");
    let first = server::restart_server_with(
        home.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_viva")),
    )
    .unwrap();
    worker.join().unwrap();
    // A resumed host must itself be replaceable: the second receiver binds
    // the other prepared slot instead of overwriting the first live socket.
    let second = server::restart_server_with(
        home.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_viva")),
    )
    .unwrap();
    let mut current = Client::connect(home.path()).unwrap();
    let after = {
        let store = Store::open(
            &viva::foundation::paths::database_path(home.path()),
            server::migrations(),
        )
        .unwrap();
        get_execution(&store, &execution_id).unwrap().unwrap()
    };
    // Cleanup before assertions, even if a regression changed the facts.
    current
        .call(RequestKind::Shutdown { close_policy: None })
        .unwrap();
    for pid in [first, second] {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut status = 0;
            // SAFETY: these are exact child PIDs spawned by this test's restart calls.
            let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
            if result == pid as i32 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "receiver child did not exit"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    assert_eq!(after.status.as_str(), "running");
    assert_eq!(after.attribution, before.attribution);
    assert_eq!(after.task_id, before.task_id);
    let store = Store::open(
        &viva::foundation::paths::database_path(home.path()),
        server::migrations(),
    )
    .unwrap();
    let stopped = get_execution(&store, &execution_id).unwrap().unwrap();
    assert_eq!(stopped.status.as_str(), "stopped");
    assert_ne!(
        TaskRegistry::new(&store)
            .require_task(&task.task_id)
            .unwrap()
            .status
            .as_str(),
        "done"
    );
}
