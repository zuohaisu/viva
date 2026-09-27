//! Typed identifiers and timestamps.
//!
//! G0 semantics: ids are typed newtypes with a fixed prefix so a `TaskId` can
//! never be passed where an `ExecutionId` is expected. Prefixes follow the
//! Python reference (`task-`, `exec-`, `grant-`, `sess-`); bodies are freshly
//! generated uuid4 hex. Ids are configuration-level facts, never identity.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

/// ISO-8601 UTC timestamp with a trailing offset, mirroring the Python
/// reference format.
pub fn utc_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("now_utc is always representable as RFC 3339")
}

/// A malformed identifier string was parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("id `{id}` must start with prefix `{expected}-`")]
    BadPrefix { id: String, expected: &'static str },
    #[error("id `{id}` has an empty or malformed body")]
    BadBody { id: String },
}

/// Defines a newtype id: `TaskId("task-<uuid4>")`, serde-transparent.
macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            /// Generate a fresh id: `<prefix>-<uuid4 hex>`.
            pub fn new() -> Self {
                Self(format!("{}-{}", $prefix, Uuid::new_v4().simple()))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let prefix = concat!($prefix, "-");
                let body = s
                    .strip_prefix(prefix)
                    .ok_or_else(|| IdError::BadPrefix { id: s.to_string(), expected: $prefix })?;
                if body.is_empty()
                    || body.len() > 64
                    || !body.chars().all(|c| c.is_ascii_alphanumeric())
                {
                    return Err(IdError::BadBody { id: s.to_string() });
                }
                Ok(Self(s.to_string()))
            }
        }
    };
}

define_id!(
    /// A member of the office. Members are configuration, never hard-coded
    /// identity: `Viva ≠ Samuel`.
    MemberId,
    "mem"
);
define_id!(
    /// A workspace: the primary work context that groups projects.
    WorkspaceId,
    "ws"
);
define_id!(
    /// A project inside a workspace.
    ProjectId,
    "proj"
);
define_id!(
    /// A git worktree allocated for isolation.
    WorktreeId,
    "wt"
);
define_id!(
    /// A task delivered through the office.
    TaskId,
    "task"
);
define_id!(
    /// One supervised execution of a task.
    ExecutionId,
    "exec"
);
define_id!(
    /// An office-side conversation/execution session reference.
    SessionId,
    "sess"
);
define_id!(
    /// An authorization grant with task/actions/mode scope.
    GrantId,
    "grant"
);
define_id!(
    /// A terminal hosted by the office (member execution, user shell, …).
    TerminalId,
    "term"
);
define_id!(
    /// A control-channel request id.
    RequestId,
    "req"
);
define_id!(
    /// A control-channel binding issued by the host.
    ChannelId,
    "chan"
);
define_id!(
    /// An append-only office event.
    EventId,
    "evt"
);
define_id!(
    /// A persisted launch specification.
    LaunchSpecId,
    "spec"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_carry_prefix_and_roundtrip() {
        let id = TaskId::new();
        assert!(id.as_str().starts_with("task-"));
        let parsed: TaskId = id.as_str().parse().expect("generated id reparses");
        assert_eq!(parsed, id);
    }

    #[test]
    fn foreign_prefix_is_rejected() {
        let err = TaskId::from_str("exec-abc123").expect_err("wrong prefix must fail");
        assert!(matches!(
            err,
            IdError::BadPrefix {
                expected: "task",
                ..
            }
        ));
    }

    #[test]
    fn malformed_body_is_rejected() {
        assert!(TaskId::from_str("task-").is_err());
        assert!(TaskId::from_str("task-has space").is_err());
    }

    #[test]
    fn utc_now_is_rfc3339() {
        let now = utc_now();
        OffsetDateTime::parse(&now, &Rfc3339).expect("utc_now parses as RFC 3339");
    }
}
