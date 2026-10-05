//! V14 acceptance tests (issue #27): the parallel-development workbench over
//! real git repositories, real PTY terminals and real store records — one
//! office spanning projects/tasks/worktrees, keyboard-switchable panes,
//! per-terminal input isolation, real diffs, and a quit protocol that stops
//! owned processes while preserving worktree contents.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tempfile::TempDir;

use viva::foundation::ids::{MemberId, WorktreeId};
use viva::git::worktrees::ProtectedRefs;
use viva::harness::HarnessSpec;
use viva::terminal::TerminalRegistry;
use viva::tui::workbench::{WorkbenchApp, WorkbenchStore};

// ---------------------------------------------------------------------------
// Real local git fixtures (no network: a bare clone serves as origin)
// ---------------------------------------------------------------------------

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo_with_origin(base: &Path, name: &str) -> PathBuf {
    let repo = base.join(name);
    std::fs::create_dir_all(&repo).expect("repo dir");
    run_git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "seed\n").expect("seed file");
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "seed"]);
    let origin = base.join(format!("{name}-origin.git"));
    run_git(
        base,
        &[
            "clone",
            "--bare",
            repo.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    run_git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    repo
}

fn wait_until(what: &str, timeout: Duration, mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

struct Bench {
    _dir: TempDir,
    store: viva::foundation::store::Store,
    terminals: TerminalRegistry,
}

impl Bench {
    /// WorkbenchStore borrows store + terminals, so build it per call.
    fn workbench(&self) -> WorkbenchStore<'_> {
        WorkbenchStore {
            store: &self.store,
            terminals: &self.terminals,
            protected: ProtectedRefs::new(vec!["main".into(), "master".into()]),
        }
    }
}

fn bench() -> Bench {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().join("home");
    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(&home),
        viva::office::office_migrations(),
    )
    .expect("store");
    let terminals = TerminalRegistry::new();
    Bench {
        _dir: dir,
        store,
        terminals,
    }
}

fn render(app: &WorkbenchApp, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal.draw(|f| app.draw(f)).expect("draw");
    terminal.backend().to_string()
}

fn user_shell_harness(cwd: &Path) -> HarnessSpec {
    HarnessSpec::new("user shell", vec!["/bin/cat".into()], cwd).expect("harness")
}

// ---------------------------------------------------------------------------
// The one-office-many-projects span, with real git facts
// ---------------------------------------------------------------------------

#[test]
fn the_workbench_entry_exists_and_fails_honestly_without_a_tty() {
    // D5/D6: `viva workbench` is a real product entry. Without an
    // interactive terminal it refuses loudly (it never half-runs against a
    // pipe); with one, quit goes through the office shutdown protocol
    // (stop owned terminals → join watchers → persist handoff), which the
    // v07 graceful-shutdown test pins at the protocol level.
    let out = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(["workbench"])
        .env(
            "VIVA_HOME",
            std::env::temp_dir().join("viva-workbench-tty-test"),
        )
        .stdin(std::process::Stdio::null())
        .output()
        .expect("viva runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a piped stdin must not silently pass: {stderr}"
    );
    assert!(stderr.contains("interactive terminal"), "got: {stderr}");
}

#[test]
fn bare_viva_opens_the_workbench_and_fails_honestly_without_a_tty() {
    // Bare `viva` (no subcommand) is the workbench entry: typing `viva`
    // in a real terminal opens the TUI directly. Without a terminal it
    // must fail exactly like `viva workbench` — loudly, never
    // half-running against a pipe.
    let out = Command::new(env!("CARGO_BIN_EXE_viva"))
        .env(
            "VIVA_HOME",
            std::env::temp_dir().join("viva-bare-viva-tty-test"),
        )
        .stdin(std::process::Stdio::null())
        .output()
        .expect("viva runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a piped stdin must not silently pass: {stderr}"
    );
    assert!(stderr.contains("interactive terminal"), "got: {stderr}");
}

#[test]
fn workbench_spans_projects_tasks_worktrees_with_real_git_facts() {
    let b = bench();
    let repo_a = repo_with_origin(b._dir.path(), "alpha");
    let repo_b = repo_with_origin(b._dir.path(), "beta");

    // Two projects, one office.
    let projects = viva::projects::ProjectRegistry::new(&b.store);
    let project_a = projects
        .register(None, "alpha", &repo_a)
        .expect("project a");
    let _project_b = projects.register(None, "beta", &repo_b).expect("project b");

    // A task with an isolated worktree, created through the workbench
    // action layer from the latest remote default ref.
    let task_id = b
        .workbench()
        .create_task(
            "ship the workbench",
            Some(MemberId::new()),
            Some(project_a.project_id.clone()),
        )
        .expect("task");
    let record = b
        .workbench()
        .create_task_worktree(&repo_a, b._dir.path(), &task_id, "agent/ship-workbench")
        .expect("worktree");

    // Real isolation: the worktree exists on disk, on its own branch.
    assert!(record.worktree_path.join("README.md").exists());
    let branch = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&record.worktree_path)
            .output()
            .expect("git")
            .stdout,
    )
    .expect("utf8");
    assert_eq!(branch.trim(), "agent/ship-workbench");

    // Adopting an existing checkout is an explicit selection, recorded.
    let adopted_repo = repo_with_origin(b._dir.path(), "gamma");
    let adopted_path = b._dir.path().join("hand-made");
    run_git(
        &adopted_repo,
        &[
            "worktree",
            "add",
            "-b",
            "agent/handmade",
            adopted_path.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = b
        .workbench()
        .adopt_existing(&adopted_repo, &adopted_path, &task_id)
        .expect("adopt");
    assert_eq!(adopted.source.as_str(), "adopted");

    // The model spans both projects and both worktrees.
    let model = b.workbench().refresh().expect("model");
    assert_eq!(model.projects.len(), 2, "alpha + beta in one office");
    assert_eq!(model.worktrees.len(), 2, "created + adopted");
    let created_row = model
        .worktrees
        .iter()
        .find(|w| w.branch == "agent/ship-workbench")
        .expect("created row");
    assert_eq!(created_row.dirty, Some(false), "clean is a real git fact");
    assert_eq!(created_row.source, "created");

    // Rendering shows the span in the sidebar (provenance itself is
    // asserted on the model above — the 26-column sidebar row carries the
    // branch and the honest dirty state).
    let mut app = WorkbenchApp::new();
    app.set_model(model.clone());
    app.on_key(ratatui::crossterm::event::KeyEvent::from(
        ratatui::crossterm::event::KeyCode::Char('1'),
    ));
    let view = render(&app, 240, 24);
    assert!(view.contains("agent/ship-workbench"));
    assert!(view.contains("clean"));
    assert!(view.contains("└─"), "tree glyphs group worktrees");
}

#[test]
fn dirty_state_and_diff_are_real_facts() {
    let b = bench();
    let repo = repo_with_origin(b._dir.path(), "delta");
    let task_id = b
        .workbench()
        .create_task("edit a file", None, None)
        .expect("task");
    let record = b
        .workbench()
        .create_task_worktree(&repo, b._dir.path(), &task_id, "agent/edit-file")
        .expect("worktree");

    // Before the edit: clean.
    let model = b.workbench().refresh().expect("model");
    assert!(model.worktrees.iter().all(|w| w.dirty == Some(false)));

    // After the edit: dirty + a real bounded diff.
    std::fs::write(
        record.worktree_path.join("README.md"),
        "seed\nchanged line\n",
    )
    .expect("edit");
    let model = b.workbench().refresh().expect("model");
    assert_eq!(model.worktrees[0].dirty, Some(true));
    let diff = b
        .workbench()
        .worktree_diff(&record.worktree_path, 8_192)
        .expect("diff");
    assert!(
        diff.contains("changed line"),
        "the real diff shows the edit"
    );

    // And the diff overlay renders.
    let mut app = WorkbenchApp::new();
    app.set_model(model);
    app.set_diff_view(Some(diff));
    let view = render(&app, 120, 24);
    assert!(view.contains("changed line"));
}

#[test]
fn terminals_per_worktree_take_focused_input_without_crossing_neighbors() {
    let b = bench();
    let repo = repo_with_origin(b._dir.path(), "epsilon");
    let task_id = b
        .workbench()
        .create_task("two terminals", None, None)
        .expect("task");
    let record = b
        .workbench()
        .create_task_worktree(&repo, b._dir.path(), &task_id, "agent/two-terminals")
        .expect("worktree");

    // Two purpose-terminals on ONE worktree: a user shell and a test run.
    let worktree_id: WorktreeId = record.worktree_id.clone();
    let shell = b
        .workbench()
        .open_terminal(
            Some(&worktree_id),
            "user shell",
            &user_shell_harness(&record.worktree_path),
        )
        .expect("open shell");
    let mut aux_harness = user_shell_harness(&record.worktree_path);
    aux_harness.name = "test run".into();
    let aux = b
        .workbench()
        .open_terminal(Some(&worktree_id), "test run", &aux_harness)
        .expect("open aux");

    // Focused input lands in the focused terminal only.
    let shell_handle = b.terminals.handle(&shell).expect("handle").expect("live");
    shell_handle.input(b"hello-workbench\n").expect("input");
    wait_until("shell echoes", Duration::from_secs(5), || {
        shell_handle
            .snapshot()
            .map(|s| s.visible.iter().any(|l| l.contains("hello-workbench")))
            .unwrap_or(false)
    });
    let aux_handle = b.terminals.handle(&aux).expect("handle").expect("live");
    let aux_snapshot = aux_handle.snapshot().expect("snapshot");
    assert!(
        !aux_snapshot
            .visible
            .iter()
            .any(|l| l.contains("hello-workbench")),
        "input must not leak to the neighbor"
    );

    // Stopping one terminal leaves the other alive.
    b.workbench().stop_terminal(&shell).expect("stop shell");
    wait_until("shell exits", Duration::from_secs(5), || {
        b.terminals
            .handle(&shell)
            .ok()
            .flatten()
            .is_none_or(|h| h.try_wait().ok().flatten().is_some())
    });
    let aux_still_live = aux_handle.try_wait().expect("wait").is_none();
    assert!(aux_still_live, "the neighbor keeps running");

    // The terminal list reflects the real states (no guessing).
    let model = b.workbench().refresh().expect("model");
    let shell_row = model
        .terminals
        .iter()
        .find(|t| t.terminal_id == shell.to_string())
        .expect("row");
    let aux_row = model
        .terminals
        .iter()
        .find(|t| t.terminal_id == aux.to_string())
        .expect("row");
    assert_eq!(shell_row.live, Some(false));
    assert_eq!(aux_row.live, Some(true));
    assert_eq!(shell_row.owner_label, "user_shell");
    assert_eq!(
        aux_row.owner_label, "user_shell",
        "aux shells never fake a member execution"
    );
}

#[test]
fn quit_protocol_stops_owned_processes_and_preserves_the_work() {
    let b = bench();
    let repo = repo_with_origin(b._dir.path(), "zeta");
    let task_id = b
        .workbench()
        .create_task("preserve me", None, None)
        .expect("task");
    let record = b
        .workbench()
        .create_task_worktree(&repo, b._dir.path(), &task_id, "agent/preserve")
        .expect("worktree");

    let shell = b
        .workbench()
        .open_terminal(
            Some(&record.worktree_id),
            "user shell",
            &user_shell_harness(&record.worktree_path),
        )
        .expect("open");
    let handle = b.terminals.handle(&shell).expect("handle").expect("live");
    handle.input(b"before-quit\n").expect("input");

    // Work done in the worktree before quitting.
    std::fs::write(record.worktree_path.join("notes.txt"), "uncommitted work\n").expect("write");

    // The quit protocol: stop owned terminals (idempotent), then verify.
    b.workbench().stop_terminal(&shell).expect("stop");
    wait_until("terminal exited", Duration::from_secs(5), || {
        handle.try_wait().map(|e| e.is_some()).unwrap_or(false)
    });

    // Nothing was deleted: the worktree, the uncommitted file and the
    // office record all survive.
    assert!(record.worktree_path.join("notes.txt").exists());
    assert!(record.worktree_path.join("README.md").exists());
    let model = b.workbench().refresh().expect("model");
    assert_eq!(model.worktrees.len(), 1, "worktree record kept");
    assert_eq!(model.worktrees[0].branch, "agent/preserve");
}
