//! S1 acceptance (issue #74): opening and composing workspaces over real
//! `.code-workspace` files, real git repositories in unrelated parent
//! directories, real linked worktrees, real PTY terminals and the real
//! host socket. Each test maps to one line of the #74 acceptance list.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;
use tempfile::TempDir;

use viva::foundation::ids::WorktreeId;
use viva::foundation::store::Store;
use viva::git::worktrees::ProtectedRefs;
use viva::harness::HarnessSpec;
use viva::terminal::TerminalRegistry;
use viva::tui::workbench::{
    KeyOutcome, WorkbenchAction, WorkbenchApp, WorkbenchModel, WorkbenchStore,
};
use viva::workspaces::navigation::{Navigation, parse_jsonc};

// ---------------------------------------------------------------------------
// Real fixtures
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
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

fn init_repo(parent: &Path, name: &str) -> PathBuf {
    let repo = parent.join(name);
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "seed\n").expect("seed");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "seed"]);
    repo.canonicalize().expect("canonical")
}

fn add_linked(repo: &Path, path: &Path, branch: &str) -> PathBuf {
    git(
        repo,
        &["worktree", "add", "-b", branch, path.to_str().unwrap()],
    );
    path.canonicalize().expect("canonical")
}

fn store() -> Store {
    Store::open_in_memory(viva::server::migrations()).expect("store")
}

fn model(store: &Store, terminals: &TerminalRegistry) -> WorkbenchModel {
    WorkbenchStore {
        store,
        terminals,
        protected: ProtectedRefs::new(vec!["main".into()]),
    }
    .refresh()
    .expect("model")
}

fn count(store: &Store, table: &str) -> i64 {
    store
        .connection()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("count")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn prefix_b() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)
}

fn prefix_then(app: &mut WorkbenchApp, c: char) -> KeyOutcome {
    app.on_key(prefix_b());
    app.on_key(key(KeyCode::Char(c)))
}

fn prefix_shift(app: &mut WorkbenchApp, c: char) -> KeyOutcome {
    app.on_key(prefix_b());
    app.on_key(KeyEvent::new(
        KeyCode::Char(c.to_ascii_uppercase()),
        KeyModifiers::SHIFT,
    ))
}

/// Two repositories under unrelated parents, repo-b with one linked
/// worktree, plus the directory the workspace file lives in.
struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    repo_a: PathBuf,
    repo_b: PathBuf,
    linked_b: PathBuf,
    ws_dir: PathBuf,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().expect("dir");
    let root = dir.path().canonicalize().expect("root");
    let repo_a = init_repo(&root.join("work-one"), "repo-a");
    let repo_b = init_repo(&root.join("work-two"), "repo-b");
    let linked_b = add_linked(
        &repo_b,
        &root.join("work-two").join("repo-b-feature"),
        "feature",
    );
    let ws_dir = root.join("workspaces");
    std::fs::create_dir_all(&ws_dir).expect("ws dir");
    Fixture {
        _dir: dir,
        root,
        repo_a,
        repo_b,
        linked_b,
        ws_dir,
    }
}

fn names(rows: &[viva::workspaces::navigation::FolderRow]) -> Vec<String> {
    rows.iter().map(|r| r.name.clone()).collect()
}

const COMMENTED_WORKSPACE: &str = r#"{
    // Team layout: comments and trailing commas are legal JSONC.
    "folders": [
        { "path": "ABS_A", "name": "Alpha" },   /* absolute path + display name */
        { "path": "../work-two/repo-b" },        // relative to this file, no name
    ],
    "settings": { "editor.tabSize": 2, "files.exclude": { "**/.git": true } },
    "tasks": { "version": "2.0.0", "tasks": [ { "label": "boom", "type": "shell", "command": "touch MARKER" } ] },
    "launch": { "configurations": [ { "name": "x", "preLaunchTask": "boom" } ] },
}
"#;

fn write_commented_workspace(f: &Fixture) -> PathBuf {
    let file = f.ws_dir.join("demo.code-workspace");
    let text = COMMENTED_WORKSPACE.replace("ABS_A", f.repo_a.to_str().unwrap());
    std::fs::write(&file, text).expect("write workspace");
    file
}

// ---------------------------------------------------------------------------
// #74 acceptance 1: a real commented .code-workspace groups projects and
// Git checkouts correctly
// ---------------------------------------------------------------------------

#[test]
fn a_real_workspace_file_groups_projects_and_checkouts_by_repo() {
    let f = fixture();
    let file = write_commented_workspace(&f);
    let before = std::fs::read_to_string(&file).unwrap();
    let store = store();
    let terminals = TerminalRegistry::new();
    let nav = Navigation { store: &store };

    let id = nav.open(&file).expect("open");

    let folders = nav.folders(&id).unwrap();
    assert_eq!(folders.len(), 2);
    assert!(
        folders.iter().all(|r| r.diagnostic.is_none()),
        "{folders:?}"
    );
    assert_eq!(
        names(&folders),
        ["Alpha", "repo-b"],
        "explicit `name` wins; otherwise VS Code shows the directory name, never the raw relative path"
    );
    assert_eq!(folders[1].path.as_deref(), f.repo_b.to_str());

    let m = model(&store, &terminals);
    assert_eq!(m.workspace_id.as_deref(), Some(id.as_str()));
    assert_eq!(
        m.projects
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "repo-b"],
        "projects follow the composition order and its display names"
    );
    let project_a = &m.projects[0].project_id;
    let project_b = &m.projects[1].project_id;
    assert_ne!(project_a, project_b);

    let rows_of = |pid: &str| -> Vec<String> {
        let mut paths: Vec<String> = m
            .worktrees
            .iter()
            .filter(|w| w.project_id == pid)
            .map(|w| w.path.clone())
            .collect();
        paths.sort();
        paths
    };
    assert_eq!(rows_of(project_a), [f.repo_a.to_str().unwrap()]);
    let mut expected_b = vec![
        f.repo_b.to_str().unwrap().to_string(),
        f.linked_b.to_str().unwrap().to_string(),
    ];
    expected_b.sort();
    assert_eq!(
        rows_of(project_b),
        expected_b,
        "main checkout and linked worktree group under ONE repo node"
    );
    assert_eq!(
        count(&store, "projects"),
        2,
        "the linked worktree is not registered as a second repository"
    );

    assert!(
        !f.ws_dir.join("MARKER").exists() && !f.root.join("MARKER").exists(),
        "tasks/launch configuration in the file are data, never executed"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "opening never rewrites the source file"
    );
}

// ---------------------------------------------------------------------------
// #74 acceptance 2: compose from empty, save, VS Code can open it, reopen
// keeps names and relations, non-folders fields survive
// ---------------------------------------------------------------------------

#[test]
fn a_composition_saves_a_file_vscode_can_open_and_reopening_keeps_everything() {
    let f = fixture();
    let store = store();
    let nav = Navigation { store: &store };

    let id = nav.create("combo").unwrap();
    assert!(nav.folders(&id).unwrap().is_empty(), "starts empty");
    nav.add(&id, serde_json::json!({"path": f.repo_a}), Path::new("/"))
        .unwrap();
    nav.add(
        &id,
        serde_json::json!({"path": f.repo_b, "name": "Backend"}),
        Path::new("/"),
    )
    .unwrap();
    let saved = f.ws_dir.join("combo.code-workspace");
    nav.save_as(&id, &saved).unwrap();

    // The saved file is what VS Code reads: a JSON object with a folders array.
    let doc = parse_jsonc(&std::fs::read_to_string(&saved).unwrap()).expect("valid JSONC");
    let listed: Vec<(String, Option<String>)> = doc["folders"]
        .as_array()
        .expect("folders array")
        .iter()
        .map(|e| {
            (
                e["path"].as_str().unwrap().to_string(),
                e["name"].as_str().map(str::to_string),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            (f.repo_a.to_str().unwrap().to_string(), None),
            (
                f.repo_b.to_str().unwrap().to_string(),
                Some("Backend".into())
            ),
        ]
    );

    // A different process (fresh store) reopens it with the same names/relations.
    let fresh = Store::open_in_memory(viva::server::migrations()).unwrap();
    let nav2 = Navigation { store: &fresh };
    let reopened = nav2.open(&saved).unwrap();
    let rows = nav2.folders(&reopened).unwrap();
    assert_eq!(names(&rows), ["repo-a", "Backend"]);
    assert_eq!(rows[1].path.as_deref(), f.repo_b.to_str());
}

#[test]
fn saving_only_changes_the_folder_list_comments_other_fields_and_relative_paths_stay() {
    let f = fixture();
    let file = write_commented_workspace(&f);
    let extra = init_repo(&f.root.join("work-three"), "repo-c");
    let store = store();
    let nav = Navigation { store: &store };
    let id = nav.open(&file).unwrap();
    nav.add(&id, serde_json::json!({"path": extra}), Path::new("/"))
        .unwrap();

    nav.save_as(&id, &file).unwrap(); // explicit save over the source file
    let after = std::fs::read_to_string(&file).unwrap();

    assert!(
        after.contains("// Team layout"),
        "comments survive: {after}"
    );
    assert!(
        after.contains("\"editor.tabSize\": 2"),
        "settings text untouched"
    );
    assert!(after.contains("\"preLaunchTask\""), "launch survives");
    assert!(
        after.contains("../work-two/repo-b"),
        "a relative entry is not rewritten to an absolute one: {after}"
    );
    let doc = parse_jsonc(&after).expect("still valid JSONC");
    let paths: Vec<&str> = doc["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths.len(), 3);
    assert_eq!(paths[2], extra.to_str().unwrap());
    assert_eq!(doc["folders"][0]["name"], "Alpha");
}

#[test]
fn save_as_elsewhere_rebases_relative_entries_and_binds_the_new_file() {
    let f = fixture();
    let file = write_commented_workspace(&f);
    let store = store();
    let nav = Navigation { store: &store };
    let id = nav.open(&file).unwrap();

    let nested = f.root.join("elsewhere").join("deeper");
    std::fs::create_dir_all(&nested).unwrap();
    let copy = nested.join("copy.code-workspace");
    nav.save_as(&id, &copy).unwrap();

    let doc = parse_jsonc(&std::fs::read_to_string(&copy).unwrap()).unwrap();
    let rel = doc["folders"][1]["path"].as_str().unwrap();
    assert_eq!(
        nested.join(rel).canonicalize().unwrap(),
        f.repo_b,
        "the rebased relative path still resolves to repo-b: {rel}"
    );
    assert!(!rel.contains("/./"), "{rel}");
    assert_eq!(
        nav.open(&copy).unwrap(),
        id,
        "the saved file is now this workspace, not a duplicate"
    );
    assert_eq!(nav.list().unwrap().len(), 1);
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&copy).unwrap().permissions());
    assert_ne!(
        mode & 0o044,
        0,
        "a saved workspace file is readable like any other file (mode {mode:o})"
    );
}

// ---------------------------------------------------------------------------
// #74 acceptance 3: a single repo opens directly; add another and save
// ---------------------------------------------------------------------------

#[test]
fn a_single_repo_opens_without_a_task_and_is_not_duplicated_by_reopening() {
    let f = fixture();
    let store = store();
    let nav = Navigation { store: &store };
    assert_eq!(count(&store, "tasks"), 0);

    let first = nav.open(&f.repo_a).unwrap();
    let again = nav.open(&f.repo_a).unwrap();
    assert_eq!(
        first, again,
        "re-opening the same directory reuses its workspace"
    );
    assert_eq!(nav.list().unwrap().len(), 1, "recents are not spammed");
    assert_eq!(nav.selected().unwrap(), Some(first.clone()));

    nav.add(
        &first,
        serde_json::json!({"path": f.repo_b}),
        Path::new("/"),
    )
    .unwrap();
    let out = f.ws_dir.join("pair.code-workspace");
    nav.save_as(&first, &out).unwrap();
    let doc = parse_jsonc(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc["folders"].as_array().unwrap().len(), 2);
    assert_eq!(
        count(&store, "tasks"),
        0,
        "no task/workspace ceremony was needed"
    );
}

// ---------------------------------------------------------------------------
// #74 acceptance 4: A→B→A filters correctly; shared repos and live scenes
// are untouched
// ---------------------------------------------------------------------------

#[test]
fn switching_a_b_a_filters_the_tree_and_never_copies_or_stops_anything() {
    let f = fixture();
    let store = store();
    let terminals = TerminalRegistry::new();
    let nav = Navigation { store: &store };

    let a = nav.create("A").unwrap();
    nav.add(&a, serde_json::json!({"path": f.repo_a}), Path::new("/"))
        .unwrap();
    nav.add(&a, serde_json::json!({"path": f.repo_b}), Path::new("/"))
        .unwrap();
    let b = nav.create("B").unwrap();
    nav.add(&b, serde_json::json!({"path": f.repo_b}), Path::new("/"))
        .unwrap(); // shared repo
    assert_eq!(
        count(&store, "projects"),
        2,
        "the shared repo is one stable project"
    );

    // A live terminal in repo-b's linked worktree.
    let linked_id = {
        nav.select(&a).unwrap();
        let m = model(&store, &terminals);
        m.worktrees
            .iter()
            .find(|w| w.path == f.linked_b.to_str().unwrap())
            .expect("linked worktree discovered")
            .worktree_id
            .clone()
    };
    let wb = WorkbenchStore {
        store: &store,
        terminals: &terminals,
        protected: ProtectedRefs::new(vec!["main".into()]),
    };
    let harness = HarnessSpec::new("cat", vec!["/bin/cat".into()], &f.linked_b).unwrap();
    let terminal_id = wb
        .open_terminal(
            Some(&linked_id.parse::<WorktreeId>().unwrap()),
            "shell",
            &harness,
        )
        .unwrap();

    let projects_in = |m: &WorkbenchModel| -> Vec<String> {
        let mut p: Vec<String> = m.projects.iter().map(|p| p.name.clone()).collect();
        p.sort();
        p
    };
    nav.select(&a).unwrap();
    let in_a = model(&store, &terminals);
    assert_eq!(projects_in(&in_a), ["repo-a", "repo-b"]);
    nav.select(&b).unwrap();
    let in_b = model(&store, &terminals);
    assert_eq!(projects_in(&in_b), ["repo-b"]);
    assert!(
        in_b.worktrees
            .iter()
            .all(|w| !w.path.starts_with(f.repo_a.to_str().unwrap())),
        "repo-a's checkouts are not shown in B"
    );
    nav.select(&a).unwrap();
    let back = model(&store, &terminals);
    assert_eq!(projects_in(&back), ["repo-a", "repo-b"]);
    assert_eq!(
        back.worktrees.len(),
        in_a.worktrees.len(),
        "A is restored exactly"
    );

    assert_eq!(count(&store, "projects"), 2, "no project was copied");
    assert_eq!(
        count(&store, "task_worktrees"),
        0,
        "no execution ownership appeared"
    );
    let still = terminals
        .list()
        .into_iter()
        .find(|t| t.terminal_id.to_string() == terminal_id.to_string())
        .expect("terminal still registered");
    assert_eq!(still.worktree_id.map(|w| w.to_string()), Some(linked_id));
    assert!(
        terminals
            .handle(&terminal_id)
            .unwrap()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none(),
        "the process in the scene kept running across A→B→A"
    );
    let _ = terminals.stop(
        &terminal_id,
        viva::terminal::StopPolicy::default(),
        Some(&store),
    );
}

/// Execute one TUI action the way the resident host would, then refresh the
/// view from the store: key → action → host handler → model → screen.
fn run_action(
    app: &mut WorkbenchApp,
    store: &Store,
    terminals: &TerminalRegistry,
    outcome: KeyOutcome,
) -> Value {
    match outcome {
        KeyOutcome::Action(WorkbenchAction::Workspace {
            action,
            value,
            project_id,
        }) => {
            let reply = Navigation { store }
                .apply(&action, value.as_deref(), project_id.as_deref())
                .expect("host applies the action");
            app.set_model(model(store, terminals));
            reply
        }
        other => panic!("expected a workspace action, got {other:?}"),
    }
}

fn render(app: &mut WorkbenchApp, w: u16, h: u16) -> (String, ratatui::buffer::Buffer) {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    terminal.draw(|f| app.draw(f)).expect("draw");
    (
        terminal.backend().to_string(),
        terminal.backend().buffer().clone(),
    )
}

#[test]
fn the_top_switcher_reaches_every_workspace_and_is_stable_across_switches() {
    let f = fixture();
    let store = store();
    let terminals = TerminalRegistry::new();
    let nav = Navigation { store: &store };
    let ids: Vec<String> = ["one", "two", "three"]
        .iter()
        .map(|n| {
            let id = nav.create(n).unwrap();
            nav.add(&id, serde_json::json!({"path": f.repo_a}), Path::new("/"))
                .unwrap();
            id
        })
        .collect();

    let mut app = WorkbenchApp::new();
    app.set_model(model(&store, &terminals));
    let strip = |app: &mut WorkbenchApp| {
        let (text, _) = render(app, 120, 30);
        text.lines().next().unwrap_or_default().to_string()
    };
    let first_strip = strip(&mut app);
    for name in ["All projects", "one", "two", "three"] {
        assert!(
            first_strip.contains(name),
            "switcher shows {name}: {first_strip}"
        );
    }
    let at = |s: &str, n: &str| s.find(n).unwrap_or_else(|| panic!("{n} in {s}"));
    assert!(
        at(&first_strip, "All projects") < at(&first_strip, "one")
            && at(&first_strip, "one") < at(&first_strip, "two")
            && at(&first_strip, "two") < at(&first_strip, "three"),
        "stable creation order: {first_strip}"
    );

    let order: Vec<Option<String>> = std::iter::once(None)
        .chain(ids.iter().cloned().map(Some))
        .collect();
    let mut visited = Vec::new();
    for id in order.iter().chain(order.iter()) {
        let value = id.as_ref().map(|i| i.as_str()).unwrap_or("all").to_string();
        let out = KeyOutcome::Action(WorkbenchAction::Workspace {
            action: "select".into(),
            value: Some(value),
            project_id: None,
        });
        run_action(&mut app, &store, &terminals, out);
        visited.push(app.model().workspace_id.clone());
    }
    let mut seen: Vec<_> = visited.clone();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 4, "All projects + 3 workspaces: {visited:?}");
    assert!(visited.contains(&None) && ids.iter().all(|i| visited.contains(&Some(i.clone()))));
    assert_eq!(
        strip(&mut app),
        first_strip,
        "switching never reorders the switcher"
    );
}

#[test]
fn clicking_a_header_tab_switches_and_the_current_one_is_highlighted() {
    use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::style::Modifier;

    let f = fixture();
    let store = store();
    let terminals = TerminalRegistry::new();
    let nav = Navigation { store: &store };
    let a = nav.open(&f.repo_a).unwrap();
    let b = nav.open(&f.repo_b).unwrap();
    assert_eq!(nav.selected().unwrap().as_deref(), Some(b.as_str()));

    let mut app = WorkbenchApp::new();
    app.set_model(model(&store, &terminals));
    let (text, buffer) = render(&mut app, 140, 30);
    let header = text.lines().next().expect("header row");
    // Row 0 of the text dump is the top border quote line of TestBackend's
    // Display; locate the label's column in the buffer itself.
    let column_of = |label: &str| -> u16 {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        row.find(label)
            .map(|byte| row[..byte].chars().count() as u16)
            .unwrap_or_else(|| panic!("{label} in header {row:?} ({header:?})"))
    };
    let (col_a, col_b) = (column_of("repo-a"), column_of("repo-b"));
    assert!(
        buffer[(col_b, 0)].modifier.contains(Modifier::REVERSED)
            && !buffer[(col_a, 0)].modifier.contains(Modifier::REVERSED),
        "the current workspace (repo-b) is the highlighted one"
    );

    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col_a,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    let action = app
        .take_mouse_action()
        .expect("a click on a tab selects it");
    let reply = run_action(&mut app, &store, &terminals, KeyOutcome::Action(action));
    assert_eq!(app.model().workspace_id.as_deref(), Some(a.as_str()));
    assert!(reply["message"].as_str().unwrap().contains("repo-a"));
    let (_, buffer) = render(&mut app, 140, 30);
    assert!(
        buffer[(col_a, 0)].modifier.contains(Modifier::REVERSED),
        "the clicked tab becomes the highlighted one"
    );

    // Clicking "All projects" clears the filter instead of failing.
    let (_, buffer) = render(&mut app, 140, 30);
    let row0: String = (0..buffer.area.width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    let col_all = row0.find("All projects").expect("All projects tab") as u16;
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col_all + 1,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    let action = app.take_mouse_action().expect("All projects is clickable");
    run_action(&mut app, &store, &terminals, KeyOutcome::Action(action));
    assert_eq!(app.model().workspace_id, None);
    assert_eq!(app.model().projects.len(), 2, "both projects visible again");
}

#[test]
fn the_open_prompt_lists_recent_workspaces_takes_pasted_paths_and_refuses_empty_input() {
    let f = fixture();
    let store = store();
    let terminals = TerminalRegistry::new();
    let nav = Navigation { store: &store };
    let a = nav.open(&f.repo_a).unwrap();
    let b = nav.open(&f.repo_b).unwrap();
    nav.select(&a).unwrap(); // MRU order is now a, b

    let mut app = WorkbenchApp::new();
    app.set_model(model(&store, &terminals));

    // ctrl+b w opens the workspace picker; recents are listed newest first.
    assert_eq!(prefix_then(&mut app, 'w'), KeyOutcome::Handled);
    let (text, _) = render(&mut app, 140, 34);
    assert!(text.contains("Open workspace"), "{text}");
    let recent_a = text
        .find("repo-a (current) — ")
        .unwrap_or_else(|| panic!("current repo-a listed with its place: {text}"));
    let recent_b = text
        .rfind("repo-b — ")
        .unwrap_or_else(|| panic!("repo-b listed with its place: {text}"));
    assert!(recent_a < recent_b, "most recently opened first");

    // Empty Enter without a pick is refused; the prompt stays open.
    assert_eq!(app.on_key(key(KeyCode::Enter)), KeyOutcome::Handled);
    let (text, _) = render(&mut app, 140, 34);
    assert!(text.contains("Open workspace"), "prompt still open: {text}");

    // ↓ ↓ picks the second recent (repo-b); Enter switches without typing.
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    let out = app.on_key(key(KeyCode::Enter));
    run_action(&mut app, &store, &terminals, out);
    assert_eq!(app.model().workspace_id.as_deref(), Some(b.as_str()));
    let (text, _) = render(&mut app, 140, 34);
    assert!(
        !text.contains("Open workspace"),
        "prompt closed after Enter"
    );

    // A pasted path lands in the prompt (newline dropped) and opens.
    prefix_then(&mut app, 'w');
    assert_eq!(
        app.on_paste(&format!("{}\n", f.repo_a.display())),
        KeyOutcome::Handled
    );
    let out = app.on_key(key(KeyCode::Enter));
    let reply = run_action(&mut app, &store, &terminals, out);
    assert_eq!(app.model().workspace_id.as_deref(), Some(a.as_str()));
    assert_eq!(count(&store, "workspaces"), 2, "same path, same workspace");
    assert!(reply["message"].as_str().unwrap().starts_with("opened"));

    // Esc cancels and Ctrl-U clears.
    prefix_shift(&mut app, 'n');
    app.on_key(key(KeyCode::Char('x')));
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(app.on_key(key(KeyCode::Enter)), KeyOutcome::Handled);
    app.on_key(key(KeyCode::Esc));
    assert_eq!(count(&store, "workspaces"), 2, "nothing was created");
}

#[test]
fn remove_acts_on_the_selected_project_and_never_fires_without_one() {
    let f = fixture();
    let store = store();
    let terminals = TerminalRegistry::new();
    let nav = Navigation { store: &store };
    let ws = nav.create("pair").unwrap();
    nav.add(&ws, serde_json::json!({"path": f.repo_a}), Path::new("/"))
        .unwrap();
    nav.add(&ws, serde_json::json!({"path": f.repo_b}), Path::new("/"))
        .unwrap();
    nav.select(&ws).unwrap();

    let mut app = WorkbenchApp::new();

    nav.select_all().unwrap();
    app.set_model(model(&store, &terminals));
    assert_eq!(
        app.on_key(key(KeyCode::Char('R'))),
        KeyOutcome::Ignored,
        "legacy direct keys are gone"
    );
    assert_eq!(nav.folders(&ws).unwrap().len(), 2, "nothing removed");

    // Workspace selected, cursor on the first row: remove drops that project
    // from the composition only (host action, not a legacy key).
    nav.select(&ws).unwrap();
    app.set_model(model(&store, &terminals));
    let project = app.model().worktrees[0].project_id.clone();
    let repos_before = count(&store, "projects");
    let out = KeyOutcome::Action(WorkbenchAction::Workspace {
        action: "remove".into(),
        value: None,
        project_id: Some(project.clone()),
    });
    let reply = run_action(&mut app, &store, &terminals, out);
    let left: Vec<String> = nav
        .folders(&ws)
        .unwrap()
        .into_iter()
        .filter_map(|r| r.project_id)
        .collect();
    assert_eq!(left.len(), 1);
    assert!(
        !left.contains(&project),
        "the selected project left the composition"
    );
    assert_eq!(
        count(&store, "projects"),
        repos_before,
        "the project itself stays"
    );
    assert!(f.repo_a.exists() && f.repo_b.exists(), "no file touched");
    assert!(reply["message"].as_str().is_some());
}

// ---------------------------------------------------------------------------
// #74 acceptance 5: broken input is reported, never swallowed or executed
// ---------------------------------------------------------------------------

#[test]
fn broken_input_is_reported_and_unavailable_entries_are_kept_visible() {
    let f = fixture();
    let store = store();
    let nav = Navigation { store: &store };
    let grants_before = count(&store, "grants");

    let missing = f.ws_dir.join("nope.code-workspace");
    let err = nav.open(&missing).expect_err("a missing workspace file");
    assert!(
        err.to_string().contains("nope.code-workspace"),
        "names the path: {err}"
    );

    let bad = f.ws_dir.join("bad.code-workspace");
    std::fs::write(&bad, "{ \"folders\": [ ").unwrap();
    assert!(nav.open(&bad).is_err());
    std::fs::write(&bad, "{ \"settings\": {} }").unwrap();
    assert!(
        nav.open(&bad)
            .expect_err("no folders array")
            .to_string()
            .contains("folders")
    );
    assert_eq!(
        nav.list().unwrap().len(),
        0,
        "a failed open leaves no half-created workspace"
    );

    let weird = f.ws_dir.join("weird.code-workspace");
    let a_file = f.root.join("just-a-file.txt");
    std::fs::write(&a_file, "x").unwrap();
    std::fs::write(
        &weird,
        format!(
            "\u{feff}{{ \"folders\": [ {{\"path\": {a:?}}}, {{\"path\": {b:?}}}, {{\"path\": {file:?}}}, \
             {{\"uri\": \"vscode-remote://ssh-remote+box/srv/app\"}}, {{\"path\": {a:?}}} ] }}",
            a = f.repo_a,
            b = f.root.join("gone"),
            file = a_file,
        ),
    )
    .unwrap();
    let id = nav.open(&weird).expect("BOM and odd entries still open");
    let rows = nav.folders(&id).unwrap();
    assert_eq!(
        rows.len(),
        5,
        "nothing is silently dropped, duplicates included: {rows:?}"
    );
    let diags: Vec<Option<&str>> = rows.iter().map(|r| r.diagnostic.as_deref()).collect();
    assert!(diags[0].is_none());
    assert!(diags[1].unwrap().contains("missing"), "{diags:?}");
    assert!(diags[2].unwrap().contains("not a directory"), "{diags:?}");
    assert!(diags[3].unwrap().contains("unsupported"), "{diags:?}");
    assert_eq!(
        rows[0].project_id, rows[4].project_id,
        "duplicate entry shares the project"
    );
    assert_eq!(
        count(&store, "projects"),
        1,
        "only the real repo became a project"
    );

    // Saving keeps every entry, including the unavailable ones.
    let out = f.ws_dir.join("weird-saved.code-workspace");
    nav.save_as(&id, &out).unwrap();
    let doc = parse_jsonc(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc["folders"].as_array().unwrap().len(), 5);
    assert_eq!(
        doc["folders"][3]["uri"],
        "vscode-remote://ssh-remote+box/srv/app"
    );

    assert_eq!(
        count(&store, "grants"),
        grants_before,
        "opening never creates or widens a grant"
    );
}

// ---------------------------------------------------------------------------
// Repo → worktree grouping stays true after the repo changes underneath us
// ---------------------------------------------------------------------------

#[test]
fn worktrees_created_or_removed_outside_viva_are_picked_up_on_select() {
    let f = fixture();
    let store = store();
    let terminals = TerminalRegistry::new();
    let nav = Navigation { store: &store };
    let id = nav.open(&f.repo_b).unwrap();
    let paths = |m: &WorkbenchModel| -> Vec<String> {
        let mut p: Vec<String> = m.worktrees.iter().map(|w| w.path.clone()).collect();
        p.sort();
        p
    };
    assert_eq!(paths(&model(&store, &terminals)).len(), 2);

    let late = add_linked(
        &f.repo_b,
        &f.root.join("work-two").join("repo-b-late"),
        "late",
    );
    nav.select(&id).unwrap();
    let m = model(&store, &terminals);
    assert!(
        paths(&m).contains(&late.to_str().unwrap().to_string()),
        "new worktree shows up"
    );

    git(
        &f.repo_b,
        &["worktree", "remove", "--force", late.to_str().unwrap()],
    );
    nav.select(&id).unwrap();
    let m = model(&store, &terminals);
    assert!(
        !paths(&m).contains(&late.to_str().unwrap().to_string()),
        "removed worktree disappears"
    );
    assert_eq!(m.worktrees.len(), 2);
}

#[test]
fn task_worktrees_keep_their_repo_node_when_a_workspace_is_selected() {
    let f = fixture();
    git(
        &f.repo_a,
        &["remote", "add", "origin", f.repo_a.to_str().unwrap()],
    );
    git(&f.repo_a, &["fetch", "origin"]);
    let store = store();
    let terminals = TerminalRegistry::new();
    let wb = WorkbenchStore {
        store: &store,
        terminals: &terminals,
        protected: ProtectedRefs::new(vec!["main".into()]),
    };
    let task = wb.create_task("ship S1", None, None).unwrap();
    let base = f.root.join("task-worktrees");
    std::fs::create_dir_all(&base).unwrap();
    let record = wb
        .create_task_worktree(&f.repo_a, &base, &task, "agent/s1-visible")
        .expect("task worktree");

    let nav = Navigation { store: &store };
    nav.open(&f.repo_a).unwrap();
    let m = model(&store, &terminals);
    let row = m
        .worktrees
        .iter()
        .find(|w| w.worktree_id == record.worktree_id.to_string())
        .expect("the task's worktree stays visible inside the workspace that holds its repo");
    assert!(
        m.projects.iter().any(|p| p.project_id == row.project_id),
        "and hangs under the repo node (project_id {:?})",
        row.project_id
    );
    assert_eq!(row.task_id.as_deref(), Some(task.to_string().as_str()));
}

// ---------------------------------------------------------------------------
// Owner-only: members never compose workspaces; the host records the refusal
// ---------------------------------------------------------------------------

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

#[test]
fn only_the_owner_composes_workspaces_over_the_real_host_socket() {
    use viva::authority::{AuthorityEngine, GrantMode};
    use viva::members::{MemberBinding, MemberRegistry};
    use viva::server::{self, Host, RequestKind};

    let f = fixture();
    let home = f.root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (member_id, grant_id) = {
        let store = Store::open(
            &viva::foundation::paths::database_path(&home),
            server::migrations(),
        )
        .unwrap();
        let members = MemberRegistry::new(&store);
        let member = members.register("Samuel").unwrap();
        members
            .set_binding(&MemberBinding {
                member_id: member.member_id.clone(),
                role: "developer".into(),
                model_binding: "glm-5.3-flash".into(),
                tools: vec!["pi-cli".into()],
                updated_at: viva::foundation::ids::utc_now(),
            })
            .unwrap();
        let grant = AuthorityEngine::new(&store)
            .issue_root_grant(
                Some(member.member_id.clone()),
                None,
                vec!["terminal_control".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .unwrap();
        (member.member_id.to_string(), grant.grant_id.to_string())
    };
    let host_home = home.clone();
    let host = std::thread::spawn(move || {
        let host = Host::open(&host_home).expect("host");
        host.serve().expect("serve");
    });
    wait_until("socket", Duration::from_secs(10), || {
        std::os::unix::net::UnixStream::connect(home.join(server::SOCKET_NAME)).is_ok()
    });

    let request = |member: Option<&str>, grant: Option<&str>, kind: RequestKind| {
        let mut r = server::new_request(kind);
        r.member = member.map(str::to_string);
        r.grant = grant.map(str::to_string);
        server::send_request(&home, r).expect("socket answers")
    };
    let open = |p: &Path| RequestKind::Workspace {
        action: "open".into(),
        value: Some(p.to_str().unwrap().to_string()),
        project_id: None,
    };

    // Owner (OS-authenticated, no member attribution): works.
    let ok = request(None, None, open(&f.repo_a));
    assert!(ok.ok, "{:?}", ok.error);
    let result: Value = ok.result.unwrap();
    assert!(result["workspace_id"].is_string());
    assert_eq!(
        result["folders"].as_array().map(Vec::len),
        Some(1),
        "the response lists what was opened: {result}"
    );

    // A member — even with a live grant — cannot touch the composition.
    let denied = request(Some(&member_id), Some(&grant_id), open(&f.repo_b));
    assert!(!denied.ok);
    assert!(
        denied.error.unwrap().contains("owner"),
        "refusal names the owner boundary"
    );
    let nogrant = request(Some(&member_id), None, open(&f.repo_b));
    assert!(!nogrant.ok);

    let list = request(
        None,
        None,
        RequestKind::Workspace {
            action: "list".into(),
            value: None,
            project_id: None,
        },
    );
    let workspaces = list.result.unwrap()["workspaces"].as_array().unwrap().len();
    assert_eq!(workspaces, 1, "the refused opens changed nothing");

    let _ = server::send_request(
        &home,
        server::new_request(RequestKind::Shutdown { close_policy: None }),
    );
    host.join().ok();
}

// ---------------------------------------------------------------------------
// The CLI entry points: `viva workspace …` and `viva open …`
// ---------------------------------------------------------------------------

#[test]
fn the_cli_composes_workspaces_over_the_resident_server_and_open_gates_on_a_tty() {
    use std::process::Stdio;
    use viva::server::{Client, RequestKind};

    let f = fixture();
    let home = TempDir::new().expect("home");
    let viva = |args: &[&str], cwd: &Path| {
        Command::new(env!("CARGO_BIN_EXE_viva"))
            .args(args)
            .current_dir(cwd)
            .env("VIVA_HOME", home.path())
            .stdin(Stdio::null())
            .output()
            .expect("viva runs")
    };

    // `viva open` needs a terminal for the workbench it attaches. It must
    // refuse BEFORE starting a server or opening anything.
    let out = viva(&["open", f.repo_a.to_str().unwrap()], &f.root);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("interactive terminal"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        Client::connect(home.path()).is_err(),
        "a call that must fail left no resident server behind"
    );

    // The resident server, as `viva server` runs it.
    /// A failing assertion must not leave a resident server running.
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut server = Command::new(env!("CARGO_BIN_EXE_viva"))
        .arg("server")
        .env("VIVA_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(KillOnDrop)
        .expect("server starts");
    wait_until("the resident server", Duration::from_secs(20), || {
        Client::connect(home.path()).is_ok()
    });

    let json = |out: &std::process::Output| -> Value {
        assert!(
            out.status.success(),
            "viva failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("json on stdout")
    };

    // A real workspace file, given relative to the caller's directory.
    let file = write_commented_workspace(&f);
    let reply = json(&viva(
        &["workspace", "open", "demo.code-workspace"],
        &f.ws_dir,
    ));
    assert!(
        reply["message"]
            .as_str()
            .unwrap()
            .starts_with("opened demo"),
        "{reply}"
    );
    let folders = reply["folders"].as_array().expect("folders");
    assert_eq!(folders.len(), 2, "{reply}");
    assert_eq!(folders[0]["name"], "Alpha");
    assert_eq!(folders[1]["name"], "repo-b");

    // Relative paths resolve on the client side, and the same folder twice
    // is reported, not duplicated.
    let reply = json(&viva(
        &["workspace", "add", "../work-one/repo-a"],
        &f.ws_dir,
    ));
    assert!(
        reply["message"].as_str().unwrap().contains("already in"),
        "{reply}"
    );
    assert_eq!(reply["folders"].as_array().unwrap().len(), 2);

    // Save As writes a file VS Code can read, relative to the new place.
    let saved = f.root.join("elsewhere");
    std::fs::create_dir_all(&saved).unwrap();
    let reply = json(&viva(&["workspace", "save", "copy"], &saved));
    assert!(
        reply["message"].as_str().unwrap().starts_with("saved"),
        "{reply}"
    );
    let copy = saved.join("copy.code-workspace");
    let doc = parse_jsonc(&std::fs::read_to_string(&copy).expect("saved file")).unwrap();
    assert_eq!(doc["folders"].as_array().unwrap().len(), 2);
    assert_eq!(doc["settings"]["editor.tabSize"], 2, "other fields survive");

    // Selecting "all" and listing both work from the CLI.
    let reply = json(&viva(&["workspace", "select", "all"], &f.root));
    assert!(reply["workspace_id"].is_null(), "{reply}");
    let reply = json(&viva(&["workspace", "list"], &f.root));
    assert_eq!(reply["workspaces"].as_array().unwrap().len(), 1, "{reply}");

    // Garbage fails loudly with a message, never silently.
    let out = viva(&["workspace", "open", "does/not/exist"], &f.root);
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).trim().is_empty());
    assert!(
        file.exists(),
        "the source file was not touched by any of it"
    );

    let mut client = Client::connect(home.path()).expect("client");
    let _ = client.call(RequestKind::Shutdown { close_policy: None });
    let _ = server.0.wait();
}
