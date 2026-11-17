#[test]
fn actual_member_requests_recheck_grants_and_cannot_self_authorize() {
    use viva::foundation::ids::{GrantId, MemberId, TaskId};
    let temp = tempfile::Builder::new().prefix("p2").tempdir().unwrap();
    let home = temp.path();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let coordinator: MemberId = member(&store, "coordinator").parse().unwrap();
    let worker: MemberId = member(&store, "worker").parse().unwrap();
    let tasks = TaskRegistry::new(&store);
    let source_task = tasks
        .create_task("source", vec![], None, None, None)
        .unwrap();
    let target_task = tasks
        .create_task("target", vec![], None, None, None)
        .unwrap();
    let engine = AuthorityEngine::new(&store);
    let base = engine
        .issue_root_grant(
            Some(coordinator.clone()),
            None,
            vec!["dispatch_delegated".into(), "terminal_control".into(), "member_report".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let mut host = start(home);
    let command = home.join("fixture-command.sh");
    let status = home.join("fixture-status");
    let token_file = home.join("fixture-token");
    let script = format!(
        "umask 077; printf '%s' \"$VIVA_MEMBER_CREDENTIAL\" > '{}'; while :; do if [ -f '{}' ]; then /bin/sh '{}' > '{}/fixture-out' 2> '{}/fixture-err'; rc=$?; rm '{}'; printf '%s' \"$rc\" > '{}'; fi; sleep 0.02; done",
        token_file.display(),
        command.display(),
        command.display(),
        home.display(),
        home.display(),
        command.display(),
        status.display()
    );
    let parent = launch(
        home,
        source_task.task_id.as_str(),
        coordinator.as_str(),
        base.grant_id.as_str(),
        "broker",
        &script,
    );
    wait("member fixture token", || {
        token_file.metadata().is_ok_and(|m| m.len() > 0)
    });
    let admissions_before: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_client_admitted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let invoke = |script: &str| {
        let temporary = command.with_extension("tmp");
        std::fs::write(&temporary, script).unwrap();
        std::fs::rename(temporary, &command).unwrap();
        wait("member command exit", || status.exists());
        let value = std::fs::read_to_string(&status).unwrap();
        std::fs::remove_file(&status).unwrap();
        assert_ne!(value, "0", "member effect unexpectedly succeeded");
    };
    for case in [
        "revoked",
        "expired",
        "read-mode",
        "wrong-principal",
        "cross-task",
        "unrelated-child",
        "revoked-child",
        "revoked-ancestor",
        "revoked-source-ancestor",
        "expired-source-ancestor",
        "missing-source",
        "missing-child",
    ] {
        let mode = if case == "read-mode" {
            GrantMode::Read
        } else {
            GrantMode::ActAutonomously
        };
        let principal = if case == "wrong-principal" {
            worker.clone()
        } else {
            coordinator.clone()
        };
        let task: Option<TaskId> = (case == "cross-task").then(|| source_task.task_id.clone());
        let source = engine
            .issue_root_grant(
                Some(principal),
                task,
                vec![
                    "dispatch_delegated".into(),
                    "terminal_control".into(),
                    "member_report".into(),
                ],
                mode,
                None,
            )
            .unwrap();
        let ancestor = source.grant_id.clone();
        let source = if case.ends_with("source-ancestor") {
            engine
                .delegate_grant(
                    &ancestor,
                    coordinator.clone(),
                    vec![
                        "dispatch_delegated".into(),
                        "terminal_control".into(),
                        "member_report".into(),
                    ],
                    mode,
                    None,
                )
                .unwrap()
        } else {
            source
        };
        let child: GrantId = match case {
            "unrelated-child" => {
                engine
                    .issue_root_grant(
                        Some(worker.clone()),
                        None,
                        vec!["dispatch_delegated".into()],
                        mode,
                        None,
                    )
                    .unwrap()
                    .grant_id
            }
            "revoked-ancestor" => {
                let middle = engine
                    .delegate_grant(
                        &source.grant_id,
                        coordinator.clone(),
                        vec!["dispatch_delegated".into()],
                        mode,
                        None,
                    )
                    .unwrap();
                let leaf = engine
                    .delegate_grant(
                        &middle.grant_id,
                        worker.clone(),
                        vec!["dispatch_delegated".into()],
                        mode,
                        None,
                    )
                    .unwrap();
                engine
                    .revoke(&middle.grant_id, "fixture ancestor revoked")
                    .unwrap();
                leaf.grant_id
            }
            _ => {
                engine
                    .delegate_grant(
                        &source.grant_id,
                        worker.clone(),
                        vec!["dispatch_delegated".into()],
                        mode,
                        None,
                    )
                    .unwrap()
                    .grant_id
            }
        };
        if case == "revoked" {
            engine
                .revoke(&source.grant_id, "fixture source revoked")
                .unwrap();
        }
        if case == "revoked-child" {
            engine.revoke(&child, "fixture child revoked").unwrap();
        }
        if case == "expired" {
            store
                .connection()
                .execute(
                    "UPDATE grants SET expires_at='2000-01-01T00:00:00Z' WHERE grant_id=?1",
                    [source.grant_id.as_str()],
                )
                .unwrap();
        }
        if case == "revoked-source-ancestor" {
            engine
                .revoke(&ancestor, "fixture source ancestor revoked")
                .unwrap();
        }
        if case == "expired-source-ancestor" {
            store
                .connection()
                .execute(
                    "UPDATE grants SET expires_at='2000-01-01T00:00:00Z' WHERE grant_id=?1",
                    [ancestor.as_str()],
                )
                .unwrap();
        }
        if case.ends_with("source-ancestor") {
            invoke(&format!(
                "VIVA_GRANT_ID='{}' '{}' handoff --summary source-ancestor-must-not-authorize",
                source.grant_id,
                binary().display()
            ));
        }
        let source_id = if case == "missing-source" {
            GrantId::new()
        } else {
            source.grant_id
        };
        let child = if case == "missing-child" {
            GrantId::new()
        } else {
            child
        };
        invoke(&format!(
            "VIVA_GRANT_ID='{}' '{}' dispatch --task '{}' --member '{}' --grant '{}' --request-key 'negative-{case}' --cwd '{}' -- /bin/sh -c 'sleep 120'",
            source_id,
            binary().display(),
            target_task.task_id,
            worker,
            child,
            home.display()
        ));
    }
    invoke(&format!(
        "'{}' handoff --task '{}' --member '{}' --summary wrong-scope",
        binary().display(),
        target_task.task_id,
        coordinator
    ));
    let grants_before: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM grants", [], |r| r.get(0))
        .unwrap();
    invoke(&format!(
        "'{}' grant --member '{}' --task '{}' --action dispatch_delegated --mode ACT_AUTONOMOUSLY",
        binary().display(),
        worker,
        target_task.task_id
    ));
    assert_eq!(
        grants_before,
        store
            .connection()
            .query_row("SELECT count(*) FROM grants", [], |r| r.get::<_, i64>(0))
            .unwrap()
    );
    invoke(&format!(
        "VIVA_MEMBER_CREDENTIAL=forged '{}' handoff --summary unknown-credential",
        binary().display()
    ));
    let token = std::fs::read_to_string(token_file).unwrap();
    engine.revoke_worker_credential(&token).unwrap();
    invoke(&format!(
        "'{}' handoff --summary revoked-credential",
        binary().display()
    ));
    let mutations: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM launch_intents WHERE request_key LIKE 'negative-%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mutations, 0);
    assert_eq!(
        store
            .connection()
            .query_row(
                "SELECT count(*) FROM events WHERE kind='native_member_handoff'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let after: i64 = store
        .connection()
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_client_admitted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        admissions_before, after,
        "member CLI fell back to controller authentication"
    );
    let denied: i64 = store.connection().query_row("SELECT count(*) FROM events WHERE kind IN ('native_member_effect_refused', 'native_cli_refused')", [], |r| r.get(0)).unwrap();
    assert!(denied >= 18);
    assert_eq!(
        parent["execution_id"].as_str().unwrap(),
        store
            .connection()
            .query_row(
                "SELECT execution_id FROM launch_intents WHERE request_key='broker'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap()
    );
    shutdown(home, &mut host);
}
