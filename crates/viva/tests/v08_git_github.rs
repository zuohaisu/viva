//! V08 acceptance tests (issue #17): task worktree isolation, protected
//! ref refusal, one-checkout-per-branch, discovery/adopt, and honest
//! GitHub evidence — against real temporary git repositories.

use std::path::{Path, PathBuf};
use std::process::Command;

use viva::foundation::ids::{TaskId, WorktreeId};
use viva::foundation::store::{
    DOMAIN_FOUNDATION, DOMAIN_GIT, FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::git::cli::{CliRunner, TreeState};
use viva::git::evidence::{EvidenceState, EvidenceStore, GhRunner};
use viva::git::worktrees::{ProtectedRefs, WorktreeService, WorktreeSource};

fn frozen() -> viva::foundation::store::FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(DOMAIN_GIT, 1, "git v1", viva::git::worktrees::GIT_V1_SQL)
        .freeze()
        .expect("registry")
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@test")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@test")
        .output()
        .expect("git runs");
    assert!(
        status.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repo pair for testing: `origin` is a bare repo with one commit on
/// `main`; `seed` is a clone of it.
struct RepoPair {
    dir: PathBuf,
    seed: PathBuf,
}

fn setup_repo(name: &str) -> RepoPair {
    let dir = tempfile::TempDir::new().expect("dir").keep();
    let origin = dir.join(format!("{name}-origin"));
    let seed = dir.join(format!("{name} seed with spaces")); // spaces on purpose
    std::fs::create_dir_all(&origin).expect("mkdir origin");

    git(&origin, &["init", "-q", "--bare", "-b", "main"]);
    std::fs::create_dir_all(&seed).expect("mkdir seed");
    git(&seed, &["init", "-q"]);
    git(&seed, &["checkout", "-q", "-b", "main"]);
    std::fs::write(seed.join("file.txt"), "one\n").expect("write");
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-qm", "initial"]);
    git(
        &seed,
        &["remote", "add", "origin", origin.to_string_lossy().as_ref()],
    );
    git(&seed, &["push", "-q", "origin", "main"]);
    // The local main stays one commit behind origin after this push? No —
    // push does not move origin/HEAD refs locally; fetch below aligns.
    git(&seed, &["fetch", "origin"]);
    RepoPair { dir, seed }
}

impl RepoPair {
    /// Advance origin/main with a NEW commit, leaving local main stale.
    fn advance_origin(&self, note: &str) -> String {
        std::fs::write(self.seed.join("file.txt"), format!("{note}\n")).expect("write");
        git(&self.seed, &["add", "."]);
        git(&self.seed, &["commit", "-qm", note]);
        git(&self.seed, &["push", "-q", "origin", "main"]);
        // Reset the local main back one commit: the clone must NOT matter —
        // worktree creation must use origin/main regardless of local main.
        git(&self.seed, &["reset", "-q", "--hard", "HEAD~1"]);
        git_out(&self.seed, &["rev-parse", "origin/main"])
    }
}

/// Acceptance: "两个真实临时 git repo 的任务 worktree 不相互覆盖，不从旧本地
/// main 起步；protected ref 参数拒绝".
#[test]
fn task_worktrees_branch_from_latest_remote_and_refuse_protected_refs() {
    let frozen = frozen();
    let store = Store::open(
        &tempfile::TempDir::new()
            .expect("dir")
            .keep()
            .join("office.db"),
        &frozen,
    )
    .expect("store");
    let pair_a = setup_repo("alpha");
    let pair_b = setup_repo("beta");

    // Advance origin AFTER the clone: local main is now stale.
    let latest_a = pair_a.advance_origin("second commit on alpha");
    let latest_b = pair_b.advance_origin("second commit on beta");
    let stale_local_main = git_out(&pair_a.seed, &["rev-parse", "main"]);
    assert_ne!(latest_a, stale_local_main, "local main is stale by setup");

    let mut service = WorktreeService::new(&store, ProtectedRefs::new(vec![]));
    let task_a = TaskId::new();
    let wt_a = service
        .create_task_worktree(
            &pair_a.seed,
            pair_a.dir.join("worktrees").as_path(),
            &task_a,
            "task-alpha-1",
        )
        .expect("create a");
    let task_b = TaskId::new();
    let wt_b = service
        .create_task_worktree(
            &pair_b.seed,
            pair_b.dir.join("worktrees").as_path(),
            &task_b,
            "task-beta-1",
        )
        .expect("create b");

    // Not from stale local main: base == latest origin/main.
    assert_eq!(
        wt_a.base_sha, latest_a,
        "alpha worktree bases on origin/main"
    );
    assert_eq!(
        wt_b.base_sha, latest_b,
        "beta worktree bases on origin/main"
    );
    assert_ne!(wt_a.worktree_path, wt_b.worktree_path, "no cross覆盖");

    // The checked-out branch inside the worktree is the task branch.
    let runner = CliRunner::default();
    assert_eq!(
        viva::git::cli::current_branch(&runner, &wt_a.worktree_path)
            .expect("branch")
            .as_deref(),
        Some("task-alpha-1")
    );
    assert_eq!(
        viva::git::cli::head_sha(&runner, &wt_a.worktree_path).expect("head"),
        latest_a
    );

    // Protected ref rejection: branch name AND a protected checkout.
    let err = service
        .create_task_worktree(
            &pair_a.seed,
            pair_a.dir.join("worktrees").as_path(),
            &TaskId::new(),
            "main",
        )
        .expect_err("protected branch must refuse");
    assert!(err.to_string().contains("protected"), "got: {err}");

    // A path with spaces did not break anything (see setup_repo).
    assert!(
        wt_a.worktree_path.to_string_lossy().contains(' ')
            || !pair_a.dir.to_string_lossy().contains(' ')
    );
}

/// Acceptance: "多任务同仓 worktree 创建/branch选择互斥并拒绝共享可写checkout".
#[test]
fn shared_writable_checkouts_are_refused_and_adoption_records_ownership() {
    let frozen = frozen();
    let store = Store::open(
        &tempfile::TempDir::new()
            .expect("dir")
            .keep()
            .join("office.db"),
        &frozen,
    )
    .expect("store");
    let pair = setup_repo("gamma");
    let mut service = WorktreeService::new(&store, ProtectedRefs::new(vec![]));

    let wt = service
        .create_task_worktree(
            &pair.seed,
            pair.dir.join("worktrees").as_path(),
            &TaskId::new(),
            "task-gamma-1",
        )
        .expect("first task worktree");

    // Same branch again: refused (registry claim).
    let err = service
        .create_task_worktree(
            &pair.seed,
            pair.dir.join("worktrees").as_path(),
            &TaskId::new(),
            "task-gamma-1",
        )
        .expect_err("second claim on the same branch must refuse");
    assert!(err.to_string().contains("already"), "got: {err}");

    // A user-made second worktree (as Orca or a human might) is discovered.
    let manual = pair.dir.join("manual wt");
    git(
        &pair.seed,
        &[
            "worktree",
            "add",
            "-b",
            "user-branch",
            manual.to_string_lossy().as_ref(),
        ],
    );
    let discovered = service.discover(&pair.seed).expect("discover");
    let branches: Vec<_> = discovered.iter().filter_map(|w| w.branch.clone()).collect();
    assert!(
        branches.contains(&"user-branch".to_string()),
        "discovered: {branches:?}"
    );
    assert!(branches.contains(&"task-gamma-1".to_string()));

    // The office refuses to create another writable checkout of it…
    let err = service
        .create_task_worktree(
            &pair.seed,
            pair.dir.join("worktrees").as_path(),
            &TaskId::new(),
            "user-branch",
        )
        .expect_err("shared writable checkout must refuse");
    assert!(err.to_string().contains("writable"), "got: {err}");

    // …but it can be explicitly adopted for a task, with history preserved.
    let record = service
        .adopt_existing(&pair.seed, &manual, &TaskId::new())
        .expect("adopt");
    assert_eq!(record.source, WorktreeSource::Adopted);
    assert_eq!(record.branch, "user-branch");
    assert_eq!(service.all_records().expect("records").len(), 2);

    // Release keeps the row; nothing was deleted from disk.
    let worktree_id = wt.worktree_id.clone();
    service.release(&worktree_id).expect("release");
    assert!(
        wt.worktree_path.exists(),
        "release never deletes the directory"
    );
    assert_eq!(
        service.all_records().expect("records").len(),
        2,
        "history stays"
    );
}

/// Discovery annotates dirty state and shows it without touching anything.
#[test]
fn discovery_marks_dirty_worktrees_without_modifying_them() {
    let frozen = frozen();
    let store = Store::open(
        &tempfile::TempDir::new()
            .expect("dir")
            .keep()
            .join("office.db"),
        &frozen,
    )
    .expect("store");
    let pair = setup_repo("delta");
    let mut service = WorktreeService::new(&store, ProtectedRefs::new(vec![]));

    let wt = service
        .create_task_worktree(
            &pair.seed,
            pair.dir.join("worktrees").as_path(),
            &TaskId::new(),
            "task-delta-1",
        )
        .expect("create");
    let before = git_out(&wt.worktree_path, &["status", "--porcelain"]);
    assert!(before.is_empty(), "fresh worktree is clean");

    std::fs::write(wt.worktree_path.join("file.txt"), "dirty change\n").expect("modify");

    let discovered = service.discover(&pair.seed).expect("discover");
    let mine = discovered
        .iter()
        .find(|w| w.branch.as_deref() == Some("task-delta-1"))
        .expect("worktree found");
    match &mine.tree {
        TreeState::Dirty { entries } => assert!(!entries.is_empty()),
        other => panic!("expected dirty, got {other:?}"),
    }

    // The discovery was read-only: the change is still there.
    let content = std::fs::read_to_string(wt.worktree_path.join("file.txt")).expect("read");
    assert_eq!(content, "dirty change\n");

    // QA round additions: the discovery shows the office claim's source,
    // and the diff API returns a bounded, real diff against HEAD.
    assert_eq!(
        mine.office_source,
        Some(viva::git::worktrees::WorktreeSource::Created)
    );
    let diff = service
        .worktree_diff(&wt.worktree_path, 64 * 1024)
        .expect("diff");
    assert!(diff.contains("dirty change"), "real diff content: {diff}");
    let tiny = service
        .worktree_diff(&wt.worktree_path, 16)
        .expect("diff bounded");
    assert!(
        tiny.contains("truncated"),
        "bounded diff is visibly truncated: {tiny}"
    );
}

/// Acceptance: "真实 gh 查询的结果与 head SHA 可追溯，缺联网/认证时不报告
/// 证据 PASS" + "对 stale evidence 做标记".
#[test]
fn evidence_binds_to_head_sha_marks_stale_and_never_fakes_pass() {
    let frozen = frozen();
    let store = Store::open(
        &tempfile::TempDir::new()
            .expect("dir")
            .keep()
            .join("office.db"),
        &frozen,
    )
    .expect("store");

    // A fetch that cannot actually look must say Unavailable. This sandbox
    // has no GitHub network access to a fake repo; whether or not `gh` is
    // authenticated on this machine, a PR 999999999 in an empty dir does
    // not exist — a Fetched state here would mean fabrication.
    let gh = GhRunner::default();
    let task = TaskId::new();
    let state = gh.fetch_pr(&tempfile::TempDir::new().expect("dir").keep(), 999_999_999);
    match &state {
        EvidenceState::Unavailable { reason } => assert!(!reason.is_empty()),
        EvidenceState::Fetched { .. } => panic!("fetching a nonexistent PR is fabrication"),
    }

    let evidence = EvidenceStore::new(&store);
    let record = evidence
        .bind(&task, None, "pr", "999", Some("abc123".into()), &state)
        .expect("bind even an unavailable state (it is a fact)");
    assert!(
        record.worktree_id.is_none(),
        "bind without a worktree stays None"
    );

    // The worktree binding persists (QA round: the parameter was ignored).
    let wt = WorktreeId::new();
    let bound = evidence
        .bind(
            &task,
            Some(&wt),
            "checks",
            "pr-7",
            Some("abc123".into()),
            &state,
        )
        .expect("bind with worktree");
    let reloaded = evidence
        .get_marking_stale(&bound.evidence_id, Some("abc123"))
        .expect("read")
        .expect("exists");
    assert_eq!(
        reloaded.worktree_id,
        Some(wt),
        "worktree binding roundtrips"
    );

    // Stale marking: worktree head moved past the bound SHA.
    let reloaded = evidence
        .get_marking_stale(&record.evidence_id, Some("def456"))
        .expect("read")
        .expect("exists");
    assert!(
        reloaded.stale,
        "evidence behind the head must be flagged stale"
    );

    // Matching head: not stale.
    let fresh = evidence
        .bind(&task, None, "pr", "1000", Some("def456".into()), &state)
        .expect("bind");
    let reloaded = evidence
        .get_marking_stale(&fresh.evidence_id, Some("def456"))
        .expect("read")
        .expect("exists");
    assert!(!reloaded.stale);

    // The unavailable row never claims success.
    let json: serde_json::Value = serde_json::from_str(&record.state_json).expect("json");
    assert_eq!(
        json["state"], "unavailable",
        "the stored state is honest about not looking"
    );
}

/// The whitelist unit path: writes are structurally impossible through the
/// read runner (no spawn happens for rejected args).
#[test]
fn gh_writes_cannot_reach_the_cli() {
    // Each of these would mutate GitHub if it ran; all are rejected in-process.
    for args in [
        vec!["pr", "merge", "1"],
        vec!["pr", "close", "1"],
        vec!["repo", "archive", "x/y"],
        vec!["issue", "edit", "1"],
        vec!["workflow", "run", "ci"],
    ] {
        assert!(
            GhRunner::validate_read_args(&args).is_err(),
            "{args:?} must reject"
        );
    }
}
