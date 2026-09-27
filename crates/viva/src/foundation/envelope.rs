//! Control requests and results: the control-channel envelope.
//!
//! G0 semantics (rollout §2 + issue #10):
//! - A caller's role is **bound by the host** when the channel is issued. A
//!   client may claim any role it likes inside its own messages — the claim is
//!   informational and never evidence. A worker source therefore cannot be
//!   self-declared through a flag or an environment variable; it exists only
//!   if this office issued a channel for it.
//! - Every request carries a version and a request id; payloads are size
//!   bounded; rejections carry a machine-readable [`RejectionReason`].
//! - Submissions are idempotent: the same request id replays the recorded
//!   outcome; a reused idempotency key under a new request id conflicts.

use std::collections::HashMap;
use std::fmt;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{ChannelId, ExecutionId, GrantId, RequestId, TerminalId, utc_now};
use crate::foundation::store::Store;

/// Current envelope version. A version bump is a coordinated contract change.
pub const ENVELOPE_VERSION: u16 = 1;
/// Hard bound on the serialized payload in bytes.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

/// The role a channel is bound to. Only the host (via [`ChannelRegistry`])
/// decides this; clients can only echo it back.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallerRole {
    /// The user, interacting directly.
    User,
    /// A supervised member execution.
    MemberExecution(ExecutionId),
    /// An external agent CLI the user started by hand.
    AgentCli,
    /// A test fixture. Never used in production paths.
    TestRun,
}

impl CallerRole {
    /// Stable wire/DB representation.
    pub fn tag(&self) -> String {
        match self {
            CallerRole::User => "user".into(),
            CallerRole::MemberExecution(exec) => format!("member_execution:{exec}"),
            CallerRole::AgentCli => "agent_cli".into(),
            CallerRole::TestRun => "test_run".into(),
        }
    }
}

impl fmt::Display for CallerRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.tag())
    }
}

impl fmt::Display for CallerIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.channel_id)
    }
}

/// What a client presents on the control channel. `claimed_role` is a claim,
/// not a credential — [`ChannelRegistry::validate`] is the only authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallerIdentity {
    pub channel_id: ChannelId,
    pub claimed_role: CallerRole,
}

/// A live channel issued by the host. The host owns the lifecycle: issue on
/// spawn/serve, revoke when the caller is gone.
#[derive(Debug, Clone)]
pub struct IssuedChannel {
    pub channel_id: ChannelId,
    pub role: CallerRole,
    pub issued_at: String,
    pub revoked: bool,
}

/// Host-side registry of issued control channels.
#[derive(Debug, Default)]
pub struct ChannelRegistry {
    channels: HashMap<String, IssuedChannel>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a new channel bound to `role` and return the client identity.
    /// The identity echoes the binding as its claim; an honest client passes
    /// it through, a forged claim is caught by [`ChannelRegistry::validate`].
    pub fn issue(&mut self, role: CallerRole) -> CallerIdentity {
        let channel_id = ChannelId::new();
        self.channels.insert(
            channel_id.as_str().to_string(),
            IssuedChannel {
                channel_id: channel_id.clone(),
                role: role.clone(),
                issued_at: utc_now(),
                revoked: false,
            },
        );
        CallerIdentity {
            channel_id,
            claimed_role: role,
        }
    }

    /// Revoke a live channel. Revoked channels reject every further request.
    pub fn revoke(&mut self, channel_id: &ChannelId) -> OfficeResult<()> {
        match self.channels.get_mut(channel_id.as_str()) {
            Some(channel) => {
                channel.revoked = true;
                Ok(())
            }
            None => Err(OfficeError::NotFound {
                entity: "channel",
                id: channel_id.to_string(),
            }),
        }
    }

    pub fn get(&self, channel_id: &ChannelId) -> Option<&IssuedChannel> {
        self.channels.get(channel_id.as_str())
    }

    /// Number of live (issued, not revoked) channels.
    pub fn live_count(&self) -> usize {
        self.channels.values().filter(|c| !c.revoked).count()
    }

    /// Validate a presented identity against the issued binding. The claim
    /// must match the binding exactly; anything else is untrusted.
    pub fn validate(&self, caller: &CallerIdentity) -> Result<CallerRole, String> {
        let Some(issued) = self.get(&caller.channel_id) else {
            return Err(format!(
                "channel `{caller}` was never issued by this office"
            ));
        };
        if issued.revoked {
            return Err(format!("channel `{caller}` is revoked"));
        }
        if issued.role != caller.claimed_role {
            return Err(format!(
                "channel `{caller}` is bound to {}, but the request claims {} — \
                 a caller cannot self-declare its role",
                issued.role, caller.claimed_role
            ));
        }
        Ok(issued.role.clone())
    }
}

/// The minimal frozen control-kind set. Domains extend this enum through
/// coordinated contract changes (it is `#[non_exhaustive]`); terminal
/// behavior itself lands with V05.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlKind {
    /// Liveness check; no side effects.
    Ping,
    /// Minimal terminal control surface (input/resize/snapshot/stop).
    Terminal {
        terminal_id: TerminalId,
        action: TerminalAction,
    },
}

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalAction {
    Input(String),
    Resize { cols: u16, rows: u16 },
    Snapshot,
    Stop,
}

/// Why a request was rejected. Machine-readable by design: callers branch on
/// the variant, not on error strings.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionReason {
    UnsupportedVersion { got: u16, expected: u16 },
    PayloadTooLarge { size: usize, max: usize },
    UntrustedCaller { detail: String },
    IdempotencyConflict { key: String },
    Malformed { detail: String },
}

impl fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RejectionReason::UnsupportedVersion { got, expected } => {
                write!(
                    f,
                    "unsupported envelope version {got} (expected {expected})"
                )
            }
            RejectionReason::PayloadTooLarge { size, max } => {
                write!(f, "payload is {size} bytes; the bound is {max}")
            }
            RejectionReason::UntrustedCaller { detail } => write!(f, "untrusted caller: {detail}"),
            RejectionReason::IdempotencyConflict { key } => {
                write!(f, "idempotency key `{key}` already used by another request")
            }
            RejectionReason::Malformed { detail } => write!(f, "malformed request: {detail}"),
        }
    }
}

/// A control request as submitted by a caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlRequest {
    pub request_id: RequestId,
    pub version: u16,
    pub kind: ControlKind,
    pub caller: CallerIdentity,
    /// Pointer to an authorization grant when the action requires one.
    /// Scope enforcement belongs to the authority domain (V04); the envelope
    /// only carries the reference.
    pub grant: Option<GrantId>,
    pub payload: Value,
    pub idempotency_key: Option<String>,
}

impl ControlRequest {
    pub fn new(kind: ControlKind, caller: CallerIdentity) -> Self {
        Self {
            request_id: RequestId::new(),
            version: ENVELOPE_VERSION,
            kind,
            caller,
            grant: None,
            payload: Value::Null,
            idempotency_key: None,
        }
    }

    #[must_use]
    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }

    #[must_use]
    pub fn with_grant(mut self, grant: GrantId) -> Self {
        self.grant = Some(grant);
        self
    }

    #[must_use]
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    pub fn payload_size(&self) -> OfficeResult<usize> {
        Ok(serde_json::to_vec(&self.payload)?.len())
    }
}

/// The outcome of a submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ControlOutcome {
    Accepted {
        request_id: String,
    },
    /// Same request id seen before: the recorded outcome stands, nothing is
    /// re-executed.
    Replayed {
        request_id: String,
        original_status: String,
    },
    Rejected {
        request_id: String,
        reason: RejectionReason,
    },
}

/// A persisted control-request row (audit + idempotency ledger).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredControlRequest {
    pub request_id: String,
    pub idempotency_key: Option<String>,
    pub caller_channel: String,
    pub caller_role: String,
    pub kind: String,
    pub status: String,
    pub rejection: Option<String>,
    pub result_json: Option<String>,
    pub received_at: String,
}

/// Submits control requests against a channel registry and records every
/// outcome in `control_requests`.
pub struct ControlDispatcher<'a> {
    registry: &'a ChannelRegistry,
    store: &'a Store,
}

impl<'a> ControlDispatcher<'a> {
    pub fn new(registry: &'a ChannelRegistry, store: &'a Store) -> Self {
        Self { registry, store }
    }

    /// Validate and record one request. Never panics on bad input — bad input
    /// is a rejection. Replay detection runs first: a request id whose
    /// outcome is already recorded replays that outcome without re-validation.
    pub fn submit(&self, request: &ControlRequest) -> OfficeResult<ControlOutcome> {
        if let Some(existing) = self.lookup(&request.request_id)? {
            // Idempotency by request id: the recorded outcome stands.
            return Ok(ControlOutcome::Replayed {
                request_id: existing.request_id,
                original_status: existing.status,
            });
        }

        if request.version != ENVELOPE_VERSION {
            return self.reject(
                request,
                RejectionReason::UnsupportedVersion {
                    got: request.version,
                    expected: ENVELOPE_VERSION,
                },
            );
        }

        let payload_size = serde_json::to_vec(&request.payload)?.len();
        if payload_size > MAX_PAYLOAD_BYTES {
            return self.reject(
                request,
                RejectionReason::PayloadTooLarge {
                    size: payload_size,
                    max: MAX_PAYLOAD_BYTES,
                },
            );
        }

        if let Err(detail) = self.registry.validate(&request.caller) {
            return self.reject(request, RejectionReason::UntrustedCaller { detail });
        }

        if let Some(key) = &request.idempotency_key {
            if let Some(existing) = self.lookup_by_key(key)? {
                if existing.request_id != request.request_id.as_str() {
                    return self.reject(
                        request,
                        RejectionReason::IdempotencyConflict { key: key.clone() },
                    );
                }
            }
        }

        self.record(
            request,
            "accepted",
            None,
            Some(r#"{"accepted":true}"#),
            false,
        )?;
        Ok(ControlOutcome::Accepted {
            request_id: request.request_id.to_string(),
        })
    }

    fn reject(
        &self,
        request: &ControlRequest,
        reason: RejectionReason,
    ) -> OfficeResult<ControlOutcome> {
        // A conflict rejection must not claim the disputed key for itself —
        // the key already belongs to the original request's row.
        let strip_key = matches!(reason, RejectionReason::IdempotencyConflict { .. });
        self.record(request, "rejected", Some(reason.clone()), None, strip_key)?;
        Ok(ControlOutcome::Rejected {
            request_id: request.request_id.to_string(),
            reason,
        })
    }

    fn record(
        &self,
        request: &ControlRequest,
        status: &str,
        rejection: Option<RejectionReason>,
        result_json: Option<&str>,
        strip_key: bool,
    ) -> OfficeResult<()> {
        self.store.connection().execute(
            "INSERT INTO control_requests(
                request_id, idempotency_key, caller_channel, caller_role, kind,
                status, rejection, result_json, received_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                request.request_id.as_str(),
                if strip_key {
                    None
                } else {
                    request.idempotency_key.as_deref()
                },
                request.caller.channel_id.as_str(),
                request.caller.claimed_role.tag(),
                serde_json::to_string(&request.kind)?,
                status,
                rejection.as_ref().map(RejectionReason::to_string),
                result_json,
                utc_now(),
            ],
        )?;
        Ok(())
    }

    fn lookup(&self, request_id: &RequestId) -> OfficeResult<Option<StoredControlRequest>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT request_id, idempotency_key, caller_channel, caller_role, kind,
                    status, rejection, result_json, received_at
             FROM control_requests WHERE request_id = ?1",
        )?;
        let row = stmt.query_row([request_id.as_str()], map_row).optional()?;
        Ok(row)
    }

    fn lookup_by_key(&self, key: &str) -> OfficeResult<Option<StoredControlRequest>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT request_id, idempotency_key, caller_channel, caller_role, kind,
                    status, rejection, result_json, received_at
             FROM control_requests WHERE idempotency_key = ?1",
        )?;
        let row = stmt.query_row([key], map_row).optional()?;
        Ok(row)
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredControlRequest> {
    Ok(StoredControlRequest {
        request_id: row.get(0)?,
        idempotency_key: row.get(1)?,
        caller_channel: row.get(2)?,
        caller_role: row.get(3)?,
        kind: row.get(4)?,
        status: row.get(5)?,
        rejection: row.get(6)?,
        result_json: row.get(7)?,
        received_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{
        DOMAIN_FOUNDATION, FOUNDATION_V1_SQL, MigrationRegistry, Store,
    };
    use serde_json::json;

    fn store() -> Store {
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .freeze()
            .expect("registry");
        Store::open_in_memory(&frozen).expect("store")
    }

    #[test]
    fn issued_caller_ping_is_accepted_and_recorded() {
        let store = store();
        let mut registry = ChannelRegistry::new();
        let identity = registry.issue(CallerRole::User);
        let dispatcher = ControlDispatcher::new(&registry, &store);

        let request = ControlRequest::new(ControlKind::Ping, identity);
        let outcome = dispatcher.submit(&request).expect("submit");
        assert_eq!(
            outcome,
            ControlOutcome::Accepted {
                request_id: request.request_id.to_string()
            }
        );

        let recorded = dispatcher
            .lookup(&request.request_id)
            .expect("lookup")
            .expect("recorded");
        assert_eq!(recorded.status, "accepted");
        assert_eq!(recorded.caller_role, "user");
    }

    #[test]
    fn self_declared_role_is_never_evidence() {
        let store = store();
        let mut registry = ChannelRegistry::new();
        let user_channel = registry.issue(CallerRole::User);
        let dispatcher = ControlDispatcher::new(&registry, &store);

        // A worker pretending to be a member execution over a user channel.
        let mut forged = user_channel.clone();
        forged.claimed_role = CallerRole::MemberExecution(ExecutionId::new());
        let request = ControlRequest::new(ControlKind::Ping, forged);
        let outcome = dispatcher.submit(&request).expect("submit");
        match outcome {
            ControlOutcome::Rejected { reason, .. } => {
                assert!(
                    matches!(reason, RejectionReason::UntrustedCaller { .. }),
                    "got: {reason}"
                );
            }
            other => panic!("expected rejection, got {other:?}"),
        }

        // A channel that was never issued at all.
        let stranger = CallerIdentity {
            channel_id: ChannelId::new(),
            claimed_role: CallerRole::AgentCli,
        };
        let request = ControlRequest::new(ControlKind::Ping, stranger);
        assert!(matches!(
            dispatcher.submit(&request).expect("submit"),
            ControlOutcome::Rejected {
                reason: RejectionReason::UntrustedCaller { .. },
                ..
            }
        ));
    }

    #[test]
    fn revoked_channel_rejects_further_requests() {
        let store = store();
        let mut registry = ChannelRegistry::new();
        let identity = registry.issue(CallerRole::AgentCli);
        registry.revoke(&identity.channel_id).expect("revoke");
        assert_eq!(registry.live_count(), 0);

        let dispatcher = ControlDispatcher::new(&registry, &store);
        let request = ControlRequest::new(ControlKind::Ping, identity);
        assert!(matches!(
            dispatcher.submit(&request).expect("submit"),
            ControlOutcome::Rejected {
                reason: RejectionReason::UntrustedCaller { .. },
                ..
            }
        ));
    }

    #[test]
    fn wrong_version_and_oversized_payload_reject_cleanly() {
        let store = store();
        let mut registry = ChannelRegistry::new();
        let identity = registry.issue(CallerRole::User);
        let dispatcher = ControlDispatcher::new(&registry, &store);

        let mut request = ControlRequest::new(ControlKind::Ping, identity.clone());
        request.version = 99;
        assert!(matches!(
            dispatcher.submit(&request).expect("submit"),
            ControlOutcome::Rejected {
                reason: RejectionReason::UnsupportedVersion {
                    got: 99,
                    expected: 1
                },
                ..
            }
        ));

        let big = ControlRequest::new(ControlKind::Ping, identity)
            .with_payload(json!({ "blob": "x".repeat(MAX_PAYLOAD_BYTES + 1) }));
        assert!(matches!(
            dispatcher.submit(&big).expect("submit"),
            ControlOutcome::Rejected {
                reason: RejectionReason::PayloadTooLarge { .. },
                ..
            }
        ));
    }

    #[test]
    fn same_request_id_replays_recorded_outcome() {
        let store = store();
        let mut registry = ChannelRegistry::new();
        let identity = registry.issue(CallerRole::User);
        let dispatcher = ControlDispatcher::new(&registry, &store);

        let request = ControlRequest::new(ControlKind::Ping, identity.clone())
            .with_idempotency_key("deliver-v01-pr");
        let first = dispatcher.submit(&request).expect("first");
        assert_eq!(
            first,
            ControlOutcome::Accepted {
                request_id: request.request_id.to_string()
            }
        );

        // Replayed: accepted once, never re-executed.
        let second = dispatcher.submit(&request).expect("second");
        assert_eq!(
            second,
            ControlOutcome::Replayed {
                request_id: request.request_id.to_string(),
                original_status: "accepted".into()
            }
        );

        // Rejections are idempotent too: a replay of a rejected request id
        // returns the recorded rejection, even if the resubmission fixed the
        // version — the recorded outcome stands.
        let mut stale =
            ControlRequest::new(ControlKind::Ping, identity).with_idempotency_key("stale-key");
        stale.version = 42;
        assert!(matches!(
            dispatcher.submit(&stale).expect("submit"),
            ControlOutcome::Rejected {
                reason: RejectionReason::UnsupportedVersion { .. },
                ..
            }
        ));
        let fixed = ControlRequest {
            version: ENVELOPE_VERSION,
            ..stale.clone()
        };
        assert_eq!(
            dispatcher.submit(&fixed).expect("replay"),
            ControlOutcome::Replayed {
                request_id: stale.request_id.to_string(),
                original_status: "rejected".into()
            }
        );
    }

    #[test]
    fn reused_idempotency_key_conflicts() {
        let store = store();
        let mut registry = ChannelRegistry::new();
        let identity = registry.issue(CallerRole::User);
        let dispatcher = ControlDispatcher::new(&registry, &store);

        let first = ControlRequest::new(ControlKind::Ping, identity.clone())
            .with_idempotency_key("same-key");
        dispatcher.submit(&first).expect("first");

        let second =
            ControlRequest::new(ControlKind::Ping, identity).with_idempotency_key("same-key");
        assert!(matches!(
            dispatcher.submit(&second).expect("submit"),
            ControlOutcome::Rejected { reason: RejectionReason::IdempotencyConflict { key } , .. } if key == "same-key"
        ));
    }

    #[test]
    fn terminal_kind_serializes_with_action() {
        let kind = ControlKind::Terminal {
            terminal_id: TerminalId::new(),
            action: TerminalAction::Resize {
                cols: 120,
                rows: 40,
            },
        };
        let text = serde_json::to_string(&kind).expect("serialize");
        let back: ControlKind = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back, kind);
    }
}
