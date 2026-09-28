//! V10 acceptance tests (issue #19): conversation metadata through the real
//! `viva` binary — fork trees that survive restarts, renames that stay,
//! forks that never create tasks, task closure that never deletes
//! conversations, and cross-harness handoffs that stay traceable.
//!
//! Real-Pi interactive forking (a human forking live Pi sessions) stays
//! pending until a Pi installation with model credentials exercises it;
//! the extension enforces native-fork-first ordering and is covered by its
//! own TS suite (`extensions/pi/`).

use std::process::Command;
use tempfile::TempDir;

use viva::conversations::{ConversationRegistry, ForkCapability};
use viva::foundation::records::{SessionKind, SessionRecord, insert_session};
use viva::foundation::store::Store;
use viva::office::office_migrations;
use viva::tasks::TaskRegistry;

fn run_viva(home: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .output()
        .expect("viva runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn conversation_tree_survives_restart_and_task_closure() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().join("home");

    // Seed a session + a task in the store directly.
    let (session_id, task_id) = {
        let store = Store::open(
            &viva::foundation::paths::database_path(&home),
            office_migrations(),
        )
        .expect("store");
        let session = SessionRecord::new(SessionKind::Conversation, "Design chat");
        insert_session(&store, &session).expect("session");
        let task = TaskRegistry::new(&store)
            .create_task("design the tree", vec![], None, None, None)
            .expect("task");
        (session.session_id, task.task_id)
    };

    // Build a tree through the real CLI: root -> two branches.
    let (code, out, err) = run_viva(
        &home,
        &[
            "conversations",
            "create",
            "--session",
            session_id.as_str(),
            "--name",
            "Main line",
        ],
    );
    assert_eq!(code, 0, "create failed: {err}");
    let root: serde_json::Value = serde_json::from_str(out.trim()).expect("root json");
    let root_id = root["node_id"].as_str().expect("node id").to_string();

    let (code, out, err) = run_viva(
        &home,
        &[
            "conversations",
            "fork",
            "--parent",
            &root_id,
            "--name",
            "Theme A",
            "--native-session",
            "pi-native-a",
        ],
    );
    assert_eq!(code, 0, "fork failed: {err}");
    let branch_a: serde_json::Value = serde_json::from_str(out.trim()).expect("branch json");
    let branch_a_id = branch_a["node_id"].as_str().unwrap().to_string();
    assert_eq!(
        branch_a["task_id"],
        serde_json::Value::Null,
        "a fork never creates a task"
    );

    let (code, _, err) = run_viva(
        &home,
        &[
            "conversations",
            "fork",
            "--parent",
            &root_id,
            "--name",
            "Theme B",
        ],
    );
    assert_eq!(code, 0, "fork b failed: {err}");

    // Rename one branch; attach the task to it explicitly.
    let (code, _, err) = run_viva(
        &home,
        &[
            "conversations",
            "rename",
            "--node",
            &branch_a_id,
            "--name",
            "Theme A (renamed)",
        ],
    );
    assert_eq!(code, 0, "rename failed: {err}");
    let (code, _, err) = run_viva(
        &home,
        &[
            "conversations",
            "attach-task",
            "--node",
            &branch_a_id,
            "--task",
            task_id.as_str(),
        ],
    );
    assert_eq!(code, 0, "attach failed: {err}");

    // Close (complete) the task: the conversation tree must survive.
    {
        let store = Store::open(
            &viva::foundation::paths::database_path(&home),
            office_migrations(),
        )
        .expect("reopen");
        TaskRegistry::new(&store)
            .complete_task(&task_id, "design accepted by the owner", "owner")
            .expect("complete");
    }
    let (code, out, err) = run_viva(
        &home,
        &["conversations", "tree", "--session", session_id.as_str()],
    );
    assert_eq!(code, 0, "tree failed: {err}");
    let tree: serde_json::Value = serde_json::from_str(out.trim()).expect("tree json");
    assert_eq!(
        tree.as_array().expect("array").len(),
        3,
        "root + two branches survive task closure"
    );
    let renamed = tree
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["node_id"] == value_text(&branch_a_id))
        .expect("branch still present");
    assert_eq!(renamed["display_name"], value_text("Theme A (renamed)"));
    assert_eq!(renamed["task_id"], value_text(task_id.as_str()));

    // Native references are pointers, labeled to the harness.
    assert_eq!(renamed["native_session_id"], value_text("pi-native-a"));
    assert_eq!(renamed["harness"], value_text("pi"));
}

#[test]
fn handoff_to_a_capability_declared_harness_stays_traceable() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().join("home");
    let store = Store::open(
        &viva::foundation::paths::database_path(&home),
        office_migrations(),
    )
    .expect("store");
    let session = SessionRecord::new(SessionKind::Conversation, "Handoff source");
    insert_session(&store, &session).expect("session");
    let session_id = session.session_id.clone();
    drop(store);

    let (code, out, err) = run_viva(
        &home,
        &[
            "conversations",
            "create",
            "--session",
            session_id.as_str(),
            "--name",
            "Main",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let root: serde_json::Value = serde_json::from_str(out.trim()).expect("root json");
    let root_id = root["node_id"].as_str().unwrap().to_string();

    // Hand off to a handoff-only harness: capability declared, not assumed.
    let (code, out, err) = run_viva(
        &home,
        &[
            "conversations",
            "handoff",
            "--node",
            &root_id,
            "--to",
            "codex",
            "--capability",
            "handoff_only",
            "--brief",
            "Design decisions so far: tree in office, transcript stays in Pi",
            "--history-ref",
            "node-history-ref-1",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let handoff: serde_json::Value = serde_json::from_str(out.trim()).expect("handoff json");
    assert_eq!(handoff["to_capability"], value_text("handoff_only"));
    assert_eq!(
        handoff["brief_snapshot"],
        value_text("Design decisions so far: tree in office, transcript stays in Pi")
    );
    assert_eq!(handoff["history_ref"], value_text("node-history-ref-1"));

    // The original record is untouched and traceable from the handoff.
    let store = Store::open(
        &viva::foundation::paths::database_path(&home),
        office_migrations(),
    )
    .expect("reopen");
    let registry = ConversationRegistry::new(&store);
    let handoffs = registry.handoffs_for_node(&root_id).expect("handoffs");
    assert_eq!(handoffs.len(), 1);
    assert_eq!(handoffs[0].to_capability, ForkCapability::HandoffOnly);
    assert_eq!(
        registry
            .get_node(&root_id)
            .expect("get")
            .expect("exists")
            .display_name,
        "Main",
        "the source branch survives the handoff unchanged"
    );
}

fn value_text(text: &str) -> serde_json::Value {
    serde_json::Value::String(text.to_string())
}
