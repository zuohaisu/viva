//! #191 P1: real installed-entry contracts, no fake CLI or renderer.
#![cfg(all(feature = "herdr-runtime", unix))]
#[path = "support/owner_anchor.rs"]
mod owner_support;
use owner_support::{call, spawn_manual, wait_ready};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
    time::{Duration, Instant},
};
use viva::{
    authority::{AuthorityEngine, GrantMode},
    foundation::{paths, store::Store},
    members::{MemberBinding, MemberRegistry},
    tasks::TaskRegistry,
};

fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(
        std::env::var_os("VIVA_P1_BIN").unwrap_or_else(|| env!("CARGO_BIN_EXE_viva").into()),
    )
    .args(args)
    .env_clear()
    .env("HOME", home)
    .env("VIVA_HOME", home)
    .env("PATH", "/usr/bin:/bin")
    .env("SHELL", "/bin/sh")
    .env("TERM", "xterm-256color")
    .current_dir(home)
    .output()
    .unwrap()
}
fn ok(home: &Path, args: &[&str]) -> Value {
    let out = cli(home, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn top_level_status_and_pause_use_the_native_host_without_legacy_socket() {
    let temp = tempfile::Builder::new().prefix("p1").tempdir().unwrap();
    let home = temp.path();
    let host = spawn_manual(home);
    wait_ready(home, &host);
    let snapshot = call(home, "viva.snapshot", json!({}));
    let status = ok(home, &["status"]);
    assert_eq!(status["pid"], snapshot["result"]["snapshot"]["runtime_pid"]);
    assert_eq!(status["backend"], "herdr-source-library");
    let feed = ok(home, &["events", "--since", "0", "--limit", "1"]);
    assert_eq!(feed["events"].as_array().unwrap().len(), 1);
    assert!(feed["last_seq"].as_u64().unwrap() > 0);
    for action in ["pause", "resume"] {
        ok(home, &[action]);
        assert_eq!(ok(home, &["status"])["paused"], action == "pause");
    }
    assert!(!home.join("server.sock").exists());
}

#[test]
fn workspace_and_store_only_domain_commands_survive_native_cutover() {
    let temp = tempfile::Builder::new().prefix("p1").tempdir().unwrap();
    let home = temp.path();
    let host = spawn_manual(home);
    wait_ready(home, &host);
    let folder = home.join("project");
    std::fs::create_dir(&folder).unwrap();
    let file = home.join("test.code-workspace");
    let original = format!(
        "{{\"folders\":[{{\"path\":\"{}\"}}],\"tasks\":{{\"must_not_execute\":true}}}}",
        folder.display()
    );
    std::fs::write(&file, &original).unwrap();
    let workspace = ok(home, &["workspace", "open", file.to_str().unwrap()]);
    assert_eq!(
        workspace["folders"][0]["path"],
        folder.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    let again = ok(home, &["workspace", "list"]);
    assert_eq!(workspace["workspace_id"], again["workspace_id"]);
    let task = ok(home, &["create-task", "--goal", "P1 native domain test"]);
    let task_id = task["task_id"].as_str().unwrap();
    assert_eq!(
        ok(home, &["brief", task_id])["goal"],
        "P1 native domain test"
    );
    assert_eq!(ok(home, &["result", task_id]), json!([]));
    let refused = call(
        home,
        "viva.workspace",
        json!({"action": "new", "value": "forged"}),
    );
    assert!(
        refused.get("error").is_some(),
        "unbound peer mutated navigation: {refused}"
    );
    assert!(!home.join("server.sock").exists());
}

#[test]
fn public_dispatch_is_idempotent_pause_gated_and_stops_only_its_binding() {
    let temp = tempfile::Builder::new().prefix("p1").tempdir().unwrap();
    let home = temp.path();
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let registry = MemberRegistry::new(&store);
    let member = registry.register("P1 member").unwrap();
    registry
        .set_binding(&MemberBinding {
            member_id: member.member_id.clone(),
            role: "fixture".into(),
            model_binding: "fixture:model".into(),
            tools: vec!["fixture-tool".into()],
            updated_at: viva::foundation::ids::utc_now(),
        })
        .unwrap();
    let task = TaskRegistry::new(&store)
        .create_task(
            "P1 execution",
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
            GrantMode::ActAutonomously,
            None,
        )
        .unwrap();
    let mut host = spawn_manual(home);
    wait_ready(home, &host);
    let args = [
        "dispatch",
        "--task",
        task.task_id.as_str(),
        "--member",
        member.member_id.as_str(),
        "--grant",
        grant.grant_id.as_str(),
        "--request-key",
        "p1-key",
        "--cwd",
        home.to_str().unwrap(),
        "--",
        "/bin/sh",
        "-c",
        "sleep 120",
    ];
    ok(home, &["pause"]);
    assert!(!cli(home, &args).status.success());
    ok(home, &["resume"]);
    let execution = ok(home, &args);
    let replay = ok(home, &args);
    assert_eq!(execution["execution_id"], replay["execution_id"]);
    assert_eq!(execution["pid"], replay["pid"]);
    assert_eq!(execution["terminal_id"], replay["terminal_id"]);
    assert_eq!(replay["replayed"], true);
    let mut neighbor_args = args;
    neighbor_args[8] = "p1-neighbor";
    let neighbor = ok(home, &neighbor_args);
    let terminal = execution["terminal_id"].as_str().unwrap();
    let listed = ok(home, &["terminals"]);
    assert!(
        listed["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["terminal_id"] == terminal)
    );
    ok(home, &["stop-terminal", terminal]);
    assert!(
        ok(home, &["terminals"])["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["execution_id"] == neighbor["execution_id"]),
        "targeted stop killed its neighbor"
    );
    assert_ne!(
        TaskRegistry::new(&store)
            .require_task(&task.task_id)
            .unwrap()
            .status
            .as_str(),
        "done"
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let count: i64 = store
            .connection()
            .query_row(
                "SELECT count(*) FROM executions WHERE status='running'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if count == 1 {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!home.join("server.sock").exists());
    let running = neighbor;
    ok(home, &["shutdown"]);
    let deadline = Instant::now() + Duration::from_secs(15);
    while host.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(25));
    }
    let state: String = store
        .connection()
        .query_row(
            "SELECT status FROM executions WHERE execution_id=?1",
            [running["execution_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "stopped");
    let pid = running["pid"].as_i64().unwrap() as i32;
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            Instant::now() < deadline,
            "owned root process remains after shutdown"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn public_server_aliases_start_and_shutdown_the_native_owner() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for entry in ["server", "start"] {
        let temp = tempfile::Builder::new().prefix("p1").tempdir().unwrap();
        let home = temp.path();
        viva::foundation::paths::ensure_private_dir(home).unwrap();
        let runtime = home.join("runtime/workbench");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("config.toml"), "[terminal]\ndefault_shell='/bin/sh'\nshell_mode='non_login'\n[ui.sound]\nenabled=false\n").unwrap();
        let mut host = ChildGuard(
            Command::new(env!("CARGO_BIN_EXE_viva"))
                .arg(entry)
                .env_clear()
                .env("HOME", home)
                .env("VIVA_HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .env("SHELL", "/bin/sh")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(home.join("host.log")).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while !home.join("runtime-capability/owner-anchor").exists() {
            assert!(
                host.0.try_wait().unwrap().is_none(),
                "host exited: {}",
                std::fs::read_to_string(home.join("host.log")).unwrap()
            );
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(ok(home, &["status"])["pid"], host.0.id());
        ok(home, &["shutdown"]);
        loop {
            if let Some(status) = host.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!home.join("server.sock").exists());
    }
}

#[test]
fn unintegrated_commands_refuse_without_starting_a_legacy_host() {
    let temp = tempfile::Builder::new().prefix("p1").tempdir().unwrap();
    let home = temp.path();
    for command in ["agent", "terminal", "scene", "plugin", "server-restart"] {
        let out = cli(home, &[command]);
        assert!(!out.status.success(), "{command} unexpectedly succeeded");
        assert!(String::from_utf8_lossy(&out.stderr).contains("no legacy host was started"));
        assert!(!home.join("server.sock").exists());
    }
    // All workbench aliases gate non-TTY calls BEFORE touching a store/server.
    for args in [vec![], vec!["workbench"], vec!["open", "/tmp"]] {
        let out = cli(home, &args);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("interactive terminal"));
        assert!(!home.join("viva.db").exists());
    }
}
