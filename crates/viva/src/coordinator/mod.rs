//! Coordinator-facing progress rollup (Resident C4 morning report, issue #121).
//!
//! Live queries over Task / Execution facts and read-only GitHub evidence.
//! Nothing here writes to GitHub or substitutes memory for dynamic state.

mod morning;

pub use morning::{
    BlockedSignals, GithubAuth, GithubSection, MorningItem, MorningOptions, MorningReport,
    MorningRollup, MultiRoundSignal, WaitingMerge, default_previous_local_day,
    local_tz_offset_seconds,
};
