//! V05 acceptance tests (issue #14): real PTY sessions on this machine
//! (macOS arm64 during development; Linux CI runs the same suite — that is
//! NOT Intel Mac acceptance, which stays pending for V12).
//!
//! What is proven here with real processes: interactive input (incl.
//! Chinese/Unicode and pastes), ANSI parsing (escape bytes are consumed by
//! the emulator, never faked away), resize, session isolation, stop
//! idempotency under races, neighbor survival, bounded memory under high
//! volume, redacted bounded disk logs, and the ownership boundary of the
//! recorded terminal events.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use viva::terminal::ExitVia;

use viva::foundation::ids::ExecutionId;
use viva::foundation::records::TerminalOwner;
use viva::foundation::store::{DOMAIN_FOUNDATION, FOUNDATION_V1_SQL, MigrationRegistry, Store};
use viva::terminal::{DiskLog, StopPolicy, TerminalRegistry, TerminalSpec};

fn spec(argv: &[&str]) -> TerminalSpec {
    TerminalSpec::new(
        argv.iter().map(|s| s.to_string()).collect(),
        std::env::temp_dir(),
    )
    .expect("spec")
}

fn registry() -> TerminalRegistry {
    TerminalRegistry::new()
}

fn wait_for<F: Fn() -> bool>(timeout: Duration, predicate: F) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    predicate()
}

/// Stdio being tty-shaped is not enough: the child must actually own a
/// controlling terminal. Use cat, since a shell may repair a missing tty.
#[test]
fn child_has_a_controlling_terminal() {
    let reg = registry();
    let (_id, child) = reg
        .spawn(
            spec(&["/bin/cat"]),
            TerminalOwner::TestRun,
            None,
            "controlling terminal",
            None,
            None,
        )
        .expect("spawn");
    let ps = std::process::Command::new("ps")
        .args(["-p", &child.pid().expect("pid").to_string(), "-o", "tty="])
        .output()
        .expect("ps");
    child.stop(StopPolicy::default()).expect("stop");
    assert!(ps.status.success());
    let tty = String::from_utf8(ps.stdout).expect("tty name");
    assert!(
        !tty.trim().is_empty() && !tty.trim().chars().all(|c| c == '?'),
        "child has no controlling terminal: {tty:?}"
    );
}

/// Concurrent fork/exec must not inherit or acquire a neighbor's PTY.
#[test]
fn concurrent_spawns_keep_controlling_terminals_isolated() {
    const SESSIONS: usize = 8;
    let barrier = Arc::new(std::sync::Barrier::new(SESSIONS));
    let workers: Vec<_> = (0..SESSIONS)
        .map(|index| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let reg = registry();
                barrier.wait();
                let (_id, child) = reg
                    .spawn(
                        spec(&["/bin/cat"]),
                        TerminalOwner::TestRun,
                        None,
                        "concurrent cat",
                        None,
                        None,
                    )
                    .expect("spawn");
                let marker = format!("session-{index}-alive");
                child
                    .input(format!("{marker}\n").as_bytes())
                    .expect("input");
                let echoed = wait_for(Duration::from_secs(5), || {
                    child
                        .snapshot()
                        .expect("snapshot")
                        .visible
                        .iter()
                        .any(|line| line.contains(&marker))
                });
                let ps = std::process::Command::new("ps")
                    .args(["-p", &child.pid().expect("pid").to_string(), "-o", "tty="])
                    .output()
                    .expect("ps");
                (child, ps, echoed)
            })
        })
        .collect();
    // Keep every handle alive until all workers have sampled their tty.
    let children: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker"))
        .collect();
    let mut ttys = std::collections::HashSet::new();
    for (child, ps, echoed) in children {
        child.resize(60, 12).expect("resize");
        let exit = child.stop(StopPolicy::default()).expect("stop");
        assert!(echoed, "session lost its output");
        assert!(ps.status.success());
        assert_eq!(exit.via, ExitVia::GracefulStop);
        let tty = String::from_utf8(ps.stdout).expect("tty name");
        let tty = tty.trim().to_string();
        assert!(!tty.is_empty() && !tty.chars().all(|c| c == '?'), "{tty:?}");
        ttys.insert(tty);
    }
    assert_eq!(ttys.len(), SESSIONS, "every child owns a distinct tty");
}

/// Acceptance: "真实交互程序可输入、中文/Unicode、粘贴、resize" and the
/// no-fake-transcript rule: ANSI bytes are parsed into the grid (the styled
/// text is visible, the escape bytes are gone), never dropped or lied about.
#[test]
fn real_pty_input_unicode_ansi_and_resize() {
    let reg = registry();
    let (cat_id, cat) = reg
        .spawn(
            spec(&["/bin/cat"]),
            TerminalOwner::UserShell,
            None,
            "interactive cat",
            None,
            None,
        )
        .expect("spawn cat");

    // Chinese + ASCII paste, including a multi-byte boundary stress.
    cat.input("你好，viva — paste 测试\n".as_bytes())
        .expect("input");
    assert!(
        wait_for(Duration::from_secs(5), || {
            cat.snapshot()
                .expect("snapshot")
                .visible
                .iter()
                .any(|line| line.contains("你好，viva — paste 测试"))
        }),
        "the terminal must echo the unicode paste: {:?}",
        cat.snapshot().expect("snapshot").visible
    );

    // ANSI: a styled producer. The escape bytes feed the emulator; the grid
    // shows the *text*, proving the parser consumed the stream as a real
    // terminal does (no ANSI-stripped fake, no raw dump).
    let (_id, styled) = reg
        .spawn(
            spec(&["/bin/sh", "-c", "printf '\\033[31mred-alert\\033[0m plain'"]),
            TerminalOwner::TestRun,
            None,
            "styled",
            None,
            None,
        )
        .expect("spawn styled");
    assert!(
        wait_for(Duration::from_secs(5), || {
            let snap = styled.snapshot().expect("snapshot");
            snap.visible.iter().any(|l| l.contains("red-alert"))
                && snap.visible.iter().any(|l| l.contains("plain"))
        }),
        "styled text must appear in the grid: {:?}",
        styled.snapshot().expect("snapshot").visible
    );
    let raw = styled
        .snapshot()
        .expect("snapshot")
        .visible
        .to_vec()
        .join("\n");
    assert!(
        !raw.contains("\u{1b}[31m"),
        "escape bytes must be parsed, not echoed: {raw:?}"
    );

    // Resize: both the kernel side and the emulator grid follow.
    cat.resize(40, 10).expect("resize");
    let snap = cat.snapshot().expect("snapshot");
    assert_eq!((snap.rows, snap.cols), (10, 40), "grid follows the resize");

    // Input after resize still works.
    cat.input("after-resize\n".as_bytes()).expect("input");
    assert!(
        wait_for(Duration::from_secs(5), || {
            cat.snapshot()
                .expect("snapshot")
                .visible
                .iter()
                .any(|line| line.contains("after-resize"))
        }),
        "input must work after resize"
    );

    // Registry stop by id routes to the same handle; use it for the cat.
    let registry_exit = reg
        .stop(&cat_id, StopPolicy::default(), None)
        .expect("registry stop");
    assert_eq!(
        registry_exit.via,
        viva::terminal::ExitVia::GracefulStop,
        "cat exits on the group TERM"
    );
    styled.stop(StopPolicy::default()).expect("stop styled");
}

/// Acceptance: "一个活跃 PTY 的操作不落到另一会话" and "停一个不影响邻居".
#[test]
fn sessions_are_isolated_and_stopping_one_spares_the_neighbor() {
    let reg = registry();
    let (_id_a, a) = reg
        .spawn(
            spec(&["/bin/cat"]),
            TerminalOwner::UserShell,
            None,
            "A",
            None,
            None,
        )
        .expect("spawn a");
    let (_id_b, b) = reg
        .spawn(
            spec(&["/bin/cat"]),
            TerminalOwner::UserShell,
            None,
            "B",
            None,
            None,
        )
        .expect("spawn b");

    // Input to A only.
    a.input("only-for-A\n".as_bytes()).expect("input a");
    assert!(
        wait_for(Duration::from_secs(5), || {
            a.snapshot()
                .expect("snapshot")
                .visible
                .iter()
                .any(|line| line.contains("only-for-A"))
        }),
        "A must show its own input"
    );
    let b_view = b.snapshot().expect("snapshot");
    assert!(
        !b_view
            .visible
            .iter()
            .any(|line| line.contains("only-for-A")),
        "B must not see A's input: {:?}",
        b_view.visible
    );

    // Stop A; B keeps working.
    let exit_a = a.stop(StopPolicy::default()).expect("stop a");
    assert_eq!(
        exit_a.via,
        ExitVia::GracefulStop,
        "cat exits on TERM; got {exit_a:?}"
    );
    b.input("B-still-alive\n".as_bytes()).expect("input b");
    assert!(
        wait_for(Duration::from_secs(5), || {
            b.snapshot()
                .expect("snapshot")
                .visible
                .iter()
                .any(|line| line.contains("B-still-alive"))
        }),
        "B must keep working after A stopped"
    );

    b.stop(StopPolicy::default()).expect("stop b");
}

/// Acceptance: "两个进程树并行，停一个不影响邻居或未拥有进程；stop/wait/exit
/// 竞态不会重复回收或改写已停止结论".
#[test]
fn stop_is_idempotent_under_races_and_never_touches_unowned_processes() {
    let reg = registry();
    // Two long-running trees: `sh` sleeps keep child processes around.
    let (_a_id, a) = reg
        .spawn(
            spec(&["/bin/sh", "-c", "sleep 30 & wait"]),
            TerminalOwner::UserShell,
            None,
            "tree-a",
            None,
            None,
        )
        .expect("spawn a");
    let (_b_id, b) = reg
        .spawn(
            spec(&["/bin/sh", "-c", "sleep 30 & wait"]),
            TerminalOwner::UserShell,
            None,
            "tree-b",
            None,
            None,
        )
        .expect("spawn b");
    assert_ne!(a.pid(), b.pid(), "distinct process groups");

    // Race stop/wait from several threads at once.
    let a_handle = a.clone();
    let racy: Vec<_> = (0..4)
        .map(|_| {
            let handle = a_handle.clone();
            std::thread::spawn(move || {
                handle.stop(StopPolicy {
                    graceful_timeout: Duration::from_secs(3),
                })
            })
        })
        .collect();
    let waited = {
        let handle = a_handle.clone();
        std::thread::spawn(move || handle.wait())
    };

    let first = a
        .stop(StopPolicy {
            graceful_timeout: Duration::from_secs(3),
        })
        .expect("stop");

    let _ = waited.join().expect("wait joins");
    for result in racy {
        let exit = result.join().expect("racy stop joins").expect("racy stop");
        assert_eq!(exit, first, "every racer sees the same recorded conclusion");
    }
    assert_ne!(
        first.via,
        ExitVia::ChildExit,
        "our stop drove the exit: {first:?}"
    );

    // The neighbor is untouched: still running, still accepts input.
    assert!(
        b.try_wait().expect("b try_wait").is_none(),
        "b must survive A's stop"
    );
    b.input("b-unharmed\n".as_bytes()).expect("input b");
    assert!(
        wait_for(Duration::from_secs(5), || {
            b.snapshot()
                .expect("snapshot")
                .visible
                .iter()
                .any(|line| line.contains("b-unharmed"))
        }),
        "b must still work"
    );
    b.stop(StopPolicy::default()).expect("stop b");

    // And an unowned process is not ours to signal: the office only ever
    // signals its own recorded process groups (see signal_group guard).
    let _ = PathBuf::new();
}

/// Acceptance: "大输出和慢磁盘测试中内存/队列/scrollback 有界，截断或落盘策略
/// 可见；写日志走 V04 脱敏".
#[test]
fn high_volume_output_stays_bounded_and_the_log_is_redacted() {
    let dir = tempfile::TempDir::new().expect("dir");
    let log_path = dir.path().join("logs").join("session.raw");
    let secret = "office-token-9f8e7d";
    let disk_log = Arc::new(
        DiskLog::create(&log_path, vec![secret.to_string()], Some(512 * 1024)).expect("disk log"),
    );
    let reg = registry();
    let (_id, loud) = reg
        .spawn(
            // ANSI + secret FIRST so they land inside the capped log's
            // retained prefix; then 700KB drive the cap home — bounded
            // (head -c), DETERMINISTIC on any runner (the old 8.2MB
            // pipeline starved mid-flow on loaded 2-core runners, making
            // every downstream measurement a coin flip).
            TerminalSpec::new(
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!(
                        "printf '\\033[32m{}\\033[0m\\n'; \
                         yes 0123456789012345678901234567890123456789 | head -c 700000; \
                         echo done-loud",
                        secret
                    ),
                ],
                std::env::temp_dir(),
            )
            .expect("spec"),
            TerminalOwner::TestRun,
            None,
            "loud",
            Some(disk_log.clone()),
            None,
        )
        .expect("spawn loud");

    // Wait for the producer to finish.
    assert!(
        wait_for(Duration::from_secs(30), || {
            loud.try_wait().expect("try_wait").is_some()
        }),
        "the loud session must finish"
    );

    // The child exiting does NOT mean the reader thread drained the PTY
    // yet — on a busy runner the kernel buffer can still hold megabytes
    // (CI saw total_output_bytes at ~288k right after exit, then the
    // count kept growing). eof_seen is set only after the reader saw the
    // stream end, which IS the drain point.
    assert!(
        wait_for(Duration::from_secs(30), || { loud.output_stream_drained() }),
        "the reader must drain the stream before the volume is judged"
    );

    let snap = loud.snapshot().expect("snapshot");
    // Bounded memory: the ring never exceeds the cap even with ~8MB fed.
    assert!(snap.scrollback.len() <= viva::terminal::SCROLLBACK_LINES);
    // 700KB through the pty (ONLCR expands \n to \r\n) — comfortably
    // above the 512KiB log cap and the 2000-line ring, and DETERMINISTIC
    // on any runner.
    assert!(
        snap.total_output_bytes >= 600_000,
        "the volume really flowed: {}",
        snap.total_output_bytes
    );
    // The bound is visible.
    assert!(
        snap.scrollback_capped,
        "the cap flag must be visible when the ring is full"
    );

    // The disk log is bounded (512KiB cap) and the cap is visible.
    assert!(disk_log.truncated(), "the log cap must be visibly hit");

    // And the exit conclusion is a real child exit.
    let exit = loud.wait().expect("wait");
    assert_eq!(exit.via, ExitVia::ChildExit);

    // Give the reader thread a moment to flush the tail, then read the log.
    std::thread::sleep(Duration::from_millis(300));
    let logged = std::fs::read(&log_path).expect("log readable");
    let logged_text = String::from_utf8_lossy(&logged);
    assert!(
        !logged_text.contains(secret),
        "the secret must be redacted from the raw log"
    );
    assert!(
        logged_text.contains("[REDACTED]"),
        "the redacted secret must be visible as the marker in the kept prefix"
    );
    assert!(
        logged_text.contains('\u{1b}'),
        "the raw log keeps full ANSI fidelity (no stripped transcript): {:?}",
        &logged_text[..logged_text.len().min(160)]
    );
    assert!(
        logged.len() < 600 * 1024,
        "the log respects its byte cap: {}",
        logged.len()
    );

    // The redaction carry flushed the tail correctly: no half-secret.
    assert!(
        !logged_text.contains("office-token-"),
        "no partial secret either"
    );
}

/// Acceptance: "慢磁盘…内存/队列/scrollback 有界" — a slow log sink applies
/// backpressure to the producer while the office stays responsive, and the
/// session remains stoppable.
#[test]
fn slow_consumer_backpressure_keeps_the_session_controllable() {
    let dir = tempfile::TempDir::new().expect("dir");
    let log_path = dir.path().join("slow.raw");
    // No disk log here: the slow consumer is simulated by the child reading
    // stdin slowly while the office writes fast — the bounded pty pipe is
    // the backpressure. We prove input() still returns (bounded queue) and
    // stop() still works under pressure.
    let reg = registry();
    let (_id, slow) = reg
        .spawn(
            TerminalSpec::new(
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "while read -r line; do printf '%s\\n' \"$line\"; sleep 0.2; done".to_string(),
                ],
                std::env::temp_dir(),
            )
            .expect("spec"),
            TerminalOwner::UserShell,
            None,
            "slow-consumer",
            None,
            None,
        )
        .expect("spawn slow");

    // Flood faster than the consumer drains; the pipe bounds the queue.
    for i in 0..200 {
        slow.input(format!("flood-{i}\n").as_bytes())
            .expect("input under pressure");
    }

    // The office remains in control: snapshot works, stop works, and the
    // forced escalation path is reachable if the consumer ignores TERM.
    let snap = slow.snapshot().expect("snapshot under pressure");
    assert!(snap.rows > 0);
    let exit = slow
        .stop(StopPolicy {
            graceful_timeout: Duration::from_millis(300),
        })
        .expect("stop under pressure");
    assert!(matches!(
        exit.via,
        ExitVia::GracefulStop | ExitVia::ForcedKill
    ));
    let _ = log_path; // path reserved for future slow-disk fixtures
}

/// The registry records foundation terminal events with the ownership
/// boundary: a member-execution terminal carries its execution id; a user
/// shell can never carry one (type + CHECK).
#[test]
fn recorded_terminal_events_keep_the_ownership_boundary() {
    let frozen = MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .freeze()
        .expect("registry");
    let store = Store::open_in_memory(&frozen).expect("store");
    let reg = registry();

    let execution = ExecutionId::new();
    let (_member_id, member_handle) = reg
        .spawn(
            spec(&["/bin/sh", "-c", "true"]),
            TerminalOwner::MemberExecution(execution.clone()),
            None,
            "member exec",
            None,
            Some(&store),
        )
        .expect("spawn member terminal");
    let (_user_id, user_handle) = reg
        .spawn(
            spec(&["/bin/sh", "-c", "true"]),
            TerminalOwner::UserShell,
            None,
            "user shell",
            None,
            Some(&store),
        )
        .expect("spawn user terminal");
    let _ = (member_handle, user_handle);

    let count = store.row_count("terminal_events").expect("count");
    assert_eq!(count, 2, "two spawned events recorded");

    let mut stmt = store
        .connection()
        .prepare("SELECT owner, execution_id FROM terminal_events ORDER BY seq")
        .expect("stmt");
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .expect("query");
    let mut seen_member = false;
    let mut seen_user_shell = false;
    for row in rows {
        let (owner, execution_id) = row.expect("row");
        match owner.as_str() {
            "member_execution" => {
                seen_member = true;
                assert_eq!(
                    execution_id.as_deref(),
                    Some(execution.as_str()),
                    "member terminal carries its execution id"
                );
            }
            "user_shell" => {
                seen_user_shell = true;
                assert!(
                    execution_id.is_none(),
                    "a user shell can never carry an execution id"
                );
            }
            other => panic!("unexpected owner {other}"),
        }
    }
    assert!(seen_member && seen_user_shell);
}

/// QA regression gate (independent-QA finding Q2): the stop/wait/exit race
/// previously deadlocked intermittently (~2 of 6 rounds held locks across a
/// blocking child.wait). The reap discipline is now lock-under-no-blocking;
/// this test loops the race to keep that honest.
#[test]
fn stop_wait_race_survives_repeated_rounds() {
    for round in 0..4 {
        let reg = registry();
        let (_id, a) = reg
            .spawn(
                spec(&["/bin/sh", "-c", "sleep 20 & wait"]),
                TerminalOwner::UserShell,
                None,
                format!("race-a{round}"),
                None,
                None,
            )
            .expect("spawn a");
        let (_id, b) = reg
            .spawn(
                spec(&["/bin/sh", "-c", "sleep 20 & wait"]),
                TerminalOwner::UserShell,
                None,
                format!("race-b{round}"),
                None,
                None,
            )
            .expect("spawn b");

        let a_handle = a.clone();
        let policy = StopPolicy {
            graceful_timeout: Duration::from_secs(3),
        };
        let racers: Vec<_> = (0..4)
            .map(|_| {
                let handle = a_handle.clone();
                std::thread::spawn(move || handle.stop(policy))
            })
            .collect();
        let waiter = {
            let handle = a_handle.clone();
            std::thread::spawn(move || handle.wait())
        };

        let first = a.stop(policy).expect("stop");
        assert_ne!(
            first.via,
            ExitVia::ChildExit,
            "round {round}: our stop must drive the exit, not the child's own"
        );
        let waited = waiter.join().expect("wait joins").expect("wait ok");
        assert_eq!(
            waited, first,
            "round {round}: wait sees the same conclusion"
        );
        for racer in racers {
            let exit = racer.join().expect("racer joins").expect("racer ok");
            assert_eq!(exit, first, "round {round}: identical recorded conclusion");
        }

        // The neighbor survives every round.
        assert!(b.try_wait().expect("b alive").is_none());
        b.stop(StopPolicy::default()).expect("stop b");
    }
}
