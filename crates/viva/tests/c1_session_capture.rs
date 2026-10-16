//! C1 session capture (issue #115): Claude Code, Codex, and Pi turns land
//! in experience, attributed from an execution when Viva started the session.

use std::path::{Path, PathBuf};
use std::process::Command;

use viva::experience::{ExperienceService, HarnessRoots};
use viva::foundation::ids::{ExecutionId, MemberId, TaskId};
use viva::foundation::paths::database_path;
use viva::foundation::records::{AttributionSnapshot, ExecutionRecord, insert_execution};
use viva::foundation::store::{
    DOMAIN_EXPERIENCE, DOMAIN_FOUNDATION, FOUNDATION_V1_SQL, MigrationRegistry, Store,
};

fn store() -> Store {
    let registry = MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(
            DOMAIN_EXPERIENCE,
            1,
            "experience v1",
            viva::experience::EXPERIENCE_V1_SQL,
        )
        .freeze()
        .expect("registry");
    Store::open_in_memory(&registry).expect("store")
}

fn execution(store: &Store, member: &MemberId) -> ExecutionId {
    let record = ExecutionRecord::start(
        TaskId::new(),
        AttributionSnapshot::capture(member.clone(), "role", "model", "harness"),
    );
    insert_execution(store, &record).expect("execution");
    record.execution_id
}

struct Fixture {
    _dir: tempfile::TempDir,
    roots: HarnessRoots,
    home: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp");
    let home = dir.path().join("viva-home");
    std::fs::create_dir_all(&home).expect("home");
    let roots = HarnessRoots {
        claude_projects: dir.path().join("claude"),
        codex_sessions: dir.path().join("codex"),
        pi_sessions: dir.path().join("pi"),
    };
    for root in [
        &roots.claude_projects,
        &roots.codex_sessions,
        &roots.pi_sessions,
    ] {
        std::fs::create_dir_all(root).expect("root");
    }
    Fixture {
        _dir: dir,
        roots,
        home,
    }
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    std::fs::write(path, body).expect("write");
}

fn scan<'a>(store: &'a Store, fixture: &'a Fixture, secrets: &[String]) -> ExperienceService<'a> {
    let service = ExperienceService::new(store, &fixture.home);
    service.scan(&fixture.roots, secrets).expect("scan");
    service
}

fn counts(store: &Store) -> (i64, i64) {
    let imports: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM experience_imports", [], |row| {
            row.get(0)
        })
        .unwrap();
    let turns: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM experience_turns", [], |row| {
            row.get(0)
        })
        .unwrap();
    (imports, turns)
}

const CLAUDE_ID: &str = "11111111-1111-4111-8111-111111111111";
const CODEX_ID: &str = "22222222-2222-4222-8222-222222222222";
const CODEX_CHILD: &str = "33333333-3333-4333-8333-333333333333";
const PI_ID: &str = "pi-session-1";
const PI_CHILD: &str = "pi-session-2";

fn claude_body(extra: &str) -> String {
    format!(
        r#"{{"type":"queue-operation","operation":"enqueue","sessionId":"{CLAUDE_ID}"}}
{{"type":"user","isMeta":true,"sessionId":"{CLAUDE_ID}","message":{{"role":"user","content":"hook noise"}}}}
{{"type":"user","sessionId":"{CLAUDE_ID}","message":{{"role":"user","content":[{{"type":"text","text":"Review the archive plan"}}]}}}}
{{"type":"assistant","sessionId":"{CLAUDE_ID}","message":{{"role":"assistant","content":[{{"type":"text","text":"Keep each member's sessions isolated."}}]}}}}
{extra}"#
    )
}

fn codex_body(id: &str, parent: Option<&str>, turns: &[(&str, &str)]) -> String {
    let parent_json = parent
        .map(|id| format!("\"{id}\""))
        .unwrap_or_else(|| "null".into());
    let mut lines = vec![format!(
        r#"{{"type":"session_meta","payload":{{"id":"{id}","session_id":"{id}","parent_thread_id":{parent_json}}}}}"#
    )];
    for (role, text) in turns {
        let kind = if *role == "user" {
            "input_text"
        } else {
            "output_text"
        };
        lines.push(format!(
            r#"{{"type":"response_item","payload":{{"type":"message","role":"{role}","content":[{{"type":"{kind}","text":"{text}"}}]}}}}"#
        ));
    }
    lines.push(
        r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"system instructions are not a turn"}]}}"#
            .into(),
    );
    lines.join("\n")
}

fn pi_body(id: &str, parent: Option<&str>, user: &str, assistant: &str) -> String {
    let parent_json = parent
        .map(|path| format!(",\"parentSession\":{path}"))
        .unwrap_or_default();
    format!(
        r#"{{"type":"session","version":3,"id":"{id}","cwd":"/tmp/work"{parent_json}}}
{{"type":"message","id":"m1","parentId":null,"message":{{"role":"user","content":[{{"type":"text","text":"{user}"}}]}}}}
{{"type":"message","id":"m2","parentId":"m1","message":{{"role":"assistant","content":"{assistant}"}}}}
{{"type":"model_change","id":"mc","parentId":"m2","provider":"local","modelId":"test"}}
"#
    )
}

#[test]
fn viva_started_sessions_are_listed_for_that_member_only() {
    let store = store();
    let fixture = fixture();
    let member = MemberId::new();
    let other = MemberId::new();
    let claude_exec = execution(&store, &member);
    let codex_exec = execution(&store, &member);
    let pi_exec = execution(&store, &member);
    let service = ExperienceService::new(&store, &fixture.home);
    service
        .link_execution(&claude_exec, "claude-code", CLAUDE_ID)
        .unwrap();
    service
        .link_execution(&codex_exec, "codex", CODEX_ID)
        .unwrap();
    service.link_execution(&pi_exec, "pi", PI_ID).unwrap();

    write(
        &fixture
            .roots
            .claude_projects
            .join("proj")
            .join(format!("{CLAUDE_ID}.jsonl")),
        &claude_body(""),
    );
    write(
        &fixture.roots.codex_sessions.join("rollout.jsonl"),
        &codex_body(
            CODEX_ID,
            None,
            &[
                ("user", "Summarize the open tasks"),
                ("assistant", "There are two open tasks."),
            ],
        ),
    );
    write(
        &fixture
            .roots
            .pi_sessions
            .join("cwd")
            .join(format!("2026_{PI_ID}.jsonl")),
        &pi_body(
            PI_ID,
            None,
            "Draft the recovery note",
            "Recovery keeps the original session id.",
        ),
    );

    let pi_sentence = "Draft the recovery note";
    let execution_blob: String = store
        .connection()
        .query_row(
            "SELECT attribution FROM executions WHERE execution_id = ?1",
            [pi_exec.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !execution_blob.contains(pi_sentence),
        "an execution record has no transcript; Pi turns have to come from the session file"
    );

    service.scan(&fixture.roots, &[]).unwrap();
    let listed = service.list(&member).unwrap();
    assert_eq!(listed.len(), 3, "one session from each harness");
    assert!(
        listed
            .iter()
            .any(|row| row.harness == "claude-code" && row.session_id == CLAUDE_ID)
    );
    assert!(
        listed
            .iter()
            .any(|row| row.harness == "codex" && row.session_id == CODEX_ID)
    );
    assert!(listed.iter().any(|row| row.harness == "pi" && row.execution_id.as_deref() == Some(pi_exec.as_str())));
    assert!(service.list(&other).unwrap().is_empty());

    let claude = service.show(&member, "claude-code", CLAUDE_ID).unwrap();
    assert_eq!(claude.turns.len(), 2);
    assert_eq!(claude.turns[0].content, "Review the archive plan");
    assert!(claude.session.source_path.contains(CLAUDE_ID));
    assert_eq!(claude.session.turn_start, Some(0));
    assert_eq!(claude.session.turn_end, Some(1));
    let denied = service.show(&other, "claude-code", CLAUDE_ID);
    assert!(denied.is_err(), "another member cannot read the turns");

    let pi = service.show(&member, "pi", PI_ID).unwrap();
    assert_eq!(pi.turns[0].content, pi_sentence);
    let still: String = store
        .connection()
        .query_row(
            "SELECT attribution FROM executions WHERE execution_id = ?1",
            [pi_exec.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!still.contains(pi_sentence));

    let memory_tables: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'memory_candidates'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(memory_tables, 0, "capture does not write knowledge");
}

#[test]
fn sessions_outside_viva_stay_unassigned_until_haisu_assigns_one() {
    let store = store();
    let fixture = fixture();
    let member = MemberId::new();
    let other = MemberId::new();
    write(
        &fixture
            .roots
            .claude_projects
            .join(format!("{CLAUDE_ID}.jsonl")),
        &claude_body(""),
    );
    let service = scan(&store, &fixture, &[]);
    assert!(service.list(&member).unwrap().is_empty());
    assert!(service.list(&other).unwrap().is_empty());
    let pending = service.unassigned().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].member_id.is_none());
    assert!(pending[0].execution_id.is_none());

    let pending_archive = pending[0].archive_path.clone();
    assert!(pending_archive.contains("_unassigned"));
    service.assign(&member, "claude", CLAUDE_ID, &[]).unwrap();
    let listed = service.list(&member).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, CLAUDE_ID);
    assert!(listed[0].archive_path.contains(member.as_str()));
    assert!(service.list(&other).unwrap().is_empty());
    assert!(service.unassigned().unwrap().is_empty());
    let shown = service.show(&member, "claude-code", CLAUDE_ID).unwrap();
    assert_eq!(shown.turns[0].role, "user");
    let refused = service.assign(&other, "claude-code", CLAUDE_ID, &[]);
    assert!(refused.is_err(), "a second member cannot take the session");
}

#[test]
fn the_same_range_imports_once_and_a_fork_keeps_only_new_turns() {
    let store = store();
    let fixture = fixture();
    let member = MemberId::new();
    let claude_exec = execution(&store, &member);
    let codex_exec = execution(&store, &member);
    let child_exec = execution(&store, &member);
    let service = ExperienceService::new(&store, &fixture.home);
    service
        .link_execution(&claude_exec, "claude-code", CLAUDE_ID)
        .unwrap();
    service
        .link_execution(&codex_exec, "codex", CODEX_ID)
        .unwrap();
    service
        .link_execution(&child_exec, "codex", CODEX_CHILD)
        .unwrap();

    let claude_path = fixture
        .roots
        .claude_projects
        .join(format!("{CLAUDE_ID}.jsonl"));
    write(&claude_path, &claude_body(""));
    let parent_path = fixture.roots.codex_sessions.join("parent.jsonl");
    write(
        &parent_path,
        &codex_body(
            CODEX_ID,
            None,
            &[
                ("user", "Summarize the open tasks"),
                ("assistant", "There are two open tasks."),
            ],
        ),
    );
    service.scan(&fixture.roots, &[]).unwrap();
    let (imports, turns) = counts(&store);
    assert_eq!(imports, 2);
    assert_eq!(turns, 4);

    service.scan(&fixture.roots, &[]).unwrap();
    let (imports_again, turns_again) = counts(&store);
    assert_eq!(imports_again, imports, "the same turn range is a no-op");
    assert_eq!(turns_again, turns);

    let mut grown = claude_body("");
    grown.push_str(
        r#"{"type":"assistant","sessionId":"11111111-1111-4111-8111-111111111111","message":{"role":"assistant","content":[{"type":"text","text":"The follow-up turn arrived after the first scan."}]}}
"#,
    );
    write(&claude_path, &grown);
    service.scan(&fixture.roots, &[]).unwrap();
    let claude = service.show(&member, "claude-code", CLAUDE_ID).unwrap();
    assert_eq!(claude.turns.len(), 3);
    assert!(claude.turns[2].content.contains("follow-up"));
    let (imports_grown, turns_grown) = counts(&store);
    assert_eq!(imports_grown, 3, "only the new range is inserted");
    assert_eq!(turns_grown, 5);

    service.scan(&fixture.roots, &[]).unwrap();
    assert_eq!(
        counts(&store).1,
        5,
        "a later scan does not copy the follow-up"
    );

    write(
        &fixture.roots.codex_sessions.join("child.jsonl"),
        &codex_body(
            CODEX_CHILD,
            Some(CODEX_ID),
            &[
                ("user", "Summarize the open tasks"),
                ("assistant", "There are two open tasks."),
                ("user", "Add the part that was not in the parent."),
            ],
        ),
    );
    service.scan(&fixture.roots, &[]).unwrap();
    let child = service.show(&member, "codex", CODEX_CHILD).unwrap();
    assert_eq!(child.session.parent_session.as_deref(), Some(CODEX_ID));
    assert_eq!(child.turns.len(), 1);
    assert_eq!(child.turns[0].index, 2);
    assert!(child.turns[0].content.contains("not in the parent"));
    let copies: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM experience_turns WHERE content = 'Summarize the open tasks'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(copies, 1, "the forked file does not copy collected turns");
}

#[test]
fn a_pi_fork_records_the_parent_and_a_downtime_file_is_caught_on_the_next_scan() {
    let store = store();
    let fixture = fixture();
    let member = MemberId::new();
    let parent_exec = execution(&store, &member);
    let child_exec = execution(&store, &member);
    let service = ExperienceService::new(&store, &fixture.home);
    service.link_execution(&parent_exec, "pi", PI_ID).unwrap();
    service.link_execution(&child_exec, "pi", PI_CHILD).unwrap();
    let parent_path = fixture
        .roots
        .pi_sessions
        .join("cwd")
        .join(format!("2026_{PI_ID}.jsonl"));
    write(
        &parent_path,
        &pi_body(
            PI_ID,
            None,
            "Draft the recovery note",
            "Recovery keeps the original session id.",
        ),
    );
    service.scan(&fixture.roots, &[]).unwrap();

    let quoted = serde_json::to_string(parent_path.to_str().unwrap()).unwrap();
    write(
        &fixture
            .roots
            .pi_sessions
            .join("cwd")
            .join(format!("2026_{PI_CHILD}.jsonl")),
        &format!(
            r#"{{"type":"session","version":3,"id":"{PI_CHILD}","cwd":"/tmp/work","parentSession":{quoted}}}
{{"type":"message","id":"m1","parentId":null,"message":{{"role":"user","content":"Draft the recovery note"}}}}
{{"type":"message","id":"m2","parentId":"m1","message":{{"role":"assistant","content":"Recovery keeps the original session id."}}}}
{{"type":"message","id":"m3","parentId":"m2","message":{{"role":"user","content":"The new turn after the fork."}}}}
"#
        ),
    );
    service.scan(&fixture.roots, &[]).unwrap();
    let child = service.show(&member, "pi", PI_CHILD).unwrap();
    assert_eq!(child.session.parent_session.as_deref(), Some(PI_ID));
    assert_eq!(child.turns.len(), 1);
    assert_eq!(child.turns[0].content, "The new turn after the fork.");

    let late_id = "44444444-4444-4444-8444-444444444444";
    let late_exec = execution(&store, &member);
    service
        .link_execution(&late_exec, "claude-code", late_id)
        .unwrap();
    assert!(
        service
            .list(&member)
            .unwrap()
            .iter()
            .all(|row| row.session_id != late_id)
    );
    write(
        &fixture
            .roots
            .claude_projects
            .join(format!("{late_id}.jsonl")),
        &format!(
            r#"{{"type":"user","sessionId":"{late_id}","message":{{"role":"user","content":"This session ended while Viva was down."}}}}"#
        ),
    );
    service.scan(&fixture.roots, &[]).unwrap();
    let late = service.show(&member, "claude-code", late_id).unwrap();
    assert_eq!(late.turns.len(), 1);
    assert!(late.turns[0].content.contains("while Viva was down"));
}

#[test]
fn configured_secrets_are_removed_from_turns_and_the_archive_only() {
    let store = store();
    let fixture = fixture();
    let member = MemberId::new();
    let exec = execution(&store, &member);
    let service = ExperienceService::new(&store, &fixture.home);
    service
        .link_execution(&exec, "claude-code", CLAUDE_ID)
        .unwrap();
    let path = fixture
        .roots
        .claude_projects
        .join(format!("{CLAUDE_ID}.jsonl"));
    let secret = "sk-test-token-abc";
    write(
        &path,
        &format!(
            r#"{{"type":"user","sessionId":"{CLAUDE_ID}","message":{{"role":"user","content":"the token is {secret} in the note"}}}}
{{"type":"attachment","sessionId":"{CLAUDE_ID}","content":"sidecar {secret}"}}
"#
        ),
    );
    service.scan(&fixture.roots, &[secret.to_string()]).unwrap();
    let shown = service.show(&member, "claude-code", CLAUDE_ID).unwrap();
    assert!(!shown.turns[0].content.contains(secret));
    assert!(shown.turns[0].content.contains("[REDACTED]"));
    let archive = std::fs::read_to_string(&shown.session.archive_path).unwrap();
    assert!(!archive.contains(secret));
    assert!(archive.contains("[REDACTED]"));
    let original = std::fs::read_to_string(&path).unwrap();
    assert!(
        original.contains(secret),
        "the harness file is left untouched"
    );
    assert!(
        shown
            .session
            .source_path
            .ends_with(format!("{CLAUDE_ID}.jsonl").as_str())
    );
    assert!(shown.session.archive_path.contains(member.as_str()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&shown.session.archive_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn a_claude_sidechain_stays_its_own_session_on_the_parent_member() {
    let store = store();
    let fixture = fixture();
    let member = MemberId::new();
    let other = MemberId::new();
    let exec = execution(&store, &member);
    let service = ExperienceService::new(&store, &fixture.home);
    service
        .link_execution(&exec, "claude-code", CLAUDE_ID)
        .unwrap();
    write(
        &fixture
            .roots
            .claude_projects
            .join(format!("{CLAUDE_ID}.jsonl")),
        &claude_body(""),
    );
    write(
        &fixture
            .roots
            .claude_projects
            .join(CLAUDE_ID)
            .join("subagents")
            .join("agent-side-1.jsonl"),
        &format!(
            r#"{{"type":"user","isSidechain":true,"sessionId":"{CLAUDE_ID}","agentId":"agent-side-1","message":{{"role":"user","content":"Subagent-only turn."}}}}
{{"type":"assistant","isSidechain":true,"sessionId":"{CLAUDE_ID}","agentId":"agent-side-1","message":{{"role":"assistant","content":"Subagent-only reply."}}}}
"#
        ),
    );
    service.scan(&fixture.roots, &[]).unwrap();
    let listed = service.list(&member).unwrap();
    assert_eq!(listed.len(), 2);
    let parent = service.show(&member, "claude-code", CLAUDE_ID).unwrap();
    assert_eq!(parent.turns.len(), 2);
    assert!(
        parent
            .turns
            .iter()
            .all(|turn| !turn.content.contains("Subagent-only"))
    );
    let child = service
        .show(&member, "claude-code", "agent-side-1")
        .unwrap();
    assert_eq!(child.session.parent_session.as_deref(), Some(CLAUDE_ID));
    assert_eq!(child.turns.len(), 2);
    assert_eq!(child.turns[0].content, "Subagent-only turn.");
    assert!(service.list(&other).unwrap().is_empty());
}

#[test]
fn experience_cli_lists_isolates_and_redacts_with_the_real_binary() {
    let home = tempfile::tempdir().expect("home");
    let roots = tempfile::tempdir().expect("roots");
    let claude = roots.path().join("claude");
    let codex = roots.path().join("codex");
    let pi = roots.path().join("pi");
    std::fs::create_dir_all(&claude).unwrap();
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::create_dir_all(&pi).unwrap();

    let (ok, _, stderr) = run_viva(home.path(), &["init"]);
    assert!(ok, "init failed: {stderr}");

    let store = Store::open(&database_path(home.path()), viva::server::migrations()).expect("open");
    let member = MemberId::new();
    let other = MemberId::new();
    let exec = execution(&store, &member);
    drop(store);

    let secret = "sk-test-token-abc";
    std::fs::write(home.path().join("redaction.secrets"), format!("{secret}\n")).unwrap();
    std::fs::write(
        claude.join(format!("{CLAUDE_ID}.jsonl")),
        format!(
            r#"{{"type":"user","sessionId":"{CLAUDE_ID}","message":{{"role":"user","content":"note {secret} for the member"}}}}
{{"type":"assistant","sessionId":"{CLAUDE_ID}","message":{{"role":"assistant","content":"Stored without the token."}}}}
"#
        ),
    )
    .unwrap();

    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "experience",
            "link",
            "--execution",
            exec.as_str(),
            "--harness",
            "claude",
            "--session",
            CLAUDE_ID,
        ],
    );
    assert!(ok, "link failed: {stderr}");

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "experience",
            "scan",
            "--claude-root",
            claude.to_str().unwrap(),
            "--codex-root",
            codex.to_str().unwrap(),
            "--pi-root",
            pi.to_str().unwrap(),
        ],
    );
    assert!(ok, "scan failed: {stderr}\n{stdout}");
    assert!(stdout.contains("\"assigned\": 1"), "{stdout}");

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &["experience", "list", "--member", member.as_str()],
    );
    assert!(ok, "list failed: {stderr}");
    assert!(stdout.contains(CLAUDE_ID), "{stdout}");
    assert!(stdout.contains("claude-code"), "{stdout}");

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &["experience", "list", "--member", other.as_str()],
    );
    assert!(ok, "other list failed: {stderr}");
    assert!(!stdout.contains(CLAUDE_ID), "{stdout}");

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "experience",
            "show",
            "--member",
            member.as_str(),
            "--harness",
            "claude-code",
            "--session",
            CLAUDE_ID,
        ],
    );
    assert!(ok, "show failed: {stderr}");
    assert!(stdout.contains("Stored without the token."));
    assert!(!stdout.contains(secret), "{stdout}");
    assert!(stdout.contains("[REDACTED]"));

    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "experience",
            "show",
            "--member",
            other.as_str(),
            "--harness",
            "claude-code",
            "--session",
            CLAUDE_ID,
        ],
    );
    assert!(!ok, "another member must not read the session");
    assert!(stderr.contains("not found"), "{stderr}");
}

fn run_viva(home: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .output()
        .expect("spawn viva");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}
