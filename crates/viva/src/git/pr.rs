//! Pull-request status and the GitHub Files-changed list, via read-only `gh`.
//!
//! The sidebar line and the PR panel both come from one `gh pr view`. The
//! file list is `GET repos/{owner}/{repo}/pulls/{n}/files` (the net
//! three-dot diff GitHub shows as Files changed). A missing `gh`, a missing
//! login, a network failure, and "no pull request" stay distinct — Viva
//! does not guess a PR from the branch name.
//!
//! Check failure uses the same tokens as the morning report
//! (`FAILURE`, `ERROR`, `TIMED_OUT`, `STARTUP_FAILURE`). An empty rollup
//! is unknown. A non-empty rollup with none of those tokens is not a
//! failure; the sidebar splits that bucket into `checks pending` (a check
//! is still running) and `checks passing` (every check has finished
//! without a failure token).

use std::path::Path;

use serde::Deserialize;

use crate::foundation::error::{Error, Result};
use crate::git::cli::CliRunner;
use crate::git::evidence::{EvidenceState, GhRunner};

/// How many file rows the panel keeps. The true total is still reported.
pub const PR_FILE_LIMIT: usize = 500;

/// How much of one file's patch the panel keeps. The true byte size is
/// still reported when GitHub sent more.
pub const PR_PATCH_LIMIT: usize = 64 * 1024;

const PR_VIEW_FIELDS: &str = "number,title,state,isDraft,url,mergeable,statusCheckRollup,reviewDecision,additions,deletions,changedFiles,commits,baseRefName,headRefName,headRefOid";

/// Conclusions and status-context states that mean the check failed.
const CHECK_FAILURES: &[&str] = &["FAILURE", "ERROR", "TIMED_OUT", "STARTUP_FAILURE"];

/// One check node from `statusCheckRollup`. `conclusion` is a CheckRun
/// field; `state` is a StatusContext field; `status` is the CheckRun
/// progress word (`COMPLETED`, `IN_PROGRESS`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckNode {
    pub conclusion: Option<String>,
    pub state: Option<String>,
    pub status: Option<String>,
}

/// Sidebar word for the rollup. Failure tokens match the morning report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksWord {
    Failing,
    /// At least one check has not finished, and none has failed.
    Pending,
    /// Every check has finished, and none has failed.
    Passing,
    Unknown,
}

impl ChecksWord {
    /// Morning's `github_checks_failed`: `None` when the rollup is empty
    /// or missing, `Some(true)` on a failure token, `Some(false)` otherwise
    /// (a still-running check has not failed).
    pub fn failed(self) -> Option<bool> {
        match self {
            ChecksWord::Unknown => None,
            ChecksWord::Failing => Some(true),
            ChecksWord::Pending | ChecksWord::Passing => Some(false),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ChecksWord::Failing => "checks failing",
            ChecksWord::Pending => "checks pending",
            ChecksWord::Passing => "checks passing",
            ChecksWord::Unknown => "checks unknown",
        }
    }
}

/// `true` when `value` is one of the morning-report failure tokens.
pub fn token_is_check_failure(value: Option<&str>) -> bool {
    value
        .map(|token| {
            CHECK_FAILURES
                .iter()
                .any(|failure| token.eq_ignore_ascii_case(failure))
        })
        .unwrap_or(false)
}

/// Classify a rollup. `None` or an empty slice is [`ChecksWord::Unknown`].
pub fn checks_word(nodes: Option<&[CheckNode]>) -> ChecksWord {
    let Some(nodes) = nodes else {
        return ChecksWord::Unknown;
    };
    if nodes.is_empty() {
        return ChecksWord::Unknown;
    }
    if nodes.iter().any(|node| {
        token_is_check_failure(node.conclusion.as_deref())
            || token_is_check_failure(node.state.as_deref())
    }) {
        return ChecksWord::Failing;
    }
    if nodes.iter().any(node_is_pending) {
        ChecksWord::Pending
    } else {
        ChecksWord::Passing
    }
}

fn node_is_pending(node: &CheckNode) -> bool {
    if let Some(status) = node.status.as_deref() {
        return !status.eq_ignore_ascii_case("COMPLETED");
    }
    if let Some(state) = node.state.as_deref() {
        return state.eq_ignore_ascii_case("PENDING") || state.eq_ignore_ascii_case("EXPECTED");
    }
    node.conclusion.is_none()
}

/// A pull request `gh pr view` actually returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrInfo {
    pub number: u64,
    pub title: String,
    /// `draft`, `open`, `merged`, or `closed`.
    pub state: String,
    pub url: String,
    pub checks: ChecksWord,
    /// `review approved`, `review changes requested`, `review pending`,
    /// or `review unknown` when the payload did not carry a decision.
    pub review: String,
    /// `merge clean`, `merge conflict`, or `merge unknown`.
    pub merge: String,
    pub changed_files: Option<u64>,
    pub additions: Option<i64>,
    pub deletions: Option<i64>,
    pub commits: Option<u64>,
    pub base_ref: String,
    pub head_ref: String,
    pub head_sha: String,
    pub fetched_at: String,
}

impl PrInfo {
    /// The sidebar scan line. File counts appear only when `gh` sent them.
    pub fn summary_line(&self) -> String {
        let mut line = format!(
            "#{} · {} · {} · {} · {}",
            self.number,
            self.state,
            self.checks.label(),
            self.review,
            self.merge
        );
        if let (Some(files), Some(add), Some(del)) =
            (self.changed_files, self.additions, self.deletions)
        {
            line.push_str(&format!(" · {files} files +{add} \u{2212}{del}"));
        }
        line
    }

    /// `#418  title`
    pub fn heading(&self) -> String {
        if self.title.is_empty() {
            format!("#{}", self.number)
        } else {
            format!("#{}  {}", self.number, self.title)
        }
    }

    /// `base ← head · N commits · head <sha> · fetched <time>`
    pub fn meta_line(&self) -> String {
        let mut parts = Vec::new();
        if !self.base_ref.is_empty() || !self.head_ref.is_empty() {
            parts.push(format!("{} ← {}", self.base_ref, self.head_ref));
        }
        if let Some(commits) = self.commits {
            parts.push(format!("{commits} commits"));
        }
        if !self.head_sha.is_empty() {
            parts.push(format!("head {}", short_sha(&self.head_sha)));
        }
        if !self.fetched_at.is_empty() {
            parts.push(format!("fetched {}", self.fetched_at));
        }
        parts.join(" · ")
    }
}

/// What a read-only `gh pr view <branch>` produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrLookup {
    Found(Box<PrInfo>),
    /// `gh` ran and reported that the branch has no pull request.
    NoPr,
    /// `gh` is missing, logged out, offline, or failed for another reason.
    Unknown {
        reason: String,
    },
}

impl PrLookup {
    pub fn summary_line(&self) -> String {
        match self {
            PrLookup::Found(info) => info.summary_line(),
            PrLookup::NoPr => "no pr".into(),
            PrLookup::Unknown { reason } => format!("unknown · {reason}"),
        }
    }

    /// The state word when a PR was found (`draft` / `open` / `merged` /
    /// `closed`). Absence is not a state.
    pub fn state_word(&self) -> Option<&str> {
        match self {
            PrLookup::Found(info) => Some(info.state.as_str()),
            PrLookup::NoPr | PrLookup::Unknown { .. } => None,
        }
    }
}

/// The PR of `branch`, or an honest miss. Callers display
/// [`PrLookup::summary_line`] and do not invent a PR from the branch name.
pub fn pr_status_for_branch(repo_root: &Path, branch: &str) -> PrLookup {
    pr_status_for_branch_using(repo_root, branch, "gh")
}

/// [`pr_status_for_branch`] with an explicit `gh` program. Tests inject a
/// fake here instead of racing the process-global PATH.
pub fn pr_status_for_branch_using(repo_root: &Path, branch: &str, gh_program: &str) -> PrLookup {
    if branch.is_empty() {
        return PrLookup::Unknown {
            reason: "worktree has no branch".into(),
        };
    }
    let gh = GhRunner::with_program(gh_program);
    let branch = branch.to_string();
    match gh.read(
        repo_root,
        &["pr", "view", &branch, "--json", PR_VIEW_FIELDS],
        parse_pr_view,
    ) {
        EvidenceState::Fetched {
            payload,
            fetched_at,
        } => {
            let mut info = payload;
            info.fetched_at = fetched_at;
            PrLookup::Found(Box::new(info))
        }
        EvidenceState::Unavailable { reason } => classify_unavailable(&reason),
    }
}

fn classify_unavailable(reason: &str) -> PrLookup {
    let lower = reason.to_lowercase();
    if lower.contains("no pull requests found") {
        return PrLookup::NoPr;
    }
    let short = if lower.contains("cannot launch") || lower.contains("no such file or directory") {
        "gh is not installed".into()
    } else if lower.contains("gh auth login")
        || lower.contains("http 401")
        || lower.contains("bad credentials")
        || lower.contains("authentication failed")
        || lower.contains("unauthorized")
    {
        "gh is not logged in".into()
    } else if lower.contains("could not resolve host")
        || lower.contains("connection refused")
        || lower.contains("network is unreachable")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("tls")
        || lower.contains("connection reset")
        || lower.contains("dial tcp")
        || lower.contains("temporary failure in name resolution")
    {
        "network failed".into()
    } else {
        one_line(reason)
    };
    PrLookup::Unknown { reason: short }
}

fn one_line(reason: &str) -> String {
    // The runner prefixes the argv. The useful part is what `gh` printed.
    let reason = reason
        .split_once("failed (")
        .and_then(|(_, rest)| rest.split_once(": ").map(|(_, tail)| tail))
        .unwrap_or(reason);
    let flat = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat.trim();
    if flat.is_empty() {
        return "gh failed".into();
    }
    flat.chars().take(160).collect()
}

/// `owner` and `repo` from a GitHub pull URL, or `None` when the URL is
/// not `https://host/{owner}/{repo}/pull/{n}`.
pub fn github_repo(url: &str) -> Option<(String, String)> {
    let (_scheme, rest) = url.split_once("://")?;
    let mut parts = rest.split('/');
    let _host = parts.next()?;
    let owner = parts.next()?;
    let repo = parts.next()?;
    let kind = parts.next()?;
    let number = parts.next()?;
    if parts.next().is_some() || (kind != "pull" && kind != "pulls") {
        return None;
    }
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

#[derive(Debug, Deserialize)]
struct RawCheck {
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

fn parse_pr_view(stdout: &str) -> Result<PrInfo> {
    let value: serde_json::Value = serde_json::from_str(stdout)?;
    let number = value
        .get("number")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| Error::Validation("pr view: missing number".into()))?;
    let state = value
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Validation("pr view: missing state".into()))?;
    let is_draft = value
        .get("isDraft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let checks = match value.get("statusCheckRollup") {
        None | Some(serde_json::Value::Null) => ChecksWord::Unknown,
        Some(other) => {
            let raw: Vec<RawCheck> = serde_json::from_value(other.clone()).unwrap_or_default();
            let nodes: Vec<CheckNode> = raw
                .into_iter()
                .map(|node| CheckNode {
                    conclusion: node.conclusion,
                    state: node.state,
                    status: node.status,
                })
                .collect();
            checks_word(Some(&nodes))
        }
    };
    let review = review_phrase(value.get("reviewDecision"));
    let merge = merge_phrase(value.get("mergeable"));
    let commits = value
        .get("commits")
        .and_then(|v| v.as_array())
        .map(|rows| rows.len() as u64);
    Ok(PrInfo {
        number,
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        state: display_state(state, is_draft),
        url: value
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        checks,
        review: review.to_string(),
        merge: merge.to_string(),
        changed_files: value.get("changedFiles").and_then(|v| v.as_u64()),
        additions: value.get("additions").and_then(|v| v.as_i64()),
        deletions: value.get("deletions").and_then(|v| v.as_i64()),
        commits,
        base_ref: value
            .get("baseRefName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        head_ref: value
            .get("headRefName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        head_sha: value
            .get("headRefOid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        fetched_at: String::new(),
    })
}

fn display_state(state: &str, is_draft: bool) -> String {
    if is_draft && state.eq_ignore_ascii_case("open") {
        "draft".into()
    } else {
        state.to_ascii_lowercase()
    }
}

fn review_phrase(value: Option<&serde_json::Value>) -> &'static str {
    let Some(value) = value else {
        return "review unknown";
    };
    if value.is_null() {
        return "review pending";
    }
    match value.as_str().unwrap_or("").trim() {
        "" | "REVIEW_REQUIRED" => "review pending",
        other if other.eq_ignore_ascii_case("APPROVED") => "review approved",
        other if other.eq_ignore_ascii_case("CHANGES_REQUESTED") => "review changes requested",
        _ => "review unknown",
    }
}

fn merge_phrase(value: Option<&serde_json::Value>) -> &'static str {
    let Some(text) = value.and_then(|v| v.as_str()) else {
        return "merge unknown";
    };
    if text.eq_ignore_ascii_case("MERGEABLE") {
        "merge clean"
    } else if text.eq_ignore_ascii_case("CONFLICTING") {
        "merge conflict"
    } else {
        "merge unknown"
    }
}

pub fn short_sha(sha: &str) -> &str {
    let end = sha
        .char_indices()
        .nth(12)
        .map(|(i, _)| i)
        .unwrap_or(sha.len());
    &sha[..end]
}

/// Shown when the worktree HEAD is not the PR head. The list is still
/// GitHub's, not the local checkout.
pub const LOCAL_HEAD_NOTE: &str = "列表来自 GitHub 上的这份 PR";

pub fn local_head_differs(pr_head: &str, local_head: Option<&str>) -> bool {
    match local_head {
        Some(local) if !pr_head.is_empty() && !local.is_empty() => {
            !local.eq_ignore_ascii_case(pr_head)
        }
        _ => false,
    }
}

/// `git rev-parse HEAD` in the worktree, or `None` when it cannot be read.
pub fn local_head(worktree: &Path) -> Option<String> {
    let out = CliRunner::default()
        .git(worktree, &["rev-parse", "HEAD"])
        .ok()?;
    let sha = out.stdout.trim();
    if sha.is_empty() {
        None
    } else {
        Some(sha.to_string())
    }
}

/// One file from `GET .../pulls/{n}/files`. The patch is GitHub's net
/// diff against the PR base. A missing patch stays missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrFile {
    pub filename: String,
    pub status: String,
    pub additions: i64,
    pub deletions: i64,
    pub previous_filename: Option<String>,
    pub patch: PatchBody,
}

/// What the panel can show for one file. Viva does not invent a diff when
/// GitHub omitted `patch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchBody {
    Diff(String),
    Truncated { text: String, bytes: usize },
    Absent,
}

impl PrFile {
    /// `M  src/git/pr.rs  +40 −2` or `R  old/path.rs → src/git/pr.rs  +10 −4`.
    pub fn label(&self) -> String {
        let path = match (self.status.as_str(), self.previous_filename.as_deref()) {
            ("renamed" | "copied", Some(previous)) if !previous.is_empty() => {
                format!("{previous} → {}", self.filename)
            }
            _ => self.filename.clone(),
        };
        format!(
            "{}  {path}  +{} \u{2212}{}",
            status_mark(&self.status),
            self.additions,
            self.deletions
        )
    }

    /// `(kind, text)` for the file view. `kind` is `diff`, `truncated`,
    /// or `binary`.
    pub fn view(&self) -> (&'static str, String) {
        match &self.patch {
            PatchBody::Diff(text) => ("diff", text.clone()),
            PatchBody::Truncated { text, bytes } => {
                ("truncated", format!("{text}\n… [{bytes} bytes]"))
            }
            PatchBody::Absent => ("binary", "binary — GitHub returned no patch".into()),
        }
    }
}

fn status_mark(status: &str) -> &str {
    match status {
        "added" => "A",
        "removed" => "D",
        "modified" => "M",
        "renamed" => "R",
        "copied" => "C",
        "changed" => "T",
        "unchanged" => "U",
        other => other,
    }
}

/// The file list for PR `number`, paged through the read-only `gh api`
/// allowlist. The list is GitHub's net diff: a file edited in several
/// commits is one row, and a file added then deleted is absent.
pub fn fetch_pr_files(
    repo_root: &Path,
    owner: &str,
    repo: &str,
    number: u64,
    gh_program: &str,
) -> EvidenceState<Vec<PrFile>> {
    let Some(path) = crate::git::evidence::pull_files_api_path(owner, repo, number) else {
        return EvidenceState::Unavailable {
            reason: "pull request URL is not a files API path".into(),
        };
    };
    let gh = GhRunner::with_program(gh_program);
    gh.read(repo_root, &["api", "--paginate", &path], parse_pr_files)
}

pub fn parse_pr_files(stdout: &str) -> Result<Vec<PrFile>> {
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<serde_json::Value> = serde_json::from_str(stdout)?;
    let mut files = Vec::with_capacity(rows.len());
    for row in rows {
        let filename = row
            .get("filename")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if filename.is_empty() {
            continue;
        }
        let status = row
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let patch = row.get("patch").and_then(|v| v.as_str());
        files.push(PrFile {
            filename,
            status,
            additions: row.get("additions").and_then(|v| v.as_i64()).unwrap_or(0),
            deletions: row.get("deletions").and_then(|v| v.as_i64()).unwrap_or(0),
            previous_filename: row
                .get("previous_filename")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            patch: bound_patch(patch),
        });
    }
    Ok(files)
}

fn bound_patch(patch: Option<&str>) -> PatchBody {
    let Some(patch) = patch.map(str::trim_end).filter(|text| !text.is_empty()) else {
        return PatchBody::Absent;
    };
    if patch.as_bytes().contains(&0) {
        return PatchBody::Absent;
    }
    let bytes = patch.len();
    if bytes > PR_PATCH_LIMIT {
        let text = crate::git::cli::bounded_output(patch.as_bytes(), PR_PATCH_LIMIT);
        return PatchBody::Truncated { text, bytes };
    }
    PatchBody::Diff(patch.to_string())
}

/// Keep at most [`PR_FILE_LIMIT`] rows. `total` is the real count.
pub fn page_files(mut files: Vec<PrFile>) -> (Vec<PrFile>, usize, bool) {
    let total = files.len();
    let truncated = total > PR_FILE_LIMIT;
    files.truncate(PR_FILE_LIMIT);
    (files, total, truncated)
}

/// Files cached against the PR head they were fetched for. A different
/// head drops the list; the caller fetches again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedPrFiles {
    pub head_sha: String,
    pub fetched_at: String,
    pub files: Vec<PrFile>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_gh(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("gh");
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn lookup(dir: &Path, branch: &str, script: &str) -> PrLookup {
        let gh = write_gh(dir, script);
        pr_status_for_branch_using(dir, branch, &gh.to_string_lossy())
    }

    #[test]
    fn summary_uses_morning_check_words_and_does_not_invent_counts() {
        let draft = parse_pr_view(
            &serde_json::json!({
                "number": 7,
                "title": "wip",
                "state": "OPEN",
                "isDraft": true,
                "url": "https://github.com/example/repo/pull/7",
                "mergeable": "MERGEABLE",
                "reviewDecision": "REVIEW_REQUIRED",
                "statusCheckRollup": [{
                    "conclusion": "SUCCESS",
                    "status": "COMPLETED"
                }],
                "additions": 3,
                "deletions": 1,
                "changedFiles": 1,
                "commits": [{"oid": "a"}],
                "baseRefName": "main",
                "headRefName": "feature-draft",
                "headRefOid": "abc"
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(draft.state, "draft");
        assert_eq!(draft.checks, ChecksWord::Passing);
        assert_eq!(draft.checks.failed(), Some(false));
        assert_eq!(
            draft.summary_line(),
            "#7 · draft · checks passing · review pending · merge clean · 1 files +3 \u{2212}1"
        );

        let failing = parse_pr_view(
            &serde_json::json!({
                "number": 418,
                "title": "track",
                "state": "OPEN",
                "isDraft": false,
                "url": "https://github.com/example/repo/pull/418",
                "mergeable": "CONFLICTING",
                "reviewDecision": "CHANGES_REQUESTED",
                "statusCheckRollup": [
                    {"conclusion": "FAILURE", "status": "COMPLETED"},
                    {"state": "ERROR"}
                ],
                "additions": 340,
                "deletions": 28,
                "changedFiles": 12
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(failing.checks, ChecksWord::Failing);
        assert_eq!(failing.checks.failed(), Some(true));
        assert_eq!(
            failing.summary_line(),
            "#418 · open · checks failing · review changes requested · merge conflict · 12 files +340 \u{2212}28"
        );

        let pending = parse_pr_view(
            &serde_json::json!({
                "number": 1,
                "state": "OPEN",
                "url": "https://github.com/example/repo/pull/1",
                "mergeable": "UNKNOWN",
                "reviewDecision": "",
                "statusCheckRollup": [{"status": "IN_PROGRESS", "conclusion": null}]
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(pending.checks, ChecksWord::Pending);
        assert_eq!(pending.checks.failed(), Some(false));
        assert_eq!(pending.review, "review pending");
        assert_eq!(pending.merge, "merge unknown");
        assert!(!pending.summary_line().contains("files"));

        let empty = parse_pr_view(
            &serde_json::json!({
                "number": 2,
                "state": "MERGED",
                "url": "https://github.com/example/repo/pull/2",
                "statusCheckRollup": []
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(empty.state, "merged");
        assert_eq!(empty.checks, ChecksWord::Unknown);
        assert_eq!(empty.checks.failed(), None);
        assert_eq!(empty.review, "review unknown");

        for token in ["TIMED_OUT", "STARTUP_FAILURE", "ERROR"] {
            let word = checks_word(Some(&[CheckNode {
                conclusion: Some(token.into()),
                state: None,
                status: Some("COMPLETED".into()),
            }]));
            assert_eq!(word, ChecksWord::Failing, "{token}");
        }
    }

    #[test]
    fn gh_misses_stay_distinct_and_do_not_guess_a_pull_request() {
        let dir = tempfile::tempdir().unwrap();
        let no_pr = lookup(
            dir.path(),
            "topic",
            "#!/bin/sh\necho 'no pull requests found for branch \"topic\"' >&2\nexit 1\n",
        );
        assert_eq!(no_pr, PrLookup::NoPr);
        assert_eq!(no_pr.summary_line(), "no pr");

        let missing = dir.path().join("missing-gh");
        let not_installed =
            pr_status_for_branch_using(dir.path(), "topic", &missing.to_string_lossy());
        match not_installed {
            PrLookup::Unknown { reason } => assert_eq!(reason, "gh is not installed"),
            other => panic!("missing gh must be unknown, got {other:?}"),
        }

        let logged_out = lookup(
            dir.path(),
            "topic",
            "#!/bin/sh\necho 'HTTP 401: Bad credentials (gh auth login)' >&2\nexit 1\n",
        );
        assert_eq!(logged_out.summary_line(), "unknown · gh is not logged in");

        let offline = lookup(
            dir.path(),
            "topic",
            "#!/bin/sh\necho 'dial tcp: connection refused' >&2\nexit 1\n",
        );
        assert_eq!(offline.summary_line(), "unknown · network failed");

        let other = lookup(
            dir.path(),
            "topic",
            "#!/bin/sh\necho 'gh exploded' >&2\nexit 1\n",
        );
        match other {
            PrLookup::Unknown { reason } => {
                assert!(reason.contains("gh exploded"), "{reason}");
                assert!(!reason.to_lowercase().contains("no pr"));
            }
            PrLookup::NoPr => panic!("an unexplained failure is not 'no pr'"),
            PrLookup::Found(_) => panic!("an unexplained failure is not a pull request"),
        }
    }

    #[test]
    fn fake_gh_locks_draft_failing_checks_and_a_merge_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let draft = lookup(
            dir.path(),
            "feature-draft",
            r#"#!/bin/sh
echo '{"number":7,"title":"wip","state":"OPEN","isDraft":true,"url":"https://github.com/example/repo/pull/7","mergeable":"MERGEABLE","reviewDecision":"REVIEW_REQUIRED","statusCheckRollup":[{"conclusion":"SUCCESS","status":"COMPLETED"}],"additions":3,"deletions":1,"changedFiles":1}'
"#,
        );
        match draft {
            PrLookup::Found(info) => {
                assert_eq!(info.state, "draft");
                assert!(info.summary_line().contains("checks passing"));
                assert!(info.summary_line().contains("merge clean"));
            }
            other => panic!("draft payload must be a pull request, got {other:?}"),
        }

        let conflict = lookup(
            dir.path(),
            "feature-conflict",
            r#"#!/bin/sh
echo '{"number":9,"title":"conflict","state":"OPEN","isDraft":false,"url":"https://github.com/example/repo/pull/9","mergeable":"CONFLICTING","reviewDecision":"CHANGES_REQUESTED","statusCheckRollup":[{"conclusion":"FAILURE","status":"COMPLETED"},{"state":"ERROR"}],"additions":4,"deletions":1,"changedFiles":1}'
"#,
        );
        match conflict {
            PrLookup::Found(info) => {
                assert_eq!(info.checks, ChecksWord::Failing);
                assert!(info.summary_line().contains("checks failing"));
                assert!(info.summary_line().contains("merge conflict"));
                assert!(info.summary_line().contains("review changes requested"));
            }
            other => panic!("conflict payload must be a pull request, got {other:?}"),
        }
    }

    #[test]
    fn files_are_the_net_github_list_with_rename_and_bounds() {
        let patch = format!("@@ -1 +1 @@\n-{}\n+final\n", "x".repeat(PR_PATCH_LIMIT));
        let stdout = serde_json::json!([
            {
                "filename": "src/git/pr.rs",
                "status": "modified",
                "additions": 40,
                "deletions": 2,
                "patch": "@@ -1 +1 @@\n-old\n+final\n"
            },
            {
                "filename": "src/git/pr.rs",
                "status": "modified",
                "additions": 1,
                "deletions": 1,
                "patch": "@@\n+intermediate\n"
            },
            {
                "filename": "src/new.rs",
                "status": "renamed",
                "additions": 10,
                "deletions": 4,
                "previous_filename": "old/path.rs",
                "patch": "@@\n+renamed\n"
            },
            {
                "filename": "img.bin",
                "status": "added",
                "additions": 0,
                "deletions": 0
            },
            {
                "filename": "big.diff",
                "status": "modified",
                "additions": 1,
                "deletions": 0,
                "patch": patch
            }
        ])
        .to_string();
        // The fixture itself lists the same path twice, which GitHub's
        // net diff does not. Parsing must not collapse or invent rows:
        // one input object stays one row, and a path that is not in the
        // payload (added, then deleted) never appears.
        let files = parse_pr_files(&stdout).unwrap();
        assert_eq!(files.len(), 5);
        assert!(files.iter().all(|file| file.filename != "gone.txt"));
        assert_eq!(files[0].label(), "M  src/git/pr.rs  +40 \u{2212}2");
        assert!(files[0].view().1.contains("+final"));
        assert!(!files[0].view().1.contains("+intermediate"));
        assert_eq!(
            files[2].label(),
            "R  old/path.rs → src/new.rs  +10 \u{2212}4"
        );
        assert_eq!(files[3].view().0, "binary");
        assert!(files[3].view().1.contains("binary"));
        assert!(!files[3].view().1.contains("@@"));
        assert_eq!(files[4].view().0, "truncated");
        assert!(files[4].view().1.contains("bytes"));

        let many = (0..PR_FILE_LIMIT + 1)
            .map(|i| PrFile {
                filename: format!("f{i}"),
                status: "modified".into(),
                additions: 1,
                deletions: 0,
                previous_filename: None,
                patch: PatchBody::Absent,
            })
            .collect();
        let (page, total, truncated) = page_files(many);
        assert_eq!(page.len(), PR_FILE_LIMIT);
        assert_eq!(total, PR_FILE_LIMIT + 1);
        assert!(truncated);
    }

    #[test]
    fn fetch_uses_the_files_get_and_rejects_a_write_shaped_program() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = api ]; then echo '[]'; exit 0; fi\nexit 1\n",
            log.display()
        );
        let gh = write_gh(dir.path(), &script);
        let state = fetch_pr_files(dir.path(), "example", "repo", 418, &gh.to_string_lossy());
        match state {
            EvidenceState::Fetched { payload, .. } => assert!(payload.is_empty()),
            EvidenceState::Unavailable { reason } => panic!("{reason}"),
        }
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("api --paginate repos/example/repo/pulls/418/files"),
            "{text}"
        );
        assert!(!text.contains("merge"));
        assert!(!text.contains("comment"));
        assert!(!text.contains("review"));
    }
}
