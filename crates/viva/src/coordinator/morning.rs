//! Morning progress rollup: waiting merge, blocked signals, multi-round hints.

use serde::{Deserialize, Serialize};
use std::path::Path;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::foundation::error::{Error, Result};
use crate::foundation::ids::utc_now;
use crate::foundation::store::Store;
use crate::git::evidence::{EvidenceState, GhRunner};
use crate::git::worktrees::{ProtectedRefs, TaskWorktreeRecord, WorktreeService};
use crate::tasks::{TaskRecord, TaskRegistry, TaskStatus};

/// Caller-supplied or default reporting window (local calendar day bounds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MorningOptions {
    pub window_start: String,
    pub window_end: String,
    pub tz_offset_seconds: i32,
    #[serde(skip)]
    pub gh_program: Option<String>,
}

impl MorningOptions {
    pub fn parse_window(start: &str, end: &str, tz_offset_seconds: i32) -> Result<Self> {
        let _ = OffsetDateTime::parse(start, &Rfc3339)
            .map_err(|e| Error::Validation(format!("--since `{start}` is not RFC3339: {e}")))?;
        let _ = OffsetDateTime::parse(end, &Rfc3339)
            .map_err(|e| Error::Validation(format!("--until `{end}` is not RFC3339: {e}")))?;
        Ok(Self {
            window_start: start.to_string(),
            window_end: end.to_string(),
            tz_offset_seconds,
            gh_program: None,
        })
    }

    pub fn with_gh_program(mut self, program: impl Into<String>) -> Self {
        self.gh_program = Some(program.into());
        self
    }
}

/// Best-effort local UTC offset for the host (seconds east of UTC).
pub fn local_tz_offset_seconds() -> i32 {
    UtcOffset::current_local_offset()
        .map(|o| o.whole_seconds())
        .unwrap_or(0)
}

/// Default window: the previous local natural day `[00:00, next 00:00)`.
pub fn default_previous_local_day(now: OffsetDateTime, tz: UtcOffset) -> Result<(String, String)> {
    let local = now.to_offset(tz);
    let today = local.date();
    let yesterday = today
        .previous_day()
        .ok_or_else(|| Error::Validation("cannot compute previous local day".into()))?;
    let start = PrimitiveDateTime::new(yesterday, Time::MIDNIGHT)
        .assume_offset(tz)
        .format(&Rfc3339)
        .map_err(|e| Error::Validation(format!("window start format failed: {e}")))?;
    let end = PrimitiveDateTime::new(today, Time::MIDNIGHT)
        .assume_offset(tz)
        .format(&Rfc3339)
        .map_err(|e| Error::Validation(format!("window end format failed: {e}")))?;
    Ok((start, end))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiRoundSignal {
    Yes { detail: String },
    No,
    Unknown { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MorningItem {
    pub task_id: String,
    pub goal: String,
    pub viva_status: String,
    pub waiting_merge: Option<WaitingMerge>,
    pub blocked: BlockedSignals,
    pub multi_round: MultiRoundSignal,
    pub github: GithubSection,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitingMerge {
    Open { pr_number: u64, url: Option<String> },
    Draft { pr_number: u64, url: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockedSignals {
    pub viva_task_blocked: bool,
    pub github_checks_failed: Option<bool>,
    pub merge_conflict: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GithubSection {
    Available {
        branch: Option<String>,
        pr_number: Option<u64>,
    },
    Blocked {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MorningReport {
    pub generated_at: String,
    pub window: MorningOptions,
    pub github_auth: GithubAuth,
    pub items: Vec<MorningItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GithubAuth {
    Available,
    Blocked { reason: String },
}

pub struct MorningRollup<'a> {
    store: &'a Store,
    gh: GhRunner,
    options: MorningOptions,
}

impl<'a> MorningRollup<'a> {
    pub fn new(store: &'a Store, options: MorningOptions) -> Self {
        let gh = options
            .gh_program
            .as_deref()
            .map(GhRunner::with_program)
            .unwrap_or_default();
        Self { store, gh, options }
    }

    pub fn build(&self) -> Result<MorningReport> {
        let tasks = TaskRegistry::new(self.store).list_tasks(0, 10_000)?;
        let github_auth = self.github_auth_state();
        let mut items = Vec::new();
        for task in tasks {
            if matches!(task.status, TaskStatus::Done | TaskStatus::Cancelled) {
                continue;
            }
            if let Some(item) = self.item_for_task(&task, &github_auth)? {
                items.push(item);
            }
        }
        Ok(MorningReport {
            generated_at: utc_now(),
            window: MorningOptions {
                gh_program: self.options.gh_program.clone(),
                ..self.options.clone()
            },
            github_auth,
            items,
        })
    }

    fn github_auth_state(&self) -> GithubAuth {
        let cwd = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
        if self.gh.auth_available(&cwd) {
            GithubAuth::Available
        } else {
            GithubAuth::Blocked {
                reason: "gh authentication missing or unavailable in this environment".into(),
            }
        }
    }

    fn item_for_task(&self, task: &TaskRecord, auth: &GithubAuth) -> Result<Option<MorningItem>> {
        let worktree = WorktreeService::new(self.store, ProtectedRefs::new(vec![]))
            .active_for_task(&task.task_id)?;
        let github = match (auth, &worktree) {
            (GithubAuth::Available, Some(wt)) => self.github_for_worktree(wt)?,
            (GithubAuth::Available, None) => GithubSection::Available {
                branch: None,
                pr_number: None,
            },
            (GithubAuth::Blocked { reason }, _) => GithubSection::Blocked {
                reason: reason.clone(),
            },
        };

        let (waiting_merge, checks_failed, merge_conflict, pr_number) =
            match (&github, &worktree, auth) {
                (GithubSection::Available { .. }, Some(wt), GithubAuth::Available) => {
                    self.classify_pr(wt)?
                }
                _ => (None, None, None, None),
            };

        let blocked = BlockedSignals {
            viva_task_blocked: task.status == TaskStatus::Blocked,
            github_checks_failed: checks_failed,
            merge_conflict,
        };

        let multi_round = match (auth, pr_number, &worktree) {
            (GithubAuth::Available, Some(n), Some(wt)) => self.multi_round_for_pr(wt, n)?,
            (GithubAuth::Blocked { reason }, _, _) => MultiRoundSignal::Unknown {
                reason: format!("github blocked: {reason}"),
            },
            _ => MultiRoundSignal::Unknown {
                reason: "no open pull request linked to this task".into(),
            },
        };

        let interesting = waiting_merge.is_some()
            || blocked.viva_task_blocked
            || blocked.github_checks_failed == Some(true)
            || blocked.merge_conflict == Some(true)
            || !matches!(multi_round, MultiRoundSignal::No);
        if !interesting && !matches!(task.status, TaskStatus::InProgress | TaskStatus::Open) {
            return Ok(None);
        }

        Ok(Some(MorningItem {
            task_id: task.task_id.to_string(),
            goal: task.goal.clone(),
            viva_status: task.status.as_str().to_string(),
            waiting_merge,
            blocked,
            multi_round,
            github,
        }))
    }

    fn github_for_worktree(&self, wt: &TaskWorktreeRecord) -> Result<GithubSection> {
        Ok(GithubSection::Available {
            branch: Some(wt.branch.clone()),
            pr_number: pr_number_for_branch(&self.gh, &wt.worktree_path, &wt.branch),
        })
    }

    #[allow(clippy::type_complexity)]
    fn classify_pr(
        &self,
        wt: &TaskWorktreeRecord,
    ) -> Result<(
        Option<WaitingMerge>,
        Option<bool>,
        Option<bool>,
        Option<u64>,
    )> {
        let Some(number) = pr_number_for_branch(&self.gh, &wt.worktree_path, &wt.branch) else {
            return Ok((None, None, None, None));
        };
        let detail = fetch_pr_detail(&self.gh, &wt.worktree_path, number);
        match detail {
            EvidenceState::Unavailable { .. } => Ok((None, None, None, Some(number))),
            EvidenceState::Fetched { payload, .. } => {
                let waiting = if payload.state.eq_ignore_ascii_case("open") {
                    if payload.is_draft {
                        Some(WaitingMerge::Draft {
                            pr_number: number,
                            url: payload.url,
                        })
                    } else {
                        Some(WaitingMerge::Open {
                            pr_number: number,
                            url: payload.url,
                        })
                    }
                } else {
                    None
                };
                Ok((
                    waiting,
                    payload.checks_failed,
                    payload.merge_conflict,
                    Some(number),
                ))
            }
        }
    }

    fn multi_round_for_pr(
        &self,
        wt: &TaskWorktreeRecord,
        pr_number: u64,
    ) -> Result<MultiRoundSignal> {
        let timeline = fetch_pr_timeline(&self.gh, &wt.worktree_path, pr_number);
        let (start, end) = (
            self.options.window_start.as_str(),
            self.options.window_end.as_str(),
        );
        let payload = match timeline {
            EvidenceState::Unavailable { reason } => {
                return Ok(MultiRoundSignal::Unknown { reason });
            }
            EvidenceState::Fetched { payload, .. } => payload,
        };
        let comments = match payload.repo.as_ref() {
            Some((owner, repo)) => {
                match fetch_review_comments(&self.gh, &wt.worktree_path, owner, repo, pr_number) {
                    EvidenceState::Fetched { payload: rows, .. } => Some(
                        count_non_author_review_comments(&rows, &payload.pr_author, start, end),
                    ),
                    EvidenceState::Unavailable { .. } => None,
                }
            }
            None => None,
        };
        Ok(classify_multi_round(
            &payload.reviews,
            &payload.commits,
            comments,
            start,
            end,
        ))
    }
}

/// Check conclusions that mean the check failed. `conclusion` is a CheckRun
/// field; `state` is a StatusContext field. Real `gh pr view` returns
/// `statusCheckRollup` as an array of those nodes, not a `{state}` object.
const CHECK_FAILURES: &[&str] = &["FAILURE", "ERROR", "TIMED_OUT", "STARTUP_FAILURE"];

#[derive(Debug, Deserialize)]
struct PrDetail {
    state: String,
    #[serde(default, rename = "isDraft")]
    is_draft: bool,
    url: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Option<Vec<StatusCheck>>,
}

#[derive(Debug, Deserialize)]
struct StatusCheck {
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReviewRow {
    #[serde(default)]
    state: String,
    #[serde(default, rename = "submittedAt")]
    submitted_at: String,
}

#[derive(Debug, Deserialize)]
struct GhUser {
    #[serde(default)]
    login: String,
}

/// One inline review comment from `gh api repos/.../pulls/{n}/comments`.
#[derive(Debug, Deserialize)]
struct ReviewComment {
    #[serde(default, rename = "created_at")]
    created_at: String,
    #[serde(default)]
    user: Option<GhUser>,
}

#[derive(Debug)]
struct PrDetailParsed {
    state: String,
    is_draft: bool,
    url: Option<String>,
    checks_failed: Option<bool>,
    merge_conflict: Option<bool>,
}

fn pr_number_for_branch(gh: &GhRunner, cwd: &Path, branch: &str) -> Option<u64> {
    let state = gh.read(
        cwd,
        &[
            "pr",
            "view",
            branch,
            "--json",
            "number,state,url,isDraft,mergeable,statusCheckRollup",
        ],
        |stdout| {
            let value: serde_json::Value = serde_json::from_str(stdout)?;
            value
                .get("number")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| Error::Validation("pr view: missing number".into()))
        },
    );
    match state {
        EvidenceState::Fetched { payload, .. } => Some(payload),
        EvidenceState::Unavailable { .. } => None,
    }
}

fn fetch_pr_detail(gh: &GhRunner, cwd: &Path, number: u64) -> EvidenceState<PrDetailParsed> {
    gh.read(
        cwd,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "state,url,isDraft,mergeable,statusCheckRollup",
        ],
        parse_pr_detail,
    )
}

fn parse_pr_detail(stdout: &str) -> Result<PrDetailParsed> {
    let raw: PrDetail = serde_json::from_str(stdout)?;
    Ok(PrDetailParsed {
        state: raw.state,
        is_draft: raw.is_draft,
        url: raw.url,
        checks_failed: checks_failed_from(raw.status_check_rollup.as_deref()),
        merge_conflict: merge_conflict_from(raw.mergeable.as_deref()),
    })
}

/// Empty or missing rollup is unknown. Any failed conclusion/state is a
/// failure; a non-empty rollup with none of those is not a failure
/// (pending checks have not failed).
fn checks_failed_from(checks: Option<&[StatusCheck]>) -> Option<bool> {
    let checks = checks?;
    if checks.is_empty() {
        return None;
    }
    Some(checks.iter().any(|check| {
        check_failed(check.conclusion.as_deref()) || check_failed(check.state.as_deref())
    }))
}

fn check_failed(value: Option<&str>) -> bool {
    value
        .map(|token| {
            CHECK_FAILURES
                .iter()
                .any(|failure| token.eq_ignore_ascii_case(failure))
        })
        .unwrap_or(false)
}

fn merge_conflict_from(mergeable: Option<&str>) -> Option<bool> {
    match mergeable {
        Some(value) if value.eq_ignore_ascii_case("CONFLICTING") => Some(true),
        Some(value) if value.eq_ignore_ascii_case("MERGEABLE") => Some(false),
        _ => None,
    }
}

#[derive(Debug)]
struct PrTimeline {
    pr_author: String,
    repo: Option<(String, String)>,
    reviews: Vec<ReviewRow>,
    commits: Vec<CommitRow>,
}

fn fetch_pr_timeline(gh: &GhRunner, cwd: &Path, number: u64) -> EvidenceState<PrTimeline> {
    gh.read(
        cwd,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "author,reviews,commits,url",
        ],
        parse_pr_timeline,
    )
}

fn parse_pr_timeline(stdout: &str) -> Result<PrTimeline> {
    let value: serde_json::Value = serde_json::from_str(stdout)?;
    let pr_author = value
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or_default()
        .to_string();
    let repo = value
        .get("url")
        .and_then(|u| u.as_str())
        .and_then(repo_from_pr_url);
    let reviews = value
        .get("reviews")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    let commits = value
        .get("commits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect();
    Ok(PrTimeline {
        pr_author,
        repo,
        reviews,
        commits,
    })
}

fn repo_from_pr_url(url: &str) -> Option<(String, String)> {
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
    Some((owner.to_string(), repo.to_string()))
}

fn fetch_review_comments(
    gh: &GhRunner,
    cwd: &Path,
    owner: &str,
    repo: &str,
    number: u64,
) -> EvidenceState<Vec<ReviewComment>> {
    let Some(path) = crate::git::evidence::pull_review_comments_api_path(owner, repo, number)
    else {
        return EvidenceState::Unavailable {
            reason: "pull request URL is not a review-comment API path".into(),
        };
    };
    gh.read(cwd, &["api", "--paginate", &path], |stdout| {
        if stdout.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(stdout)?)
    })
}

fn count_non_author_review_comments(
    comments: &[ReviewComment],
    pr_author: &str,
    start: &str,
    end: &str,
) -> usize {
    comments
        .iter()
        .filter(|comment| in_window(&comment.created_at, start, end))
        .filter(|comment| {
            let login = comment
                .user
                .as_ref()
                .map(|user| user.login.as_str())
                .unwrap_or("");
            pr_author.is_empty() || login != pr_author
        })
        .count()
}

/// `comments` is `None` when the review-comment list could not be read.
/// Three request→push cycles are enough on their own. Otherwise a missing
/// comment count stays unknown instead of being treated as zero.
fn classify_multi_round(
    reviews: &[ReviewRow],
    commits: &[CommitRow],
    comments: Option<usize>,
    start: &str,
    end: &str,
) -> MultiRoundSignal {
    let push_cycles = count_request_push_cycles(reviews, commits, start, end);
    let change_requests_in_window = reviews
        .iter()
        .filter(|review| review.state.eq_ignore_ascii_case("CHANGES_REQUESTED"))
        .filter(|review| in_window(&review.submitted_at, start, end))
        .count();
    let comment_count = comments.unwrap_or(0);
    if push_cycles >= 3 || comments.is_some_and(|count| count >= 3) {
        let shown = match comments {
            Some(count) => count.to_string(),
            None => "unknown".to_string(),
        };
        return MultiRoundSignal::Yes {
            detail: format!(
                "request_push_cycles_in_window={push_cycles}, \
                 non_author_review_comments_in_window={shown}"
            ),
        };
    }
    if comments.is_none() {
        return MultiRoundSignal::Unknown {
            reason: "could not read pull request review comments".into(),
        };
    }
    if change_requests_in_window > 0 && commits.is_empty() && comment_count < 3 {
        return MultiRoundSignal::Unknown {
            reason: "could not observe push events to pair with change requests".into(),
        };
    }
    MultiRoundSignal::No
}

#[derive(Debug, Deserialize)]
struct CommitRow {
    #[serde(default, rename = "committedDate")]
    committed_date: String,
}

/// Count paired change-request → push cycles in the window.
fn count_request_push_cycles(
    reviews: &[ReviewRow],
    commits: &[CommitRow],
    start: &str,
    end: &str,
) -> usize {
    let mut requests: Vec<OffsetDateTime> = reviews
        .iter()
        .filter(|r| r.state.eq_ignore_ascii_case("CHANGES_REQUESTED"))
        .filter_map(|r| OffsetDateTime::parse(&r.submitted_at, &Rfc3339).ok())
        .filter(|t| in_window_ts(t, start, end))
        .collect();
    requests.sort();
    let mut pushes: Vec<OffsetDateTime> = commits
        .iter()
        .filter_map(|c| OffsetDateTime::parse(&c.committed_date, &Rfc3339).ok())
        .filter(|t| in_window_ts(t, start, end))
        .collect();
    pushes.sort();
    let mut cycles = 0usize;
    let mut push_idx = 0usize;
    for req in requests {
        while push_idx < pushes.len() && pushes[push_idx] <= req {
            push_idx += 1;
        }
        if push_idx < pushes.len() {
            cycles += 1;
            push_idx += 1;
        }
    }
    cycles
}

fn in_window_ts(at: &OffsetDateTime, start: &str, end: &str) -> bool {
    let Ok(start) = OffsetDateTime::parse(start, &Rfc3339) else {
        return false;
    };
    let Ok(end) = OffsetDateTime::parse(end, &Rfc3339) else {
        return false;
    };
    *at >= start && *at < end
}

fn in_window(ts: &str, start: &str, end: &str) -> bool {
    let Ok(at) = OffsetDateTime::parse(ts, &Rfc3339) else {
        return false;
    };
    let Ok(start) = OffsetDateTime::parse(start, &Rfc3339) else {
        return false;
    };
    let Ok(end) = OffsetDateTime::parse(end, &Rfc3339) else {
        return false;
    };
    at >= start && at < end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(value: serde_json::Value) -> PrDetailParsed {
        parse_pr_detail(&value.to_string()).expect("parse pr detail")
    }

    #[test]
    fn status_check_rollup_array_reads_conclusion_and_state() {
        let success = parsed(serde_json::json!({
            "state": "OPEN",
            "isDraft": false,
            "url": "https://github.com/example/repo/pull/42",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{
                "__typename": "CheckRun",
                "conclusion": "SUCCESS",
                "status": "COMPLETED",
                "name": "rust",
                "startedAt": "2026-10-15T11:00:00Z",
                "completedAt": "2026-10-15T12:00:00Z",
                "detailsUrl": "https://example/checks/rust",
                "workflowName": "CI"
            }]
        }));
        assert_eq!(success.checks_failed, Some(false));
        assert_eq!(success.merge_conflict, Some(false));
        assert!(!success.is_draft);

        let failed = parsed(serde_json::json!({
            "state": "OPEN",
            "mergeable": "CONFLICTING",
            "statusCheckRollup": [
                {
                    "__typename": "CheckRun",
                    "conclusion": "FAILURE",
                    "status": "COMPLETED",
                    "name": "rust"
                },
                {"__typename": "StatusContext", "state": "ERROR", "context": "ci"}
            ]
        }));
        assert_eq!(failed.checks_failed, Some(true));
        assert_eq!(failed.merge_conflict, Some(true));

        let timed_out = parsed(serde_json::json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "TIMED_OUT", "status": "COMPLETED"}]
        }));
        assert_eq!(timed_out.checks_failed, Some(true));

        let startup = parsed(serde_json::json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "STARTUP_FAILURE", "status": "COMPLETED"}]
        }));
        assert_eq!(startup.checks_failed, Some(true));
    }

    #[test]
    fn empty_rollup_is_unknown_and_the_old_object_shape_does_not_parse() {
        let empty = parsed(serde_json::json!({
            "state": "OPEN",
            "mergeable": "UNKNOWN",
            "statusCheckRollup": []
        }));
        assert_eq!(empty.checks_failed, None);
        assert_eq!(empty.merge_conflict, None);

        let missing = parsed(serde_json::json!({
            "state": "OPEN",
            "statusCheckRollup": null
        }));
        assert_eq!(missing.checks_failed, None);

        let pending = parsed(serde_json::json!({
            "state": "OPEN",
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{
                "conclusion": null,
                "status": "IN_PROGRESS",
                "name": "rust"
            }]
        }));
        assert_eq!(pending.checks_failed, Some(false));

        let object = serde_json::json!({
            "state": "OPEN",
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": {"state": "SUCCESS"}
        });
        assert!(
            parse_pr_detail(&object.to_string()).is_err(),
            "gh does not return statusCheckRollup as an object"
        );
    }

    #[test]
    fn review_comment_count_skips_the_author_and_a_missing_read_stays_unknown() {
        let start = "2026-10-15T00:00:00Z";
        let end = "2026-10-16T00:00:00Z";
        let comments: Vec<ReviewComment> = serde_json::from_value(serde_json::json!([
            {"user": {"login": "reviewer"}, "created_at": "2026-10-15T10:00:00Z", "body": "a"},
            {"user": {"login": "reviewer"}, "created_at": "2026-10-15T11:00:00Z", "body": "b"},
            {"user": {"login": "reviewer"}, "created_at": "2026-10-15T12:00:00Z", "body": "c"},
            {"user": {"login": "dev"}, "created_at": "2026-10-15T12:30:00Z", "body": "self"},
            {"user": {"login": "reviewer"}, "created_at": "2026-10-14T12:00:00Z", "body": "old"}
        ]))
        .expect("comments");
        assert_eq!(
            count_non_author_review_comments(&comments, "dev", start, end),
            3
        );

        assert!(matches!(
            classify_multi_round(&[], &[], Some(3), start, end),
            MultiRoundSignal::Yes { .. }
        ));
        assert!(matches!(
            classify_multi_round(&[], &[], None, start, end),
            MultiRoundSignal::Unknown { .. }
        ));
        assert!(matches!(
            classify_multi_round(
                &[
                    review("CHANGES_REQUESTED", "2026-10-15T10:00:00Z"),
                    review("CHANGES_REQUESTED", "2026-10-15T11:00:00Z"),
                    review("CHANGES_REQUESTED", "2026-10-15T12:00:00Z"),
                ],
                &[
                    commit("2026-10-15T10:30:00Z"),
                    commit("2026-10-15T11:30:00Z"),
                    commit("2026-10-15T12:30:00Z"),
                ],
                None,
                start,
                end,
            ),
            MultiRoundSignal::Yes { .. }
        ));
    }

    #[test]
    fn timeline_parses_without_a_review_comments_field() {
        let raw = serde_json::json!({
            "author": {"login": "dev"},
            "url": "https://github.com/example/repo/pull/42",
            "reviews": [{
                "author": {"login": "reviewer"},
                "state": "COMMENTED",
                "submittedAt": "2026-10-15T10:00:00Z"
            }],
            "commits": [{"oid": "abc", "committedDate": "2026-10-15T10:30:00Z"}]
        });
        let timeline = parse_pr_timeline(&raw.to_string()).expect("timeline");
        assert_eq!(timeline.pr_author, "dev");
        assert_eq!(
            timeline.repo,
            Some(("example".to_string(), "repo".to_string()))
        );
        assert_eq!(timeline.reviews.len(), 1);
        assert_eq!(timeline.commits.len(), 1);
    }

    #[test]
    fn live_gh_accepts_the_open_pull_request_contract() {
        let gh = crate::git::evidence::GhRunner::default();
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !gh.auth_available(&root) {
            eprintln!("live gh contract not executed: gh auth unavailable");
            return;
        }
        match fetch_pr_detail(&gh, &root, 163) {
            EvidenceState::Fetched { payload, .. } => {
                assert!(
                    !payload.state.is_empty(),
                    "real gh pr view returned a state"
                );
                assert!(
                    payload.checks_failed.is_some(),
                    "statusCheckRollup array must become a boolean"
                );
            }
            EvidenceState::Unavailable { reason } => {
                panic!("real gh pr view did not parse: {reason}");
            }
        }
        let timeline = match fetch_pr_timeline(&gh, &root, 163) {
            EvidenceState::Fetched { payload, .. } => payload,
            EvidenceState::Unavailable { reason } => {
                panic!("real gh timeline did not parse: {reason}");
            }
        };
        let (owner, repo) = timeline
            .repo
            .expect("real pr view url should identify owner/repo");
        match fetch_review_comments(&gh, &root, &owner, &repo, 163) {
            EvidenceState::Fetched { .. } => {}
            EvidenceState::Unavailable { reason } => {
                panic!("real gh api review comments did not parse: {reason}");
            }
        }
    }

    fn review(state: &str, at: &str) -> ReviewRow {
        ReviewRow {
            state: state.to_string(),
            submitted_at: at.to_string(),
        }
    }

    fn commit(at: &str) -> CommitRow {
        CommitRow {
            committed_date: at.to_string(),
        }
    }
}
