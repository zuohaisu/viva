//! The resident server owns the scene / tab / pane tree. Two clients share
//! it: a split or rename on one side is what the other sees after a refresh,
//! and a size change on one tab does not rewrite the other.

use std::process::Command;
use std::time::{Duration, Instant};

use viva::foundation::paths::database_path;
use viva::foundation::store::Store;
use viva::server::{Client, Host, RequestKind, SOCKET_NAME, WorkbenchTreeOp, new_request};

fn viva_bin() -> &'static str {
    env!("CARGO_BIN_EXE_viva")
}

struct Running {
    home: std::path::PathBuf,
    server: Option<std::thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

fn start() -> Running {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let host = Host::open(&home).expect("host");
    let server = host.serve_background();
    let socket = home.join(SOCKET_NAME);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    Running {
        home,
        server: Some(server),
        _dir: dir,
    }
}

impl Running {
    fn client(&self) -> Client {
        Client::connect(&self.home).expect("client")
    }

    fn shutdown(mut self) {
        if let Ok(mut client) = Client::connect(&self.home) {
            let _ = client.call(RequestKind::Shutdown { close_policy: None });
        }
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn call(client: &mut Client, op: WorkbenchTreeOp) -> serde_json::Value {
    client
        .call(RequestKind::WorkbenchTree { op })
        .expect("tree op")
}

fn tab_id(tree: &serde_json::Value, scene: &str, name: &str) -> String {
    tree["scenes"][scene]["tabs"]
        .as_array()
        .expect("tabs")
        .iter()
        .find(|tab| tab["name"] == name)
        .unwrap_or_else(|| panic!("tab {name} missing in {tree}"))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn ratio(tree: &serde_json::Value, scene: &str, name: &str) -> u64 {
    let id = tab_id(tree, scene, name);
    tree["scenes"][scene]["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tab| tab["id"] == id)
        .unwrap()["grid"]["Split"]["ratio"]
        .as_u64()
        .expect("ratio")
}

fn two_tabs(client: &mut Client) -> (String, String) {
    call(
        client,
        WorkbenchTreeOp::CreateScene {
            id: Some("alpha".into()),
            name: "Alpha".into(),
        },
    );
    let one = call(
        client,
        WorkbenchTreeOp::CreateTab {
            scene_id: Some("alpha".into()),
            name: "one".into(),
        },
    );
    let first = tab_id(&one["tree"], "alpha", "one");
    call(
        client,
        WorkbenchTreeOp::Split {
            scene_id: Some("alpha".into()),
            tab_id: Some(first.clone()),
            pane_id: None,
            direction: "right".into(),
            terminal_id: None,
        },
    );
    let two = call(
        client,
        WorkbenchTreeOp::CreateTab {
            scene_id: Some("alpha".into()),
            name: "two".into(),
        },
    );
    let second = tab_id(&two["tree"], "alpha", "two");
    call(
        client,
        WorkbenchTreeOp::Split {
            scene_id: Some("alpha".into()),
            tab_id: Some(second.clone()),
            pane_id: None,
            direction: "down".into(),
            terminal_id: None,
        },
    );
    (first, second)
}

#[test]
fn two_clients_share_a_split_a_rename_and_per_tab_sizes() {
    let host = start();
    let mut a = host.client();
    let mut b = host.client();
    let (first, second) = two_tabs(&mut a);

    let seen = call(&mut b, WorkbenchTreeOp::List);
    assert_eq!(ratio(&seen["tree"], "alpha", "one"), 50);
    assert_eq!(ratio(&seen["tree"], "alpha", "two"), 50);
    assert!(
        seen["tree"]["scenes"]["alpha"]["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tab| tab["name"] == "one")
    );

    let output = Command::new(viva_bin())
        .env("VIVA_HOME", &host.home)
        .args([
            "scene", "tab", "rename", "--scene", "alpha", "--id", &first, "--name", "renamed",
        ])
        .output()
        .expect("cli");
    assert!(
        output.status.success(),
        "scene rename failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let renamed = call(&mut b, WorkbenchTreeOp::List);
    assert!(
        renamed["tree"]["scenes"]["alpha"]["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tab| tab["name"] == "renamed"),
        "the other client sees the rename: {renamed}"
    );

    call(
        &mut a,
        WorkbenchTreeOp::Resize {
            scene_id: Some("alpha".into()),
            tab_id: Some(first.clone()),
            path: vec![],
            ratio: 30,
        },
    );
    call(
        &mut b,
        WorkbenchTreeOp::Resize {
            scene_id: Some("alpha".into()),
            tab_id: Some(second.clone()),
            path: vec![],
            ratio: 80,
        },
    );
    let both = call(&mut a, WorkbenchTreeOp::List);
    assert_eq!(ratio(&both["tree"], "alpha", "renamed"), 30);
    assert_eq!(ratio(&both["tree"], "alpha", "two"), 80);

    call(
        &mut a,
        WorkbenchTreeOp::Resize {
            scene_id: Some("alpha".into()),
            tab_id: Some(first.clone()),
            path: vec![],
            ratio: 35,
        },
    );
    call(
        &mut b,
        WorkbenchTreeOp::Resize {
            scene_id: Some("alpha".into()),
            tab_id: Some(first.clone()),
            path: vec![],
            ratio: 65,
        },
    );
    let last = call(&mut a, WorkbenchTreeOp::List);
    assert_eq!(
        ratio(&last["tree"], "alpha", "renamed"),
        65,
        "the later size on the same tab wins"
    );
    assert_eq!(ratio(&last["tree"], "alpha", "two"), 80);

    // A full layout save from a client looking at tab two, carrying a stale
    // ratio for tab one, must not clobber tab one.
    let mut payload = last["tree"].clone();
    payload["selected"] = serde_json::json!("alpha");
    payload["scenes"]["alpha"]["active"] = serde_json::json!(second);
    for tab in payload["scenes"]["alpha"]["tabs"].as_array_mut().unwrap() {
        if tab["name"] == "renamed" {
            tab["grid"]["Split"]["ratio"] = serde_json::json!(12);
        }
        if tab["name"] == "two" {
            tab["grid"]["Split"]["ratio"] = serde_json::json!(45);
        }
    }
    a.call(RequestKind::WorkbenchLayout {
        layout_json: Some(payload.to_string()),
    })
    .expect("layout merge");
    let merged = call(&mut b, WorkbenchTreeOp::List);
    assert_eq!(ratio(&merged["tree"], "alpha", "renamed"), 65);
    assert_eq!(ratio(&merged["tree"], "alpha", "two"), 45);

    let layout = b
        .call(RequestKind::WorkbenchLayout { layout_json: None })
        .expect("layout get");
    let text = layout["layout"].as_str().expect("layout string");
    assert!(text.contains("\"version\":2"), "{text}");
    assert!(text.contains("renamed"), "{text}");
    host.shutdown();
}

#[test]
fn closing_a_pane_does_not_stop_its_pty() {
    let host = start();
    let mut client = host.client();
    let created = client
        .call(RequestKind::TerminalCreate {
            argv: vec!["/bin/sh".into(), "-c".into(), "echo pane-ready; cat".into()],
            cwd: host.home.display().to_string(),
            env: vec![],
            cols: 80,
            rows: 24,
            purpose: "pane session".into(),
            worktree_id: None,
            owner: "user_shell".into(),
        })
        .expect("terminal");
    let terminal_id = created["terminal_id"].as_str().unwrap().to_string();
    call(
        &mut client,
        WorkbenchTreeOp::CreateScene {
            id: Some("alpha".into()),
            name: String::new(),
        },
    );
    let tab = call(
        &mut client,
        WorkbenchTreeOp::CreateTab {
            scene_id: Some("alpha".into()),
            name: "shell".into(),
        },
    );
    let tab_id = tab_id(&tab["tree"], "alpha", "shell");
    call(
        &mut client,
        WorkbenchTreeOp::Split {
            scene_id: Some("alpha".into()),
            tab_id: Some(tab_id.clone()),
            pane_id: None,
            direction: "right".into(),
            terminal_id: Some(terminal_id.clone()),
        },
    );
    let snapshot = client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::PaneSnapshot {
                scene_id: Some("alpha".into()),
                tab_id: Some(tab_id.clone()),
                pane_id: terminal_id.clone(),
            },
        })
        .expect("snapshot");
    assert_eq!(snapshot["terminal_id"], terminal_id);

    call(
        &mut client,
        WorkbenchTreeOp::ClosePane {
            scene_id: Some("alpha".into()),
            tab_id: Some(tab_id.clone()),
            pane_id: terminal_id.clone(),
        },
    );
    let list = client.call(RequestKind::TerminalList).expect("list");
    assert!(
        list.to_string().contains(&terminal_id),
        "closing the pane must leave the terminal registered: {list}"
    );
    client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::PaneInput {
                scene_id: None,
                tab_id: None,
                pane_id: terminal_id.clone(),
                bytes_hex: viva::server::hex_encode(b"echo still-alive\n"),
            },
        })
        .expect("input after close");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw = false;
    while Instant::now() < deadline {
        let snap = client
            .call(RequestKind::WorkbenchTree {
                op: WorkbenchTreeOp::PaneSnapshot {
                    scene_id: None,
                    tab_id: None,
                    pane_id: terminal_id.clone(),
                },
            })
            .expect("snapshot after close");
        if snap.to_string().contains("still-alive") {
            saw = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(saw, "the closed pane's PTY still accepts input");
    let _ = client.call(RequestKind::TerminalStop {
        terminal_id: terminal_id.clone(),
    });
    host.shutdown();
}

#[test]
fn legacy_layout_json_is_readable_and_survives_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let host = Host::open(&home).expect("host");
    let server = host.serve_background();
    let socket = home.join(SOCKET_NAME);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut client = Client::connect(&home).expect("client");
    let legacy = serde_json::json!({
        "grid": {"Split": {"axis": "Horizontal", "ratio": 40, "first": {"Leaf": {"Terminal": "t0"}}, "second": {"Leaf": "Browser"}}},
        "focus": {"Terminal": "t0"}
    })
    .to_string();
    client
        .call(RequestKind::WorkbenchLayout {
            layout_json: Some(legacy),
        })
        .expect("import legacy");
    let layout = client
        .call(RequestKind::WorkbenchLayout { layout_json: None })
        .expect("read");
    let text = layout["layout"].as_str().unwrap();
    assert!(text.contains("\"version\":2"), "{text}");
    assert!(text.contains("t0"), "{text}");
    let mut other = Client::connect(&home).expect("second client");
    let listed = other
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::List,
        })
        .expect("list");
    assert!(listed.to_string().contains("t0"));
    drop(client);
    drop(other);
    let mut stopper = Client::connect(&home).expect("stop");
    stopper
        .call(RequestKind::Shutdown { close_policy: None })
        .expect("shutdown");
    drop(stopper);
    let deadline = Instant::now() + Duration::from_secs(15);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    server.join().expect("server joins");

    let host = Host::open(&home).expect("reopen");
    let server = host.serve_background();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !home.join(SOCKET_NAME).exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut client = Client::connect(&home).expect("after restart");
    let listed = client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::List,
        })
        .expect("list after restart");
    assert!(
        listed.to_string().contains("t0"),
        "the migrated tree is still there: {listed}"
    );
    client
        .call(RequestKind::Shutdown { close_policy: None })
        .expect("shutdown");
    server.join().expect("restarted server joins");
}

#[test]
fn member_tree_edits_need_a_live_grant_and_the_owner_does_not() {
    let host = start();
    let mut client = host.client();
    let mut denied = new_request(RequestKind::WorkbenchTree {
        op: WorkbenchTreeOp::CreateScene {
            id: Some("nope".into()),
            name: "nope".into(),
        },
    });
    denied.member = Some("member-00000000-0000-0000-0000-000000000000".into());
    let err = client.call_request(denied).expect_err("no grant");
    assert!(err.to_string().contains("denied"), "{err}");

    let store = Store::open(&database_path(&host.home), viva::server::migrations()).expect("store");
    let wrong = viva::authority::AuthorityEngine::new(&store)
        .issue_root_grant(
            None,
            None,
            vec!["agent_prompt".into()],
            viva::authority::GrantMode::ActAutonomously,
            None,
        )
        .expect("grant")
        .grant_id
        .to_string();
    let live = viva::authority::AuthorityEngine::new(&store)
        .issue_root_grant(
            None,
            None,
            vec!["terminal_control".into()],
            viva::authority::GrantMode::ActAutonomously,
            None,
        )
        .expect("grant")
        .grant_id
        .to_string();
    drop(store);

    let mut wrong_req = new_request(RequestKind::WorkbenchTree {
        op: WorkbenchTreeOp::CreateScene {
            id: Some("nope".into()),
            name: "nope".into(),
        },
    });
    wrong_req.member = Some("member-00000000-0000-0000-0000-000000000000".into());
    wrong_req.grant = Some(wrong);
    let err = client.call_request(wrong_req).expect_err("wrong action");
    assert!(err.to_string().contains("denied"), "{err}");

    let mut allowed = new_request(RequestKind::WorkbenchTree {
        op: WorkbenchTreeOp::CreateScene {
            id: Some("member-scene".into()),
            name: "Member".into(),
        },
    });
    allowed.member = Some("member-00000000-0000-0000-0000-000000000000".into());
    allowed.grant = Some(live);
    client.call_request(allowed).expect("live grant");
    let listed = call(&mut client, WorkbenchTreeOp::List);
    assert!(listed["tree"]["scenes"].get("member-scene").is_some());
    host.shutdown();
}

#[test]
fn owner_layout_round_trip_still_answers_when_nothing_is_saved() {
    let host = start();
    let mut client = host.client();
    let layout = client
        .call(RequestKind::WorkbenchLayout { layout_json: None })
        .expect("empty layout");
    assert!(layout["layout"].is_null(), "{layout}");
    host.shutdown();
}
