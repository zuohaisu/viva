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
        let reviews = fetch_pr_reviews(&self.gh, &wt.worktree_path, pr_number);
        let comments = fetch_pr_review_comments(&self.gh, &wt.worktree_path, pr_number);
        let (start, end) = (
            self.options.window_start.as_str(),
            self.options.window_end.as_str(),
        );
        match (reviews, comments) {
            (EvidenceState::Unavailable { reason }, _)
            | (_, EvidenceState::Unavailable { reason }) => {
                Ok(MultiRoundSignal::Unknown { reason })
            }
            (
                EvidenceState::Fetched {
                    payload: reviews, ..
                },
                EvidenceState::Fetched {
                    payload: comments, ..
                },
            ) => {
                let change_requests = reviews
                    .iter()
                    .filter(|r| r.state.eq_ignore_ascii_case("CHANGES_REQUESTED"))
                    .filter(|r| in_window(&r.submitted_at, start, end))
                    .count();
                let non_author_comments = comments
                    .iter()
                    .filter(|c| !c.author_is_pr_author)
                    .filter(|c| in_window(&c.created_at, start, end))
                    .count();
                if change_requests >= 3 || non_author_comments >= 3 {
                    Ok(MultiRoundSignal::Yes {
                        detail: format!(
                            "change_requests_in_window={change_requests}, \
                             non_author_review_comments_in_window={non_author_comments}"
                        ),
                    })
                } else {
                    Ok(MultiRoundSignal::No)
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct PrDetail {
    state: String,
    #[serde(default, rename = "isDraft")]
    is_draft: bool,
    url: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    status_check_rollup: Option<StatusCheckRollup>,
}

#[derive(Debug, Deserialize)]
struct StatusCheckRollup {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReviewRow {
    state: String,
    #[serde(default, rename = "submittedAt")]
    submitted_at: String,
}

#[derive(Debug, Deserialize)]
struct CommentRow {
    #[serde(default, rename = "createdAt")]
    created_at: String,
    author: CommentAuthor,
}

#[derive(Debug, Deserialize)]
struct CommentAuthor {
    login: String,
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
        |stdout| {
            let raw: PrDetail = serde_json::from_str(stdout)?;
            let checks_failed = raw.status_check_rollup.as_ref().and_then(|r| {
                r.state
                    .as_deref()
                    .map(|s| s.eq_ignore_ascii_case("FAILURE"))
            });
            let merge_conflict = raw
                .mergeable
                .as_deref()
                .map(|m| m.eq_ignore_ascii_case("CONFLICTING"));
            Ok(PrDetailParsed {
                state: raw.state,
                is_draft: raw.is_draft,
                url: raw.url,
                checks_failed,
                merge_conflict,
            })
        },
    )
}

fn fetch_pr_reviews(gh: &GhRunner, cwd: &Path, number: u64) -> EvidenceState<Vec<ReviewRow>> {
    gh.read(
        cwd,
        &["pr", "view", &number.to_string(), "--json", "reviews"],
        |stdout| {
            let value: serde_json::Value = serde_json::from_str(stdout)?;
            let rows = value
                .get("reviews")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            Ok(rows
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect())
        },
    )
}

fn fetch_pr_review_comments(
    gh: &GhRunner,
    cwd: &Path,
    number: u64,
) -> EvidenceState<Vec<CommentRowWithAuthorFlag>> {
    gh.read(
        cwd,
        &[
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "author,comments",
        ],
        |stdout| {
            let value: serde_json::Value = serde_json::from_str(stdout)?;
            let author = value
                .get("author")
                .and_then(|a| a.get("login"))
                .and_then(|l| l.as_str())
                .unwrap_or_default()
                .to_string();
            let comments = value
                .get("comments")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            Ok(comments
                .into_iter()
                .filter_map(|v| {
                    let row: CommentRow = serde_json::from_value(v).ok()?;
                    Some(CommentRowWithAuthorFlag {
                        created_at: row.created_at,
                        author_is_pr_author: row.author.login == author,
                    })
                })
                .collect())
        },
    )
}

#[derive(Debug)]
struct CommentRowWithAuthorFlag {
    created_at: String,
    author_is_pr_author: bool,
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
