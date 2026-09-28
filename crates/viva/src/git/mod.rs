//! Git/worktree isolation and GitHub evidence linkage (V08, issue #17).
//!
//! - [`cli`]: argv-array git/gh execution with timeouts, bounded capture
//!   and actionable failure classification. Paths with spaces work; there
//!   is no shell string anywhere.
//! - [`worktrees`]: task worktree creation from the latest remote default
//!   ref, Git-native discovery, explicit adoption of existing checkouts,
//!   protected-ref refusal and one-writable-checkout-per-branch. Nothing
//!   in this domain deletes or prunes a worktree — release only records.
//! - [`evidence`]: strictly read-only `gh` (whitelisted subcommand trees),
//!   evidence rows bound to task + head SHA with visible staleness, and
//!   honest `Unavailable` states when auth/network is missing.

pub mod cli;
pub mod evidence;
pub mod worktrees;
