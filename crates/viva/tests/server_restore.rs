//! Server-process restart (TFHS-72): a killed server restores the scene
//! tree, pane directories, and focus with new processes. It does not
//! resurrect the command that was running, and it does not re-run a
//! finished execution. Live handoff is a different path and is not covered
//! here.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use viva::foundation::paths::database_path;
use viva::foundation::store::Store;
use viva::server::{self, RequestKind, WorkbenchTreeOp};

static UNIQUE: AtomicU64 = AtomicU64::new(0);

fn unique(label: &str) -> String {
    let n = UNIQUE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{label}-{}-{nanos}-{n}", std::process::id())
}

fn socket_ready(home: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(home.join(server::SOCKET_NAME)).is_ok()
}

struct HostProc {
    child: Child,
    alive: bool,
    log_path: PathBuf,
}

impl HostProc {
    fn start(home: &Path) -> Self {
        let log_path = home.join("server-stderr.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("server log");
        let mut child = Command::new(env!("CARGO_BIN_EXE_viva"))
            .args(["start"])
            .env("VIVA_HOME", home)
            .env("SHELL", "/bin/sh")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("spawn viva start");
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if socket_ready(home) {
                return Self {
                    child,
                    alive: true,
                    log_path,
                };
            }
            if let Some(status) = child.try_wait().expect("try_wait") {
                let err = std::fs::read_to_string(&log_path).unwrap_or_default();
                panic!("viva start exited {status}: {err}");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let err = std::fs::read_to_string(&log_path).unwrap_or_default();
        let _ = child.kill();
        let _ = child.wait();
        panic!("timed out waiting for the server socket: {err}");
    }

    fn kill_hard(&mut self) {
        if !self.alive {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.alive = false;
        std::thread::sleep(Duration::from_millis(100));
    }
}

impl Drop for HostProc {
    fn drop(&mut self) {
        if self.alive {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.alive = false;
        }
        if std::thread::panicking() {
            if let Ok(text) = std::fs::read_to_string(&self.log_path) {
                eprintln!("server stderr:\n{text}");
            }
        }
    }
}

struct Scratch {
    root: PathBuf,
    pids: Vec<u32>,
    files: Vec<PathBuf>,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(unique("viva72"));
        std::fs::create_dir_all(&root).expect("scratch");
        Self {
            root,
            pids: Vec::new(),
            files: Vec::new(),
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::create_dir_all(&path).expect("pane dir");
        std::fs::canonicalize(&path).expect("canonical pane dir")
    }

    fn track(&mut self, pid: u32) {
        self.pids.push(pid);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for pid in &self.pids {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        for file in &self.files {
            let _ = std::fs::remove_file(file);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn send(home: &Path, kind: RequestKind) -> Value {
    let response = server::send_request(home, server::new_request(kind))
        .unwrap_or_else(|err| panic!("request failed: {err}"));
    if !response.ok {
        panic!(
            "request rejected: {}",
            response.error.unwrap_or_else(|| "rejected".into())
        );
    }
    response.result.unwrap_or(Value::Null)
}

fn open_store(home: &Path) -> Store {
    Store::open(&database_path(home), server::migrations()).expect("store")
}

fn count_executions(home: &Path) -> i64 {
    open_store(home)
        .row_count("executions")
        .expect("executions")
}

fn process_command(pid: u32) -> String {
    let Ok(output) = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
    else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn wait_until(what: &str, timeout: Duration, mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe() {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("timed out waiting for {what}");
}

fn latest_spawn_pid(home: &Path, terminal_id: &str) -> u32 {
    let store = open_store(home);
    let payload: String = store
        .connection()
        .query_row(
            "SELECT payload FROM terminal_events
             WHERE terminal_id = ?1 AND event_kind = 'spawned'
             ORDER BY seq DESC LIMIT 1",
            [terminal_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|err| panic!("spawned event for {terminal_id}: {err}"));
    let json: Value = serde_json::from_str(&payload).expect("spawn payload");
    json["pid"].as_u64().expect("pid") as u32
}

fn recovery_kinds(home: &Path) -> Vec<String> {
    let store = open_store(home);
    let mut stmt = store
        .connection()
        .prepare("SELECT kind FROM recovery_events ORDER BY seq")
        .expect("prepare");
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query");
    rows.collect::<Result<Vec<_>, _>>().expect("kinds")
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

fn snapshot_of(home: &Path, terminal_id: &str) -> Value {
    send(
        home,
        RequestKind::TerminalSnapshot {
            terminal_id: terminal_id.to_string(),
        },
    )
}

fn place_terminal(home: &Path, scene: &str, tab_id: &str, terminal_id: &str) {
    send(
        home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::Split {
                scene_id: Some(scene.into()),
                tab_id: Some(tab_id.into()),
                pane_id: None,
                direction: "right".into(),
                terminal_id: Some(terminal_id.into()),
            },
        },
    );
}

fn create_marked_terminal(home: &Path, cwd: &Path, marker: &str) -> (String, u32) {
    let script = format!("printf '{marker}\\n'; exec sleep 300");
    let created = send(
        home,
        RequestKind::TerminalCreate {
            argv: vec!["/bin/sh".into(), "-c".into(), script],
            cwd: cwd.display().to_string(),
            env: vec![],
            cols: 120,
            rows: 24,
            purpose: "user shell".into(),
            worktree_id: None,
            owner: "user_shell".into(),
        },
    );
    let id = created["terminal_id"]
        .as_str()
        .expect("terminal id")
        .to_string();
    let pid = created["pid"].as_u64().expect("pid") as u32;
    (id, pid)
}

fn write_resume_script(scratch: &mut Scratch) -> PathBuf {
    let path = scratch
        .root
        .join(format!("{}.sh", unique("resume-fixture")));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o755)
        .open(&path)
        .expect("resume script");
    file.write_all(b"#!/bin/sh\nprintf 'RESUME_OK\\n'\nexec cat\n")
        .expect("write script");
    drop(file);
    let path = std::fs::canonicalize(&path).expect("script path");
    let text = path.to_string_lossy();
    assert!(
        !text.contains([
            ' ', '\n', '\r', '|', ';', '&', '<', '>', '$', '`', '\\', '"', '\''
        ]),
        "resume fixture path must be one token: {text}"
    );
    scratch.files.push(path.clone());
    path
}

fn insert_resume(home: &Path, terminal_id: &str, session: &str, command: &str) {
    let store = open_store(home);
    store
        .connection()
        .execute(
            "INSERT INTO agent_native_sessions(
                terminal_id, agent, native_session, resume_command, updated_at
             ) VALUES (?1, 'fixture', ?2, ?3, '2026-01-01T00:00:00Z')",
            rusqlite::params![terminal_id, session, command],
        )
        .expect("insert resume row");
}

fn terminal_row<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["terminals"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["terminal_id"] == id))
}

fn pwd_marker(home: &Path, terminal_id: &str) -> String {
    let command = b"printf '%s\\n' \"CWD:$(pwd -P):END\"\n";
    send(
        home,
        RequestKind::TerminalInput {
            terminal_id: terminal_id.to_string(),
            bytes_hex: server::hex_encode(command),
        },
    );
    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        seen = screen_text(&snapshot_of(home, terminal_id)).replace(['\n', '\r'], "");
        if let Some(start) = seen.rfind("CWD:") {
            if let Some(end) = seen[start + 4..].find(":END") {
                return seen[start + 4..start + 4 + end].to_string();
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("pwd marker missing on {terminal_id}: {seen}");
}

fn assert_same_directory(printed: &str, expected: &Path) {
    let printed_path = std::fs::canonicalize(printed).unwrap_or_else(|err| {
        panic!(
            "printed cwd {printed} is not a directory ({err}); expected {}",
            expected.display()
        )
    });
    let expected = std::fs::canonicalize(expected).expect("expected dir");
    assert_eq!(
        printed_path, expected,
        "restored shell is not in the saved directory"
    );
}

#[test]
fn killed_server_restores_layout_cwd_and_resume_without_old_commands() {
    let dir = tempfile::tempdir().expect("home dir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let mut scratch = Scratch::new();
    let shell_dir = scratch.dir("shell");
    let resume_dir = scratch.dir("resume");
    let fallback_dir = scratch.dir("fallback");
    let gone_dir = scratch.dir("gone");
    let script = write_resume_script(&mut scratch);

    let mut host = HostProc::start(&home);
    send(
        &home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreateScene {
                id: Some("alpha".into()),
                name: "Alpha".into(),
            },
        },
    );
    let created_tab = send(
        &home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreateTab {
                scene_id: Some("alpha".into()),
                name: "main".into(),
            },
        },
    );
    let tab_id = created_tab["tree"]["scenes"]["alpha"]["tabs"][0]["id"]
        .as_str()
        .expect("tab id")
        .to_string();

    let (shell_id, shell_pid) = create_marked_terminal(&home, &shell_dir, "ORIG_shell");
    let (resume_id, resume_pid) = create_marked_terminal(&home, &resume_dir, "ORIG_resume");
    let (fallback_id, fallback_pid) = create_marked_terminal(&home, &fallback_dir, "ORIG_fallback");
    let (gone_id, gone_pid) = create_marked_terminal(&home, &gone_dir, "ORIG_gone");
    for pid in [shell_pid, resume_pid, fallback_pid, gone_pid] {
        scratch.track(pid);
        wait_until(
            &format!("original command {pid} became sleep"),
            Duration::from_secs(3),
            || process_command(pid).contains("sleep"),
        );
    }
    for id in [&shell_id, &resume_id, &fallback_id, &gone_id] {
        place_terminal(&home, "alpha", &tab_id, id);
    }
    send(
        &home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::ClosePane {
                scene_id: Some("alpha".into()),
                tab_id: Some(tab_id.clone()),
                pane_id: fallback_id.clone(),
            },
        },
    );
    send(
        &home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::FocusPane {
                scene_id: Some("alpha".into()),
                tab_id: Some(tab_id.clone()),
                pane_id: shell_id.clone(),
            },
        },
    );
    let before = send(
        &home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::List,
        },
    );
    assert_eq!(before["tree"]["selected"], "alpha");
    assert_eq!(before["tree"]["scenes"]["alpha"]["active"], tab_id);
    assert_eq!(
        before["tree"]["scenes"]["alpha"]["tabs"][0]["focus"]["Terminal"],
        shell_id
    );
    assert!(
        !home.join("pane-history.json").exists(),
        "screen history is off and must not be written while the server runs"
    );
    let executions = count_executions(&home);

    host.kill_hard();
    insert_resume(
        &home,
        &resume_id,
        "session-resume",
        script.to_str().expect("script utf-8"),
    );
    insert_resume(
        &home,
        &fallback_id,
        "session-missing",
        "/no/such/viva-resume-bin",
    );
    std::fs::remove_dir_all(&gone_dir).expect("remove saved directory");

    let up = Instant::now();
    let mut host = HostProc::start(&home);
    let after = send(
        &home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::List,
        },
    );
    assert_eq!(
        after["tree"], before["tree"],
        "scene, tab, pane ids, closed panes, and focus survive the restart"
    );
    let errors = after["pane_errors"][&gone_id]
        .as_str()
        .unwrap_or_else(|| panic!("missing pane error: {after}"))
        .to_string();
    assert!(
        errors.contains("directory does not exist") && errors.contains("pane kept"),
        "gone pane should stay with an error, got {errors}"
    );
    assert!(
        !errors.contains("home directory"),
        "a missing directory must not be described as a move home: {errors}"
    );

    let list = send(&home, RequestKind::TerminalList);
    assert!(
        terminal_row(&list, &gone_id).is_none(),
        "the pane with a missing directory has no process: {list}"
    );
    let gone_view = snapshot_of(&home, &gone_id);
    let gone_text = screen_text(&gone_view);
    assert!(
        gone_text.contains("directory does not exist") && gone_text.contains("pane kept"),
        "error pane draws the kept-pane message: {gone_text}"
    );
    assert!(
        !gone_text.contains("ORIG_gone"),
        "the missing pane must not show the dead process screen"
    );

    for (id, owner, purpose) in [
        (&shell_id, "user_shell", "shell"),
        (&fallback_id, "user_shell", "shell"),
        (&resume_id, "agent_cli", "agent resume"),
    ] {
        let row = terminal_row(&list, id).unwrap_or_else(|| panic!("{id} missing from {list}"));
        assert_eq!(row["owner"], owner, "{id}");
        assert_eq!(row["purpose"], purpose, "{id}");
        let new_pid = latest_spawn_pid(&home, id);
        scratch.track(new_pid);
        assert_ne!(new_pid, 0);
    }

    assert!(
        !screen_text(&snapshot_of(&home, &shell_id)).contains("ORIG_shell"),
        "the new shell must not adopt the previous pane screen"
    );
    assert!(
        !screen_text(&snapshot_of(&home, &fallback_id)).contains("ORIG_fallback"),
        "a failed resume reference must not adopt the previous pane screen"
    );

    let mut resume_text = String::new();
    wait_until("resume fixture output", Duration::from_secs(5), || {
        resume_text = screen_text(&snapshot_of(&home, &resume_id));
        resume_text.contains("RESUME_OK")
    });
    assert!(
        !resume_text.contains("ORIG_resume"),
        "resume output replaced the old screen: {resume_text}"
    );

    let shell_new = latest_spawn_pid(&home, &shell_id);
    let resume_new = latest_spawn_pid(&home, &resume_id);
    let fallback_new = latest_spawn_pid(&home, &fallback_id);
    assert_ne!(shell_new, shell_pid);
    assert_ne!(resume_new, resume_pid);
    assert_ne!(fallback_new, fallback_pid);
    // The dead server's PTY masters close with it, so the kernel may SIGHUP
    // the old foreground groups. Restore does not signal those pids and does
    // not start them again: the new pid is a different process.
    let shell_cmd = process_command(shell_new);
    assert!(
        shell_cmd.contains("sh") && !shell_cmd.contains("sleep"),
        "the restored pane starts a new shell, not the old sleep: {shell_cmd}"
    );
    let resume_cmd = process_command(resume_new);
    assert!(
        resume_cmd.contains("resume-fixture") || resume_cmd.contains("cat"),
        "the recoverable pane must run its resume command, got {resume_cmd}"
    );
    assert!(
        !resume_cmd.contains("sleep"),
        "resume must not be the old sleep: {resume_cmd}"
    );

    assert_same_directory(&pwd_marker(&home, &shell_id), &shell_dir);
    assert_same_directory(&pwd_marker(&home, &fallback_id), &fallback_dir);
    assert_eq!(
        count_executions(&home),
        executions,
        "restore must not dispatch or re-run an execution"
    );
    let kinds = recovery_kinds(&home);
    for kind in [
        "workspace_restored",
        "pane_restore_resume",
        "pane_restore_fallback",
        "pane_restore_kept",
    ] {
        assert!(
            kinds.iter().any(|item| item == kind),
            "missing recovery kind {kind} in {kinds:?}"
        );
    }

    let rest = Duration::from_millis(2200).saturating_sub(up.elapsed());
    std::thread::sleep(rest);
    assert!(
        !home.join("pane-history.json").exists(),
        "screen history stays off across the restarted server"
    );

    send(&home, RequestKind::Shutdown { close_policy: None });
    let _ = host.child.wait();
    host.alive = false;
}

#[test]
fn resume_agents_can_be_turned_off() {
    let dir = tempfile::tempdir().expect("home dir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join("config.toml"),
        "resume_agents_on_restore = false\n",
    )
    .expect("config");
    let mut scratch = Scratch::new();
    let cwd = scratch.dir("off");
    let script = write_resume_script(&mut scratch);

    let mut host = HostProc::start(&home);
    send(
        &home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreateScene {
                id: Some("alpha".into()),
                name: "Alpha".into(),
            },
        },
    );
    let created_tab = send(
        &home,
        RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreateTab {
                scene_id: Some("alpha".into()),
                name: "main".into(),
            },
        },
    );
    let tab_id = created_tab["tree"]["scenes"]["alpha"]["tabs"][0]["id"]
        .as_str()
        .expect("tab id")
        .to_string();
    let (id, pid) = create_marked_terminal(&home, &cwd, "ORIG_off");
    scratch.track(pid);
    place_terminal(&home, "alpha", &tab_id, &id);
    host.kill_hard();
    insert_resume(&home, &id, "session-off", script.to_str().expect("utf-8"));

    let mut host = HostProc::start(&home);
    let list = send(&home, RequestKind::TerminalList);
    let row = terminal_row(&list, &id).unwrap_or_else(|| panic!("pane missing: {list}"));
    assert_eq!(row["owner"], "user_shell");
    assert_eq!(row["purpose"], "shell");
    let new_pid = latest_spawn_pid(&home, &id);
    scratch.track(new_pid);
    let command = process_command(new_pid);
    assert!(
        !command.contains("resume-fixture") && !command.contains("cat"),
        "a disabled resume flag must start a shell, got {command}"
    );
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        assert!(
            !screen_text(&snapshot_of(&home, &id)).contains("RESUME_OK"),
            "the resume command ran while resume_agents_on_restore was off"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    send(&home, RequestKind::Shutdown { close_policy: None });
    let _ = host.child.wait();
    host.alive = false;
}

#[test]
fn a_corrupt_layout_is_backed_up_and_left_in_place() {
    let dir = tempfile::tempdir().expect("home dir");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let garbage = "{not-json";
    {
        let store = open_store(&home);
        store
            .connection()
            .execute(
                "INSERT INTO settings(key, value) VALUES ('workbench_layout', ?1)",
                [garbage],
            )
            .expect("insert corrupt layout");
    }

    let up = Instant::now();
    let mut host = HostProc::start(&home);
    let current: String = open_store(&home)
        .connection()
        .query_row(
            "SELECT value FROM settings WHERE key = 'workbench_layout'",
            [],
            |row| row.get(0),
        )
        .expect("layout row");
    assert_eq!(
        current, garbage,
        "a corrupt layout must not be replaced or cleared"
    );
    let backups: Vec<PathBuf> = std::fs::read_dir(&home)
        .expect("home")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("workbench_layout.corrupt-"))
        })
        .collect();
    assert_eq!(
        backups.len(),
        1,
        "one backup of the original bytes, found {backups:?}"
    );
    assert_eq!(
        std::fs::read(&backups[0]).expect("backup"),
        garbage.as_bytes(),
        "the backup is the original bytes"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&backups[0])
            .expect("backup meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the backup is owner-only");
    }
    let kinds = recovery_kinds(&home);
    assert!(
        kinds.iter().any(|kind| kind == "snapshot_corrupt"),
        "corrupt layout is logged: {kinds:?}"
    );
    let rest = Duration::from_millis(2200).saturating_sub(up.elapsed());
    std::thread::sleep(rest);
    assert!(
        !home.join("pane-history.json").exists(),
        "history stays off when the layout cannot be restored"
    );
    send(&home, RequestKind::Shutdown { close_policy: None });
    let _ = host.child.wait();
    host.alive = false;
}
