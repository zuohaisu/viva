//! QA round 1 regression tests (issues #43–#48, PR #50 review): the fixes
//! are exercised through the REAL binary where the bugs lived - CLI
//! routing (F1) and the full server-restart upgrade path.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use viva::office::{OFFICE_SOCKET_NAME, OfficeClient, OfficeHost, OfficeRequestKind};

fn viva_bin() -> &'static str {
    env!("CARGO_BIN_EXE_viva")
}

fn wait_for_socket(home: &Path, secs: u64) {
    let socket = home.join(OFFICE_SOCKET_NAME);
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// QA F1 (issue #45 AC): `viva server-restart` is REACHABLE — the whole
/// upgrade path runs through the real binary: the running host transfers
/// its live terminal to a freshly spawned resumed server, the CLI reports
/// `restarted: true` with a NEW pid, and the terminal survives.
#[test]
fn server_restart_is_reachable_and_hands_over_through_the_real_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");

    // A live host in-process (the old generation) with one real terminal.
    let host = OfficeHost::open(&home).expect("host claims the slot");
    let old_pid = std::process::id();
    let server = host.serve_background();
    wait_for_socket(&home, 5);

    let mut client = OfficeClient::connect(&home).expect("client");
    let created = client
        .call(OfficeRequestKind::TerminalCreate {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo restart-e2e; sleep 60".into(),
            ],
            cwd: home.display().to_string(),
            env: vec![],
            cols: 80,
            rows: 24,
            purpose: "pre-upgrade session".into(),
            worktree_id: None,
            owner: "user_shell".into(),
        })
        .expect("create");
    let terminal_id = created
        .get("terminal_id")
        .and_then(|v| v.as_str())
        .expect("terminal id")
        .to_string();
    drop(client);

    // The real CLI entry: this is the exact command QA ran by hand.
    let output = Command::new(viva_bin())
        .arg("server-restart")
        .env("VIVA_HOME", &home)
        .output()
        .expect("viva server-restart runs");
    assert!(
        output.status.success(),
        "server-restart must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"restarted\""), "{stdout}");
    assert!(stdout.contains("true"), "{stdout}");

    // The new host answers, and it is NOT the old process.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        if let Ok(client) = OfficeClient::connect(&home) {
            break client;
        }
        assert!(Instant::now() < deadline, "the resumed host never answered");
        std::thread::sleep(Duration::from_millis(100));
    };
    let status = client.call(OfficeRequestKind::Ping).expect("ping");
    let new_pid = status.get("pid").and_then(|p| p.as_u64()).expect("pid");
    assert_ne!(new_pid, u64::from(old_pid), "a new process must serve");

    // The live terminal survived the upgrade.
    let list = client.call(OfficeRequestKind::TerminalList).expect("list");
    assert!(
        format!("{list}").contains(&terminal_id),
        "the terminal survived the real-binary upgrade: {list}"
    );

    // Clean teardown of the resumed (detached) server.
    client
        .call(OfficeRequestKind::Shutdown { close_policy: None })
        .expect("shutdown");
    drop(client);
    {
        let socket = home.join(OFFICE_SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(15);
        while socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    server.join().expect("old host joins");
}

/// QA F1 negative half: an unknown subcommand still fails loudly, so the
/// routing table test cannot pass by accident.
#[test]
fn unknown_subcommands_still_fail_loudly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let output = Command::new(viva_bin())
        .arg("server-restaart")
        .env("VIVA_HOME", &home)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("binary runs");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown or missing subcommand"),
        "the old failure text, proving the table is what routes: {stderr}"
    );
}
