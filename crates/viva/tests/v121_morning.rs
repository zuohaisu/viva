//! C4 morning report acceptance (issue #121): live Task + read-only GitHub
//! rollup, honest github-auth blocking, corrections on Task, child grants.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use viva::authority::{AuthorityEngine, GrantMode};
use viva::coordinator::{MorningOptions, MorningRollup};
use viva::foundation::ids::{MemberId, TaskId};
use viva::foundation::store::{
    DOMAIN_FOUNDATION, DOMAIN_GIT, DOMAIN_TASKS_EXECUTIONS, FOUNDATION_V1_SQL, MigrationRegistry,
    Store,
};
use viva::git::worktrees::WorktreeSource;
use viva::tasks::{TaskRegistry, TaskStatus};

fn frozen() -> viva::foundation::store::FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(DOMAIN_GIT, 1, "git v1", viva::git::worktrees::GIT_V1_SQL)
        .register(
            DOMAIN_TASKS_EXECUTIONS,
            1,
            "tasks v1",
            viva::tasks::TASKS_EXECUTIONS_V1_SQL,
        )
        .register_rebuild(
            DOMAIN_TASKS_EXECUTIONS,
            2,
            "tasks v2",
            viva::tasks::TASKS_EXECUTIONS_V2_SQL,
        )
        .register(
            viva::foundation::store::DOMAIN_AUTHORITY,
            1,
            "authority v1",
            viva::authority::AUTHORITY_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

fn write_fake_gh(dir: &Path) -> std::path::PathBuf {
    let gh = dir.join("gh");
    // JSON shapes match `gh pr view --json` and
    // `gh api repos/{owner}/{repo}/pulls/{n}/comments` (gh 2.101).
    fs::write(
        &gh,
        r#"#!/bin/sh
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then exit 0; fi
if [ "$1" = "api" ]; then
  path=""
  for arg in "$@"; do
    case "$arg" in
      repos/*) path=$arg ;;
    esac
  done
  case "$path" in
    repos/example/repo/pulls/11/comments)
      echo '[{"user":{"login":"reviewer"},"created_at":"2026-10-15T10:00:00Z","body":"a"},{"user":{"login":"reviewer"},"created_at":"2026-10-15T11:00:00Z","body":"b"},{"user":{"login":"reviewer"},"created_at":"2026-10-15T12:00:00Z","body":"c"},{"user":{"login":"dev"},"created_at":"2026-10-15T12:30:00Z","body":"self"},{"user":{"login":"reviewer"},"created_at":"2026-10-14T12:00:00Z","body":"old"}]'
      exit 0
      ;;
    repos/example/repo/pulls/*/comments)
      echo '[]'
      exit 0
      ;;
  esac
  exit 1
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  case "$3" in
    feature-wait|42)
      echo '{"number":42,"state":"OPEN","url":"https://github.com/example/repo/pull/42","isDraft":false,"mergeable":"MERGEABLE","statusCheckRollup":[{"__typename":"CheckRun","conclusion":"SUCCESS","status":"COMPLETED","name":"rust","startedAt":"2026-10-15T11:00:00Z","completedAt":"2026-10-15T12:00:00Z","detailsUrl":"https://example/checks/rust","workflowName":"CI"}],"author":{"login":"dev"},"reviews":[{"author":{"login":"reviewer"},"state":"CHANGES_REQUESTED","submittedAt":"2026-10-15T10:00:00Z"},{"author":{"login":"reviewer"},"state":"CHANGES_REQUESTED","submittedAt":"2026-10-15T11:00:00Z"},{"author":{"login":"reviewer"},"state":"CHANGES_REQUESTED","submittedAt":"2026-10-15T12:00:00Z"}],"commits":[{"oid":"a","committedDate":"2026-10-15T10:30:00Z"},{"oid":"b","committedDate":"2026-10-15T11:30:00Z"},{"oid":"c","committedDate":"2026-10-15T12:30:00Z"}]}'
      exit 0
      ;;
    feature-draft|7)
      echo '{"number":7,"state":"OPEN","url":"https://github.com/example/repo/pull/7","isDraft":true,"mergeable":"MERGEABLE","statusCheckRollup":[{"__typename":"CheckRun","conclusion":"SUCCESS","status":"COMPLETED","name":"rust","workflowName":"CI"}],"author":{"login":"dev"},"reviews":[],"commits":[]}'
      exit 0
      ;;
    feature-blocked|9)
      echo '{"number":9,"state":"OPEN","url":"https://github.com/example/repo/pull/9","isDraft":false,"mergeable":"CONFLICTING","statusCheckRollup":[{"__typename":"CheckRun","conclusion":"FAILURE","status":"COMPLETED","name":"rust","workflowName":"CI"},{"__typename":"StatusContext","state":"ERROR","context":"ci","targetUrl":"https://example/status"}],"author":{"login":"dev"},"reviews":[],"commits":[]}'
      exit 0
      ;;
    feature-comments|11)
      echo '{"number":11,"state":"OPEN","url":"https://github.com/example/repo/pull/11","isDraft":false,"mergeable":"MERGEABLE","statusCheckRollup":[{"__typename":"CheckRun","conclusion":"SUCCESS","status":"COMPLETED","name":"rust","workflowName":"CI"}],"author":{"login":"dev"},"reviews":[],"commits":[]}'
      exit 0
      ;;
    feature-nochecks|12)
      echo '{"number":12,"state":"OPEN","url":"https://github.com/example/repo/pull/12","isDraft":false,"mergeable":"MERGEABLE","statusCheckRollup":[],"author":{"login":"dev"},"reviews":[],"commits":[]}'
      exit 0
      ;;
    feature-unknown|13)
      echo '{"number":13,"state":"OPEN","url":"https://github.com/example/repo/pull/13","isDraft":false,"mergeable":"UNKNOWN","statusCheckRollup":[],"author":{"login":"dev"},"reviews":[{"author":{"login":"reviewer"},"state":"CHANGES_REQUESTED","submittedAt":"2026-10-15T10:00:00Z"}],"commits":[]}'
      exit 0
      ;;
  esac
fi
exit 1
"#,
    )
    .expect("write gh");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("chmod");
    gh
}

fn bind_worktree(store: &Store, task_id: &TaskId, branch: &str, cwd: &Path) {
    let wt = viva::foundation::ids::WorktreeId::new();
    store
        .connection()
        .execute(
            "INSERT INTO task_worktrees(worktree_id, task_id, repo_root, worktree_path, branch,
             base_sha, source, created_at, released_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
            rusqlite::params![
                wt.as_str(),
                task_id.as_str(),
                cwd.to_string_lossy(),
                cwd.to_string_lossy(),
                branch,
                "abc",
                WorktreeSource::Created.as_str(),
                "2026-10-15T00:00:00Z",
            ],
        )
        .expect("insert worktree");
}

#[test]
fn morning_report_classifies_merge_draft_blocked_and_multi_round() {
    let dir = tempfile::TempDir::new().expect("dir");
    let gh = write_fake_gh(dir.path());
    let store = Store::open(&dir.path().join("viva.db"), &frozen()).expect("store");
    let tasks = TaskRegistry::new(&store);

    let wait = tasks
        .create_task("wait merge", vec![], None, None, None)
        .expect("task");
    bind_worktree(&store, &wait.task_id, "feature-wait", dir.path());

    let draft = tasks
        .create_task("draft pr", vec![], None, None, None)
        .expect("task");
    bind_worktree(&store, &draft.task_id, "feature-draft", dir.path());

    let blocked = tasks
        .create_task("blocked pr", vec![], None, None, None)
        .expect("task");
    tasks
        .set_status(&blocked.task_id, TaskStatus::Blocked)
        .expect("blocked");
    bind_worktree(&store, &blocked.task_id, "feature-blocked", dir.path());

    let comments = tasks
        .create_task("review comments", vec![], None, None, None)
        .expect("task");
    bind_worktree(&store, &comments.task_id, "feature-comments", dir.path());

    let nochecks = tasks
        .create_task("no checks", vec![], None, None, None)
        .expect("task");
    bind_worktree(&store, &nochecks.task_id, "feature-nochecks", dir.path());

    let unknown = tasks
        .create_task("unknown pushes", vec![], None, None, None)
        .expect("task");
    bind_worktree(&store, &unknown.task_id, "feature-unknown", dir.path());

    let options = MorningOptions::parse_window("2026-10-15T00:00:00Z", "2026-10-16T00:00:00Z", 0)
        .expect("window")
        .with_gh_program(gh.to_string_lossy());

    let report = MorningRollup::new(&store, options).build().expect("report");
    assert!(matches!(
        report.github_auth,
        viva::coordinator::GithubAuth::Available
    ));

    let wait_item = report
        .items
        .iter()
        .find(|i| i.task_id == wait.task_id.to_string())
        .expect("wait item");
    assert!(matches!(
        wait_item.waiting_merge,
        Some(viva::coordinator::WaitingMerge::Open { pr_number: 42, .. })
    ));
    assert_eq!(wait_item.blocked.github_checks_failed, Some(false));
    assert_eq!(wait_item.blocked.merge_conflict, Some(false));
    assert!(matches!(
        wait_item.multi_round,
        viva::coordinator::MultiRoundSignal::Yes { .. }
    ));

    let draft_item = report
        .items
        .iter()
        .find(|i| i.task_id == draft.task_id.to_string())
        .expect("draft");
    assert!(matches!(
        draft_item.waiting_merge,
        Some(viva::coordinator::WaitingMerge::Draft { .. })
    ));
    assert!(matches!(
        draft_item.multi_round,
        viva::coordinator::MultiRoundSignal::No
    ));

    let blocked_item = report
        .items
        .iter()
        .find(|i| i.task_id == blocked.task_id.to_string())
        .expect("blocked");
    assert!(blocked_item.blocked.viva_task_blocked);
    assert_eq!(blocked_item.blocked.github_checks_failed, Some(true));
    assert_eq!(blocked_item.blocked.merge_conflict, Some(true));

    let comments_item = report
        .items
        .iter()
        .find(|i| i.task_id == comments.task_id.to_string())
        .expect("comments");
    assert!(matches!(
        comments_item.multi_round,
        viva::coordinator::MultiRoundSignal::Yes { .. }
    ));

    let nochecks_item = report
        .items
        .iter()
        .find(|i| i.task_id == nochecks.task_id.to_string())
        .expect("nochecks");
    assert!(matches!(
        nochecks_item.waiting_merge,
        Some(viva::coordinator::WaitingMerge::Open { pr_number: 12, .. })
    ));
    assert_eq!(nochecks_item.blocked.github_checks_failed, None);

    let unknown_item = report
        .items
        .iter()
        .find(|i| i.task_id == unknown.task_id.to_string())
        .expect("unknown");
    assert_eq!(unknown_item.blocked.merge_conflict, None);
    assert!(matches!(
        unknown_item.multi_round,
        viva::coordinator::MultiRoundSignal::Unknown { .. }
    ));
}

#[test]
fn without_gh_auth_github_is_blocked_but_viva_tasks_remain() {
    let dir = tempfile::TempDir::new().expect("dir");
    let gh = dir.path().join("gh-deny");
    fs::write(&gh, "#!/bin/sh\nexit 1\n").expect("write");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("chmod");

    let store = Store::open(&dir.path().join("viva.db"), &frozen()).expect("store");
    TaskRegistry::new(&store)
        .create_task("local only", vec![], None, None, None)
        .expect("task");

    let options = MorningOptions::parse_window("2026-10-15T00:00:00Z", "2026-10-16T00:00:00Z", 0)
        .expect("window")
        .with_gh_program(gh.to_string_lossy());

    let report = MorningRollup::new(&store, options).build().expect("report");
    assert!(matches!(
        report.github_auth,
        viva::coordinator::GithubAuth::Blocked { .. }
    ));
    assert!(!report.items.is_empty());
    assert!(matches!(
        report.items[0].github,
        viva::coordinator::GithubSection::Blocked { .. }
    ));
}

#[test]
fn correction_is_readable_from_task_and_child_grant_cannot_widen() {
    let store = Store::open_in_memory(&frozen()).expect("store");
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task("correct me", vec![], None, None, None)
        .expect("task");
    tasks
        .record_coordinator_note(&task.task_id, "pivot to smaller scope", "haisu", "dedup-1")
        .expect("note");
    let outcomes = tasks.outcomes_for_task(&task.task_id).expect("outcomes");
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].evidence.contains("pivot"));

    let engine = AuthorityEngine::new(&store);
    let parent = engine
        .issue_root_grant(
            None,
            Some(task.task_id.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActWithApproval,
            None,
        )
        .expect("parent");
    assert!(
        engine
            .delegate_grant(
                &parent.grant_id,
                MemberId::new(),
                vec!["dispatch_delegated".into(), "invoke_worker".into()],
                GrantMode::ActWithApproval,
                None,
            )
            .is_err(),
        "child grant must not add actions outside the parent scope"
    );
    let denials = engine.recent_denials(3).expect("denials");
    assert!(
        denials
            .iter()
            .any(|(_, action, _, _)| action == "delegate_grant"),
        "widening delegation must leave a denial record"
    );
}

#[test]
fn tasks_executions_v2_migration_preserves_nonempty_v1_database() {
    let dir = tempfile::TempDir::new().expect("dir");
    let v1_only = MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(
            DOMAIN_TASKS_EXECUTIONS,
            1,
            "tasks v1",
            viva::tasks::TASKS_EXECUTIONS_V1_SQL,
        )
        .freeze()
        .expect("v1");
    let store = Store::open(&dir.path().join("viva.db"), &v1_only).expect("v1 open");
    let task = TaskRegistry::new(&store)
        .create_task("legacy task", vec![], None, None, None)
        .expect("task");
    store
        .connection()
        .execute(
            "INSERT INTO task_outcomes(task_id, kind, evidence, actor, recorded_at)
             VALUES (?1, 'corrected', 'note', 'qa', '2026-10-15T00:00:00Z')",
            [task.task_id.as_str()],
        )
        .expect("outcome");
    drop(store);

    let full = frozen();
    let store = Store::open(&dir.path().join("viva.db"), &full).expect("v2 open");
    store
        .connection()
        .query_row(
            "SELECT status FROM tasks WHERE task_id = ?1",
            [task.task_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .expect("task survived");
}
