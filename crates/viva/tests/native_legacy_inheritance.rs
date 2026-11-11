//! Real cross-backend inheritance (issue #191): a live legacy `viva server`
//! hands its PTY masters to the native host. The marker is read back through
//! the native HTTP API, the old host is gone and the shell PID is unchanged.
//! The failure case corrupts the stored scene book so takeover aborts AFTER
//! the fds arrived: the old host must reattach its readers and keep serving.
//! Never counted as coverage from the default zero-test feature-disabled run.
#![cfg(all(feature = "herdr-runtime", unix))]
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use viva::{
    authority::{AuthorityEngine, GrantMode},
    foundation::{ids::utc_now, paths, store::Store},
    members::{MemberBinding, MemberRegistry},
    server::{Client, RequestKind, WorkbenchTreeOp},
    tasks::TaskRegistry,
};

struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        // Exact task-owned PID. Forced fixture cleanup is not lifecycle PASS.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn log_tail(home: &Path, name: &str) -> String {
    std::fs::read_to_string(home.join(name)).unwrap_or_default()
}

fn spawn_legacy(home: &Path) -> Fixture {
    Fixture(
        Command::new(env!("CARGO_BIN_EXE_viva"))
            .arg("server")
            .current_dir(home)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("VIVA_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(home.join("legacy.log")).unwrap())
            .spawn()
            .unwrap(),
    )
}

fn spawn_native(home: &Path) -> Fixture {
    Fixture(
        Command::new(env!("CARGO_BIN_EXE_viva"))
            .args(["runtime", "server"])
            .current_dir(home)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("VIVA_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(home.join("native.log")).unwrap())
            .spawn()
            .unwrap(),
    )
}

fn connect_legacy(home: &Path, legacy: &mut Fixture) -> Client {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(client) = Client::connect(home) {
            return client;
        }
        if let Some(status) = legacy.0.try_wait().unwrap() {
            panic!(
                "legacy host exited before it served: {status}; log: {}",
                log_tail(home, "legacy.log")
            );
        }
        assert!(
            Instant::now() < deadline,
            "legacy host did not answer: {}",
            log_tail(home, "legacy.log")
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn screen_text(snapshot: &Value) -> String {
    snapshot["visible"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn submit_and_wait(client: &mut Client, terminal_id: &str, command: &str, needle: &str) {
    client
        .call(RequestKind::TerminalInput {
            terminal_id: terminal_id.to_string(),
            bytes_hex: viva::server::hex_encode(command.as_bytes()),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = client
            .call(RequestKind::TerminalSnapshot {
                terminal_id: terminal_id.to_string(),
            })
            .unwrap();
        let seen = screen_text(&snapshot).replace(['\n', '\r'], "");
        if seen.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "`{needle}` never reached the legacy screen: {seen}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}

/// A shell whose marker output proves the child actually executed a command,
/// placed in a saved scene/tab so the import plan has a real layout to map.
fn marked_shell(client: &mut Client, home: &Path) -> (String, u32) {
    const SCENE: &str = "legacy-fixture";
    let created = client
        .call(RequestKind::TerminalCreate {
            argv: vec!["/bin/sh".into()],
            cwd: home.display().to_string(),
            env: Vec::new(),
            cols: 80,
            rows: 24,
            purpose: "legacy inheritance fixture".into(),
            worktree_id: None,
            owner: "user_shell".into(),
        })
        .unwrap();
    let terminal_id = created["terminal_id"].as_str().unwrap().to_string();
    let pid = created["pid"].as_u64().unwrap() as u32;
    client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreateScene {
                id: Some(SCENE.into()),
                name: "legacy scene".into(),
            },
        })
        .unwrap();
    let tree = client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreateTab {
                scene_id: Some(SCENE.into()),
                name: "main".into(),
            },
        })
        .unwrap();
    let tab = tree["tree"]["scenes"][SCENE]["tabs"]
        .as_array()
        .unwrap_or_else(|| panic!("no tabs after CreateTab: {tree}"))
        .iter()
        .find(|tab| tab["name"] == "main")
        .and_then(|tab| tab["id"].as_str())
        .unwrap()
        .to_string();
    client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreatePane {
                scene_id: Some(SCENE.into()),
                tab_id: Some(tab),
                terminal_id: Some(terminal_id.clone()),
                direction: None,
            },
        })
        .unwrap();
    submit_and_wait(
        client,
        &terminal_id,
        "printf 'ALIVE-%s\\n' \"$((6*7))\"\n",
        "ALIVE-42",
    );
    (terminal_id, pid)
}

struct DispatchFixture {
    member: viva::members::MemberRecord,
    task: viva::tasks::TaskRecord,
    grant: viva::authority::Grant,
}

fn seed_dispatch(home: &Path) -> DispatchFixture {
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let members = MemberRegistry::new(&store);
    let member = members.register("legacy live execution").unwrap();
    members
        .set_binding(&MemberBinding {
            member_id: member.member_id.clone(),
            role: "legacy inheritance fixture".into(),
            model_binding: "fixture:model".into(),
            tools: vec!["fixture-tool".into()],
            updated_at: utc_now(),
        })
        .unwrap();
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task(
            "legacy live execution task",
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
    DispatchFixture {
        member,
        task,
        grant,
    }
}

fn dispatch_live_execution(
    client: &mut Client,
    home: &Path,
    data: &DispatchFixture,
    request_key: &str,
    command: &str,
) -> Value {
    client
        .call(RequestKind::Dispatch {
            task_id: data.task.task_id.to_string(),
            member_id: data.member.member_id.to_string(),
            grant_id: data.grant.grant_id.to_string(),
            request_key: request_key.into(),
            argv: vec!["/bin/sh".into(), "-c".into(), command.into()],
            cwd: home.display().to_string(),
            worktree_id: None,
        })
        .unwrap()
}

fn wait_legacy_execution_running(home: &Path, execution_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status: String = Store::open(&paths::database_path(home), viva::server::migrations())
            .unwrap()
            .connection()
            .query_row(
                "SELECT status FROM executions WHERE execution_id=?1",
                [execution_id],
                |row| row.get(0),
            )
            .unwrap();
        if status == "running" {
            return;
        }
        assert!(Instant::now() < deadline, "legacy execution never ran");
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_native_ready(home: &Path, native: &mut Fixture) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = native.0.try_wait().unwrap() {
            panic!(
                "native host exited before serving: {status}; log: {}",
                log_tail(home, "native.log")
            );
        }
        if home.join("runtime/workbench/herdr.sock").exists() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native host never bound its socket; log: {}",
            log_tail(home, "native.log")
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_exit(child: &mut Fixture, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "{what} did not exit");
        thread::sleep(Duration::from_millis(25));
    }
}

fn call(home: &Path, method: &str, params: Value) -> Value {
    let mut socket = UnixStream::connect(home.join("runtime/workbench/herdr.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        socket,
        "{}",
        json!({"id":"inheritance-fixture", "method":method, "params":params,
            "transport_peer":{"pid":1,"uid":0}, "peer_pid":1, "peer_uid":0, "claimed_role":"user"})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn a_live_legacy_execution_is_inherited_and_stoppable() {
    let temp = tempfile::Builder::new()
        .prefix("legacy-live-execution-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let data = seed_dispatch(home);
    let pause_marker = home.join("inherited-pause-exit");
    let pause_go = home.join("inherited-pause-go");
    // The member is a child of the OLD host. After the old host exits it is
    // reparented, so it is no longer a descendant of the new host: the owner
    // gate must still refuse it through the adopted execution binding.
    let member_command = format!(
        "printf 'MEMBER-ALIVE\\n'; while [ ! -f '{}' ]; do sleep 0.1; done; \
         VIVA_HOME='{}' '{}' runtime host pause >/dev/null 2>&1; \
         printf '%s' $? > '{}'; sleep 60",
        pause_go.display(),
        home.display(),
        env!("CARGO_BIN_EXE_viva"),
        pause_marker.display()
    );
    let mut legacy = spawn_legacy(home);
    let mut client = connect_legacy(home, &mut legacy);
    let dispatched = dispatch_live_execution(
        &mut client,
        home,
        &data,
        "legacy-live-execution",
        &member_command,
    );
    let execution_id = dispatched["execution_id"]
        .as_str()
        .unwrap_or_else(|| panic!("dispatch did not return an execution: {dispatched}"))
        .to_string();
    let member_pid = dispatched["pid"].as_u64().expect("dispatch pid") as i32;
    wait_legacy_execution_running(home, &execution_id);
    drop(client);

    let mut native = spawn_native(home);
    wait_native_ready(home, &mut native);
    let status = wait_exit(&mut legacy, "legacy host after the prepared takeover");
    assert!(
        status.success(),
        "the acknowledged legacy host should retire cleanly: {status}; log: {}",
        log_tail(home, "legacy.log")
    );
    let snapshot = call(home, "viva.snapshot", json!({}));
    let snapshot = &snapshot["result"]["snapshot"];
    assert_eq!(
        snapshot["integration_state"], "native-default-entry-legacy-inheritance",
        "{snapshot}"
    );

    let panes = call(home, "pane.list", json!({}));
    let panes = panes["result"]["panes"]
        .as_array()
        .unwrap_or_else(|| panic!("pane.list did not answer with panes: {panes}"));
    assert_eq!(panes.len(), 1, "the member execution's pane: {panes:?}");
    let pane_id = panes[0]["pane_id"].as_str().unwrap();
    let process = call(home, "pane.process_info", json!({"pane_id": pane_id}));
    assert_eq!(
        process["result"]["process_info"]["shell_pid"].as_u64(),
        Some(member_pid as u64),
        "the adopted pane must still own the member process: {process}"
    );

    std::fs::write(&pause_go, b"go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !pause_marker.exists() {
        assert!(
            Instant::now() < deadline,
            "the inherited member never attempted the owner pause"
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert_ne!(
        std::fs::read_to_string(&pause_marker).unwrap().trim(),
        "0",
        "an inherited member execution authenticated as the owner frontend"
    );
    let conn = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    let paused: i64 = conn
        .connection()
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_host_paused'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(paused, 0, "the inherited member paused the owner host");
    let refused: i64 = conn
        .connection()
        .query_row(
            "SELECT count(*) FROM events WHERE kind='native_owner_auth_refused' \
             AND payload LIKE '%peer_is_execution_descendant%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        refused >= 1,
        "the inherited member's owner authentication was not audited"
    );
    drop(conn);

    authenticate_owner(home);
    let stopped = call(
        home,
        "viva.execution.stop",
        json!({"execution_id": execution_id}),
    );
    assert_eq!(
        stopped["result"]["type"], "viva_execution_stopped",
        "{stopped}"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row: (String, Option<i64>) =
            Store::open(&paths::database_path(home), viva::server::migrations())
                .unwrap()
                .connection()
                .query_row(
                    "SELECT status, process_exit FROM executions WHERE execution_id=?1",
                    [&execution_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
        if row.1.is_some() {
            assert_eq!(row.0, "stopped");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "inherited execution never settled"
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !log_tail(home, "native.log").contains("owner frontend capability is refused"),
        "takeover of one live legacy execution was refused: {}",
        log_tail(home, "native.log")
    );
}

fn authenticate_owner(home: &Path) {
    let capability: Value = serde_json::from_slice(
        &std::fs::read(home.join("runtime/workbench/owner-channel.json")).unwrap(),
    )
    .unwrap();
    let peer = herdr::embedding::TransportPeer {
        pid: Some(std::process::id()),
        uid: Some(unsafe { libc::geteuid() }),
    };
    let lifetime = herdr::embedding::peer_lifetime_marker(peer).unwrap();
    let response = call(
        home,
        "viva.client.authenticate",
        json!({"credential": capability["credential"], "process_lifetime": lifetime}),
    );
    assert_eq!(response["result"]["type"], "ok", "{response}");
}

#[test]
fn live_legacy_session_is_inherited_with_its_process_and_screen() {
    let temp = tempfile::Builder::new()
        .prefix("legacy-inherit-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let mut legacy = spawn_legacy(home);
    let mut client = connect_legacy(home, &mut legacy);
    let old_pid = legacy.0.id();
    let (terminal_id, shell_pid) = marked_shell(&mut client, home);
    // Push the marker off the visible screen: the handoff must carry the
    // primary ring, not only the 24 rows the new host happens to see.
    submit_and_wait(
        &mut client,
        &terminal_id,
        "i=1; while [ \"$i\" -le 60 ]; do printf 'SCROLL-%03d\\n' \"$i\"; i=$((i+1)); done; \
         printf 'TAIL-END\\n'\n",
        "TAIL-END",
    );
    let layout: Value = client
        .call(RequestKind::WorkbenchLayout { layout_json: None })
        .unwrap();
    assert!(
        layout["layout"].as_str().is_some_and(|raw| !raw.is_empty()),
        "the legacy host must have persisted a scene book to inherit: {layout}"
    );
    drop(client);

    let mut native = spawn_native(home);
    wait_native_ready(home, &mut native);
    let status = wait_exit(&mut legacy, "legacy host after the prepared takeover");
    assert!(
        status.success(),
        "the acknowledged legacy host should retire cleanly: {status}; log: {}",
        log_tail(home, "legacy.log")
    );

    let snapshot = call(home, "viva.snapshot", json!({}));
    let snapshot = &snapshot["result"]["snapshot"];
    assert_eq!(snapshot["backend"], "herdr-source-library", "{snapshot}");
    assert_eq!(
        snapshot["integration_state"], "native-default-entry-legacy-inheritance",
        "{snapshot}"
    );
    let handoff = &snapshot["legacy_handoff"];
    assert_eq!(handoff["inherited"], true, "{snapshot}");
    assert_eq!(handoff["old_host_pid"].as_u64(), Some(u64::from(old_pid)));
    assert_eq!(handoff["adopted_panes"], 1, "{snapshot}");
    assert_eq!(handoff["fidelity"], "degraded_import");
    let not_preserved = handoff["not_preserved"].as_array().unwrap();
    assert!(
        handoff["preserved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "live_child_process")
    );
    assert!(not_preserved.iter().any(|item| item == "pane_cwd"));
    assert!(
        not_preserved
            .iter()
            .any(|item| item == "split_ratios_focus_zoom")
    );

    let panes = call(home, "pane.list", json!({}));
    let panes = panes["result"]["panes"]
        .as_array()
        .unwrap_or_else(|| panic!("pane.list did not answer with panes: {panes}"));
    assert_eq!(
        panes.len(),
        1,
        "exactly the one live legacy terminal: {panes:?}"
    );
    let pane = &panes[0];
    assert_eq!(pane["workspace_id"], "ws-legacy-1");
    let pane_id = pane["pane_id"].as_str().unwrap();

    let process = call(home, "pane.process_info", json!({"pane_id": pane_id}));
    assert_eq!(
        process["result"]["process_info"]["shell_pid"].as_u64(),
        Some(u64::from(shell_pid)),
        "the adopted pane must still own the original child: {process}"
    );
    let read = call(
        home,
        "pane.read",
        json!({"pane_id": pane_id, "source": "recent", "format": "text"}),
    );
    let text = read["result"]["read"]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("TAIL-END"),
        "the inherited screen lost the live marker: {text}"
    );
    let history = call(
        home,
        "pane.read",
        json!({"pane_id": pane_id, "source": "recent", "lines": 200, "format": "text"}),
    );
    let history = history["result"]["read"]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        history.contains("SCROLL-001") && history.contains("ALIVE-42"),
        "the inherited primary ring lost lines that were already scrolled off: {history}"
    );
    assert!(!home.join("server.sock").exists());
}

#[test]
fn failed_adoption_leaves_the_old_host_serving_and_its_process_alive() {
    let temp = tempfile::Builder::new()
        .prefix("legacy-refuse-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let mut legacy = spawn_legacy(home);
    let mut client = connect_legacy(home, &mut legacy);
    let (terminal_id, shell_pid) = marked_shell(&mut client, home);
    drop(client);

    // Corruption lands AFTER the transfer begins: the receiver accepts the
    // fds, then refuses to build a plan from this book. It must close the
    // fds without acknowledging.
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    store
        .connection()
        .execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES('workbench_layout','not-json')",
            [],
        )
        .unwrap();
    drop(store);

    let mut native = spawn_native(home);
    let status = wait_exit(&mut native, "native host after a refused takeover");
    assert!(!status.success(), "a refused takeover must not serve");
    let native_log = log_tail(home, "native.log");
    assert!(
        native_log.contains("legacy scene book is invalid"),
        "expected the corrupt-book refusal, got: {native_log}"
    );
    assert!(!home.join("runtime/workbench/herdr.sock").exists());
    assert!(
        legacy.0.try_wait().unwrap().is_none(),
        "the old host must still be running; log: {}",
        log_tail(home, "legacy.log")
    );

    let mut client = connect_legacy(home, &mut legacy);
    assert_eq!(
        client.call(RequestKind::TerminalList).unwrap()["terminals"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    submit_and_wait(
        &mut client,
        &terminal_id,
        "printf 'STILL-%s\\n' \"$((6*7))\"\n",
        "STILL-42",
    );
    let list = client.call(RequestKind::TerminalList).unwrap();
    // SAFETY: kill(pid, 0) is an existence probe with no signal delivery.
    assert_eq!(
        unsafe { libc::kill(shell_pid as i32, 0) },
        0,
        "the shell process must survive the refused takeover: {list}"
    );
    drop(client);

    let recovery: Vec<String> = {
        let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
        let mut stmt = store
            .connection()
            .prepare("SELECT kind FROM recovery_events ORDER BY seq")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    };
    assert!(
        recovery.iter().any(|kind| kind == "live_handoff_failed"),
        "the old host must record the failed transfer: {recovery:?}"
    );
}

#[test]
fn a_stale_legacy_socket_does_not_block_a_fresh_native_session() {
    let temp = tempfile::Builder::new()
        .prefix("legacy-stale-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    // A crashed host leaves the bound path on disk; connecting refuses.
    let stale = home.join(viva::server::SOCKET_NAME);
    drop(UnixListener::bind(&stale).unwrap());

    let mut native = spawn_native(home);
    wait_native_ready(home, &mut native);
    let snapshot = call(home, "viva.snapshot", json!({}));
    let snapshot = &snapshot["result"]["snapshot"];
    assert_eq!(snapshot["integration_state"], "native-default-entry");
    assert_eq!(snapshot["legacy_handoff"]["inherited"], false, "{snapshot}");
    assert!(
        log_tail(home, "native.log").contains("starting a fresh native session"),
        "the stale socket must be reported, not silently claimed: {}",
        log_tail(home, "native.log")
    );
    assert!(
        stale.exists(),
        "the stale file stays for the legacy entry to claim later"
    );
}
