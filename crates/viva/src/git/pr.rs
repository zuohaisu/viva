//! PR status via the GitHub CLI (V15-2): `gh` is the proven capability
//! (charter Architecture Order 1); Viva wraps it read-only and treats any
//! failure as "unknown" — a missing gh, no auth, or no PR for the branch
//! all render as an honest unknown instead of a guess.

use std::path::Path;

use serde::Serialize;

use crate::git::cli::CliRunner;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrInfo {
    pub number: u64,
    /// gh's own vocabulary, lowercased: `open` | `merged` | `closed`.
    pub state: String,
    pub url: String,
}

/// The PR state of a branch, or `None` when unknown (gh missing, auth
/// missing, network down, or no PR exists for the branch). Callers must
/// display unknown as unknown.
pub fn pr_status_for_branch(repo_root: &Path, branch: &str) -> Option<PrInfo> {
    let runner = CliRunner::default();
    let out = runner
        .run(
            "gh",
            repo_root,
            &["pr", "view", branch, "--json", "state,number,url"],
        )
        .ok()?;
    if !out.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&out.stdout).ok()?;
    Some(PrInfo {
        number: value.get("number")?.as_u64()?,
        state: value.get("state")?.as_str()?.to_ascii_lowercase(),
        url: value.get("url")?.as_str()?.to_string(),
    })
}
