//! Actual PTY and emulator regression checks; these are not the real Pi/Codex
//! acceptance matrix (recorded separately in validation evidence).
use std::time::{Duration, Instant};
use viva::foundation::records::TerminalOwner;
use viva::terminal::{StopPolicy, TerminalRegistry, TerminalSpec};
fn wait(mut condition: impl FnMut() -> bool) {
    let d = Instant::now() + Duration::from_secs(5);
    while Instant::now() < d {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(condition(), "PTY observation timed out");
}
#[test]
fn handoff_replay_keeps_primary_history_alternate_styles_cursor_and_modes() {
    let reg = TerminalRegistry::new();
    let spec=TerminalSpec::new(vec!["/bin/sh".into(),"-c".into(),"printf '\\033[31m'; i=0; while [ $i -lt 75 ]; do printf 'history-%s\\r\\n' $i; i=$((i+1)); done; printf '\\033[?1049h\\033[38;2;12;34;56m\\033[1m中ALT\\033[3;7H\\033[?2004h\\033[?1003h\\033[?1006h'; read input".into()],std::env::temp_dir()).unwrap();
    let (_, h) = reg
        .spawn(spec, TerminalOwner::TestRun, None, "handoff", None, None)
        .unwrap();
    wait(|| h.snapshot().unwrap().screen.unwrap().modes.alternate);
    let before = h.snapshot().unwrap().screen.unwrap();
    let mut adopted = vt100::Parser::new(40, 120, 2000);
    adopted.process(&h.formatted_screen());
    let after = viva::terminal::screen::project(adopted.screen(), 0, 0);
    assert_eq!(after.lines, before.lines);
    assert_eq!(after.cursor, before.cursor);
    assert_eq!(after.modes, before.modes);
    adopted.process(b"\x1b[?1049l");
    adopted.screen_mut().set_scrollback(usize::MAX);
    let retained = adopted.screen().scrollback();
    assert!(retained >= 36, "lost older history: {retained}");
    assert!(adopted.screen().contents().contains("history-0"));
    assert_eq!(
        adopted.screen().cell(0, 0).unwrap().fgcolor(),
        vt100::Color::Idx(1)
    );
    h.stop(StopPolicy::default()).unwrap();
}
#[test]
fn queries_reply_to_the_same_pty_and_resize_changes_child_geometry() {
    let reg = TerminalRegistry::new();
    let spec=TerminalSpec::new(vec!["/bin/sh".into(),"-c".into(),"stty raw -echo; printf '\\033[5n'; dd bs=1 count=4 2>/dev/null | od -An -tx1; sleep 1; stty size; sleep 30".into()],std::env::temp_dir()).unwrap();
    let (_, h) = reg
        .spawn(spec, TerminalOwner::TestRun, None, "query", None, None)
        .unwrap();
    h.resize(79, 23).unwrap();
    wait(|| {
        h.snapshot()
            .unwrap()
            .visible
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("1b 5b 30 6e")
    });
    wait(|| h.snapshot().unwrap().visible.join(" ").contains("23 79"));
    assert!(h.resize(513, 20).is_err());
    assert!(h.resize(0, 20).is_err());
    h.stop(StopPolicy::default()).unwrap();
}
#[test]
fn host_history_scrolls_without_replacing_the_live_screen() {
    let reg = TerminalRegistry::new();
    let spec = TerminalSpec::new(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "i=0; while [ $i -lt 100 ]; do printf 'line-%s\\r\\n' $i; i=$((i+1)); done; sleep 30"
                .into(),
        ],
        std::env::temp_dir(),
    )
    .unwrap();
    let (_, h) = reg
        .spawn(spec, TerminalOwner::TestRun, None, "history", None, None)
        .unwrap();
    wait(|| h.snapshot().unwrap().screen.unwrap().retained >= 60);
    let live = h.snapshot().unwrap();
    let old = h.snapshot_at(usize::MAX).unwrap();
    assert!(
        old.screen
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .flat_map(|r| r.iter())
            .any(|r| r.0.contains("line-0"))
    );
    assert!(!old.screen.unwrap().cursor_visible);
    assert_eq!(h.snapshot().unwrap().visible, live.visible);
    h.stop(StopPolicy::default()).unwrap();
}

#[test]
fn detached_resident_first_spawn_does_not_acquire_the_childs_tty() {
    use std::os::unix::process::CommandExt;
    use viva::office::{OfficeClient, OfficeRequestKind};
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_viva"));
    command
        .arg("server")
        .env("VIVA_HOME", home.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: only the async-signal-safe setsid call runs after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut server = command.spawn().unwrap();
    wait(|| OfficeClient::connect(home.path()).is_ok());
    let mut client = OfficeClient::connect(home.path()).unwrap();
    let result = client.call(OfficeRequestKind::TerminalCreate {
        argv: vec!["/bin/cat".into()],
        cwd: std::env::temp_dir().to_string_lossy().into(),
        env: vec![],
        cols: 80,
        rows: 24,
        purpose: "first detached spawn".into(),
        worktree_id: None,
        owner: "test_run".into(),
    });
    let shutdown = client.call(OfficeRequestKind::Shutdown { close_policy: None });
    if shutdown.is_err() {
        let _ = server.kill();
    }
    let _ = server.wait();
    assert!(result.is_ok(), "first detached spawn failed: {result:?}");
}
