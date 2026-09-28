//! Delegation authority: live grants, call origin and protected actions
//! (V04, issue #13).
//!
//! Semantics (ADR 0007 + rollout §2):
//! - Permission vocabulary is preserved from the office charter:
//!   READ / PROPOSE / ACT_WITH_APPROVAL / ACT_AUTONOMOUSLY / FORBIDDEN.
//! - Member-initiated dispatch and stopping exist only under a live grant.
//!   A child grant may never widen its parent (actions are a subset, mode is
//!   not higher, scope stays on the same task).
//! - Protected actions (push to a protected branch, merge, approve,
//!   autonomous worker invocation, self-authorization) can never be opened
//!   by any grant.
//! - Worker call credentials exist only because the live host issued them
//!   (tied to a control channel); a worker claiming to be the user, or an
//!   unissued/revoked credential, is denied and the denial is appended.
//! - Every denial is an append-only record — rejections leave evidence.

use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{ChannelId, ExecutionId, GrantId, MemberId, TaskId, utc_now};
use crate::foundation::store::{DOMAIN_AUTHORITY, MigrationRegistry, Store};

/// Register the `authority` domain migrations (V04's namespace).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(DOMAIN_AUTHORITY, 1, "authority v1", AUTHORITY_V1_SQL)
}

pub const AUTHORITY_V1_SQL: &str = r#"
CREATE TABLE grants (
    grant_id            TEXT PRIMARY KEY,
    parent_grant_id     TEXT REFERENCES grants(grant_id),
    principal_member_id TEXT,
    issued_by           TEXT NOT NULL CHECK (issued_by IN ('user', 'delegation')),
    task_id             TEXT,
    actions_json        TEXT NOT NULL,
    mode                TEXT NOT NULL
                        CHECK (mode IN ('READ', 'PROPOSE', 'ACT_WITH_APPROVAL', 'ACT_AUTONOMOUSLY')),
    status              TEXT NOT NULL CHECK (status IN ('live', 'revoked')),
    expires_at          TEXT,
    created_at          TEXT NOT NULL,
    revoked_at          TEXT,
    revoke_reason       TEXT
);

-- Append-only denial log: every rejected authorization leaves its reason.
CREATE TABLE grant_denials (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    subject      TEXT NOT NULL,
    action       TEXT NOT NULL,
    reason       TEXT NOT NULL,
    occurred_at  TEXT NOT NULL
);

-- Worker call credentials, issued only by the live host and tied to a
-- control channel. Nothing else — no flag, no environment variable — can
-- establish a worker's authority.
CREATE TABLE worker_credentials (
    credential_id TEXT PRIMARY KEY,
    channel_id    TEXT NOT NULL UNIQUE,
    execution_id  TEXT,
    member_id     TEXT NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('active', 'revoked')),
    issued_at     TEXT NOT NULL,
    revoked_at    TEXT
);
"#;

// ---------------------------------------------------------------------------
// Grant model
// ---------------------------------------------------------------------------

/// Grant modes follow the office permission vocabulary. The ordering here is
/// the escalation order used to keep children from widening parents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GrantMode {
    Read,
    Propose,
    ActWithApproval,
    ActAutonomously,
}

impl GrantMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            GrantMode::Read => "READ",
            GrantMode::Propose => "PROPOSE",
            GrantMode::ActWithApproval => "ACT_WITH_APPROVAL",
            GrantMode::ActAutonomously => "ACT_AUTONOMOUSLY",
        }
    }
}

/// A grant as persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub grant_id: GrantId,
    pub parent_grant_id: Option<GrantId>,
    pub principal_member_id: Option<MemberId>,
    pub issued_by: Issuer,
    pub task_id: Option<TaskId>,
    pub actions: Vec<String>,
    pub mode: GrantMode,
    pub status: GrantStatus,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub revoke_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Issuer {
    /// The repository owner / user issued this root grant.
    User,
    /// Delegated by a parent grant (child scope rules apply).
    Delegation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantStatus {
    Live,
    Revoked,
}

/// Why an authorization was denied. Machine-readable, and every denial is
/// also appended to `grant_denials`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum DenialReason {
    ProtectedAction,
    NoGrant,
    GrantNotLive,
    GrantExpired,
    ActionOutsideScope,
    CrossTask,
    ModeInsufficient,
    UntrustedWorker,
    WorkerImpersonatesUser,
}

impl std::fmt::Display for DenialReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            DenialReason::ProtectedAction => "protected action: no grant may open it",
            DenialReason::NoGrant => "this action requires a live grant; plain chat may only read",
            DenialReason::GrantNotLive => "the grant is revoked and no longer authorizes anything",
            DenialReason::GrantExpired => "the grant has expired",
            DenialReason::ActionOutsideScope => "the action is outside this grant's action scope",
            DenialReason::CrossTask => "the grant is scoped to another task",
            DenialReason::ModeInsufficient => "the grant mode is insufficient for this action",
            DenialReason::UntrustedWorker => "the worker credential was not issued by this host",
            DenialReason::WorkerImpersonatesUser => "a worker credential can never act as the user",
        };
        f.write_str(text)
    }
}

/// Who is asking. Either a member acting under a grant, or a supervised
/// worker presenting its host-issued credential id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Member {
        member: MemberId,
        grant: Option<GrantId>,
    },
    Worker {
        credential_id: String,
    },
}

/// Actions the office classifies as protected: no grant, root or delegated,
/// can ever authorize them. Vocabulary preserved from the office charter
/// (`src/viva/permissions/`, ADR 0007).
pub const PROTECTED_ACTIONS: &[&str] = &[
    "push_protected_branch",
    "merge_pull_request",
    "approve_pull_request",
    "invoke_worker_autonomously",
    "self_authorize",
];

/// Actions that mutate the world and therefore require a live grant with an
/// ACT_* mode; everything else a grant lists requires at least PROPOSE.
const DISPATCH_ACTIONS: &[&str] = &[
    "dispatch_delegated",
    "stop_delegated",
    "invoke_worker",
    "delegate_grant",
];

/// Read-class actions a plain chat identity may perform without any grant.
const READ_ACTIONS: &[&str] = &[
    "observe_status",
    "read_task",
    "read_execution",
    "read_knowledge",
    "read_github",
    "list_worktrees",
];

// ---------------------------------------------------------------------------
// Authority engine
// ---------------------------------------------------------------------------

pub struct AuthorityEngine<'a> {
    store: &'a Store,
}

impl<'a> AuthorityEngine<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    // -- Grant lifecycle -----------------------------------------------------

    /// Issue a root grant. Only the user issues root grants; the engine
    /// records the issuer as such and never lets this path be called "on
    /// behalf of" a member.
    pub fn issue_root_grant(
        &self,
        principal: Option<MemberId>,
        task_id: Option<TaskId>,
        actions: Vec<String>,
        mode: GrantMode,
        expires_at: Option<String>,
    ) -> OfficeResult<Grant> {
        if actions.is_empty() {
            return Err(OfficeError::Validation(
                "a grant must list at least one action".into(),
            ));
        }
        if actions.iter().any(|a| Self::is_protected(a)) {
            return Err(OfficeError::Validation(
                "protected actions can never be granted".into(),
            ));
        }
        let grant = Grant {
            grant_id: GrantId::new(),
            parent_grant_id: None,
            principal_member_id: principal,
            issued_by: Issuer::User,
            task_id,
            actions,
            mode,
            status: GrantStatus::Live,
            expires_at,
            created_at: utc_now(),
            revoked_at: None,
            revoke_reason: None,
        };
        self.insert_grant(&grant)?;
        Ok(grant)
    }

    /// Delegate a child grant from a live parent. The child can never widen
    /// the parent: its actions must be a subset, its mode must not exceed
    /// the parent's, and its scope stays on the parent's task.
    pub fn delegate_grant(
        &self,
        parent_grant_id: &GrantId,
        principal: MemberId,
        actions: Vec<String>,
        mode: GrantMode,
        expires_at: Option<String>,
    ) -> OfficeResult<Grant> {
        let parent = self.require_grant(parent_grant_id)?;
        match parent.status {
            GrantStatus::Live => {}
            GrantStatus::Revoked => {
                return Err(OfficeError::Validation(
                    "a revoked grant cannot delegate".into(),
                ));
            }
        }
        // Child never widens: the ceiling holds in TIME too. An expired
        // parent delegates nothing, and a child never outlives its parent.
        if let Some(expires_at) = &parent.expires_at {
            if expires_at.as_str() <= utc_now().as_str() {
                return Err(OfficeError::Validation(
                    "an expired grant cannot delegate — refresh or reissue the parent first".into(),
                ));
            }
        }
        // Child never widens: action subset.
        for action in &actions {
            if Self::is_protected(action) {
                return Err(OfficeError::Validation(format!(
                    "protected action `{action}` can never be granted, not even by delegation"
                )));
            }
            if !parent.actions.contains(action) {
                return Err(OfficeError::Validation(format!(
                    "child grant action `{action}` is outside the parent's scope — \
                     a child grant may never widen its parent"
                )));
            }
        }
        // Child never widens: mode ceiling.
        if mode > parent.mode {
            return Err(OfficeError::Validation(format!(
                "child grant mode {} exceeds the parent's {} — a child grant may never widen its parent",
                mode.as_str(),
                parent.mode.as_str()
            )));
        }
        // Child never widens: task scope, and the child's expiry is capped
        // at the parent's (an unset child expiry inherits the parent's).
        let expires_at = match (&parent.expires_at, expires_at) {
            (Some(parent_exp), Some(child_exp)) if child_exp.as_str() <= parent_exp.as_str() => {
                Some(child_exp)
            }
            (Some(parent_exp), Some(_too_late)) => Some(parent_exp.clone()),
            (Some(parent_exp), None) => Some(parent_exp.clone()),
            (None, child_exp) => child_exp,
        };
        let grant = Grant {
            grant_id: GrantId::new(),
            parent_grant_id: Some(parent.grant_id.clone()),
            principal_member_id: Some(principal),
            issued_by: Issuer::Delegation,
            task_id: parent.task_id.clone(),
            actions,
            mode,
            status: GrantStatus::Live,
            expires_at,
            created_at: utc_now(),
            revoked_at: None,
            revoke_reason: None,
        };
        self.insert_grant(&grant)?;
        Ok(grant)
    }

    /// Revoke a grant. Revocation takes effect immediately at every later
    /// check; a revoked grant also cannot authorize anything retroactively.
    pub fn revoke(&self, grant_id: &GrantId, reason: impl Into<String>) -> OfficeResult<()> {
        let n = self.store.connection().execute(
            "UPDATE grants SET status = 'revoked', revoked_at = ?2, revoke_reason = ?3
             WHERE grant_id = ?1 AND status = 'live'",
            rusqlite::params![grant_id.as_str(), utc_now(), reason.into()],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "live grant",
                id: grant_id.to_string(),
            });
        }
        Ok(())
    }

    pub fn get_grant(&self, grant_id: &GrantId) -> OfficeResult<Option<Grant>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT grant_id, parent_grant_id, principal_member_id, issued_by, task_id,
                    actions_json, mode, status, expires_at, created_at, revoked_at, revoke_reason
             FROM grants WHERE grant_id = ?1",
        )?;
        let row = stmt.query_row([grant_id.as_str()], map_grant).optional()?;
        Ok(row)
    }

    pub fn require_grant(&self, grant_id: &GrantId) -> OfficeResult<Grant> {
        self.get_grant(grant_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "grant",
                id: grant_id.to_string(),
            })
    }

    // -- Authorization -------------------------------------------------------

    /// Check one action. This is the final authorization point: callers must
    /// invoke it at launch time so a revoke racing a dispatch is resolved by
    /// this query, not by a stale in-memory copy.
    pub fn check(
        &self,
        actor: &Actor,
        action: &str,
        task_id: Option<&TaskId>,
    ) -> OfficeResult<Result<(), DenialReason>> {
        let outcome = self.evaluate(actor, action, task_id)?;
        if let Err(ref reason) = outcome {
            self.record_denial(actor, action, reason)?;
        }
        Ok(outcome)
    }

    fn evaluate(
        &self,
        actor: &Actor,
        action: &str,
        task_id: Option<&TaskId>,
    ) -> OfficeResult<Result<(), DenialReason>> {
        // Protected actions are closed to every actor, always.
        if Self::is_protected(action) {
            return Ok(Err(DenialReason::ProtectedAction));
        }

        let Actor::Member { grant, .. } = actor else {
            // Workers act through their own credential path (see
            // `check_worker_credential`); they cannot take member actions.
            return Ok(Err(DenialReason::UntrustedWorker));
        };

        let Some(grant_id) = grant else {
            // Plain chat without a grant: only reading allowed material.
            if Self::is_read_action(action) {
                return Ok(Ok(()));
            }
            return Ok(Err(DenialReason::NoGrant));
        };
        let grant = self.require_grant(grant_id)?;
        match grant.status {
            GrantStatus::Live => {}
            GrantStatus::Revoked => return Ok(Err(DenialReason::GrantNotLive)),
        }
        if let Some(expires_at) = &grant.expires_at {
            if expires_at.as_str() <= utc_now().as_str() {
                return Ok(Err(DenialReason::GrantExpired));
            }
        }
        // Cross-task scope: a grant scoped to a task cannot serve another.
        if let Some(scoped) = &grant.task_id {
            match task_id {
                Some(requested) if requested == scoped => {}
                _ => return Ok(Err(DenialReason::CrossTask)),
            }
        }
        if !grant.actions.iter().any(|a| a == action) {
            return Ok(Err(DenialReason::ActionOutsideScope));
        }
        // Dispatch-class actions need an ACT_* mode.
        if Self::is_dispatch(action) && grant.mode < GrantMode::ActWithApproval {
            return Ok(Err(DenialReason::ModeInsufficient));
        }
        Ok(Ok(()))
    }

    // -- Worker credentials ---------------------------------------------------

    /// Issue a worker credential. Called only by the live host when it
    /// spawns/serves a supervised worker; the credential is bound to the
    /// control channel the host created.
    pub fn issue_worker_credential(
        &self,
        channel: &ChannelId,
        execution: Option<ExecutionId>,
        member: MemberId,
    ) -> OfficeResult<String> {
        let credential_id = format!("cred-{}", uuid::Uuid::new_v4().simple());
        self.store.connection().execute(
            "INSERT INTO worker_credentials(credential_id, channel_id, execution_id, member_id, status, issued_at)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5)",
            rusqlite::params![
                credential_id,
                channel.as_str(),
                execution.as_ref().map(|e| e.as_str()),
                member.as_str(),
                utc_now(),
            ],
        )?;
        Ok(credential_id)
    }

    /// Validate a worker's claimed authority. The claim must match a live,
    /// host-issued credential; `as_user` requests are denied outright — no
    /// worker credential can ever act as the user, regardless of environment.
    pub fn check_worker_credential(
        &self,
        credential_id: &str,
        as_user: bool,
    ) -> OfficeResult<Result<(), DenialReason>> {
        let outcome = if as_user {
            Err(DenialReason::WorkerImpersonatesUser)
        } else {
            let row: Option<String> = self
                .store
                .connection()
                .query_row(
                    "SELECT status FROM worker_credentials WHERE credential_id = ?1",
                    [credential_id],
                    |row| row.get(0),
                )
                .optional()?;
            match row.as_deref() {
                Some("active") => Ok(()),
                Some("revoked") => Err(DenialReason::UntrustedWorker),
                _ => Err(DenialReason::UntrustedWorker),
            }
        };
        if let Err(ref reason) = outcome {
            self.record_denial(
                &Actor::Worker {
                    credential_id: credential_id.to_string(),
                },
                "worker_call",
                reason,
            )?;
        }
        Ok(outcome)
    }

    /// Revoke a worker credential (host-side, when the worker is gone).
    pub fn revoke_worker_credential(&self, credential_id: &str) -> OfficeResult<()> {
        let n = self.store.connection().execute(
            "UPDATE worker_credentials SET status = 'revoked', revoked_at = ?2
             WHERE credential_id = ?1 AND status = 'active'",
            rusqlite::params![credential_id, utc_now()],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "worker credential",
                id: credential_id.to_string(),
            });
        }
        Ok(())
    }

    // -- Denial log ------------------------------------------------------------

    fn record_denial(
        &self,
        actor: &Actor,
        action: &str,
        reason: &DenialReason,
    ) -> OfficeResult<()> {
        let subject = match actor {
            Actor::Member { member, grant } => match grant {
                Some(g) => format!("member:{} grant:{}", member.as_str(), g.as_str()),
                None => format!("member:{}", member.as_str()),
            },
            Actor::Worker { credential_id } => format!("worker:{credential_id}"),
        };
        self.store.connection().execute(
            "INSERT INTO grant_denials(subject, action, reason, occurred_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![subject, action, reason.to_string(), utc_now()],
        )?;
        Ok(())
    }

    /// Recent denials, newest first (audit view).
    pub fn recent_denials(
        &self,
        limit: u32,
    ) -> OfficeResult<Vec<(String, String, String, String)>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT subject, action, reason, occurred_at
             FROM grant_denials ORDER BY seq DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut denials = Vec::new();
        for row in rows {
            denials.push(row?);
        }
        Ok(denials)
    }

    fn insert_grant(&self, grant: &Grant) -> OfficeResult<()> {
        self.store.connection().execute(
            "INSERT INTO grants(grant_id, parent_grant_id, principal_member_id, issued_by,
                                task_id, actions_json, mode, status, expires_at, created_at,
                                revoked_at, revoke_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                grant.grant_id.as_str(),
                grant.parent_grant_id.as_ref().map(|g| g.as_str()),
                grant.principal_member_id.as_ref().map(|m| m.as_str()),
                match grant.issued_by {
                    Issuer::User => "user",
                    Issuer::Delegation => "delegation",
                },
                grant.task_id.as_ref().map(|t| t.as_str()),
                serde_json::to_string(&grant.actions)?,
                grant.mode.as_str(),
                match grant.status {
                    GrantStatus::Live => "live",
                    GrantStatus::Revoked => "revoked",
                },
                grant.expires_at,
                grant.created_at,
                grant.revoked_at,
                grant.revoke_reason,
            ],
        )?;
        Ok(())
    }

    fn is_protected(action: &str) -> bool {
        PROTECTED_ACTIONS.contains(&action)
    }

    fn is_dispatch(action: &str) -> bool {
        DISPATCH_ACTIONS.contains(&action)
    }

    fn is_read_action(action: &str) -> bool {
        READ_ACTIONS.contains(&action)
    }
}

fn map_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<Grant> {
    let issued_by: String = row.get(3)?;
    let mode: String = row.get(6)?;
    let status: String = row.get(7)?;
    Ok(Grant {
        grant_id: GrantId::from_str(&row.get::<_, String>(0)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        parent_grant_id: row
            .get::<_, Option<String>>(1)?
            .as_deref()
            .and_then(|g| GrantId::from_str(g).ok()),
        principal_member_id: row
            .get::<_, Option<String>>(2)?
            .as_deref()
            .and_then(|m| MemberId::from_str(m).ok()),
        issued_by: match issued_by.as_str() {
            "user" => Issuer::User,
            "delegation" => Issuer::Delegation,
            other => panic!("grants.issued_by holds an unknown value `{other}`"),
        },
        task_id: row
            .get::<_, Option<String>>(4)?
            .as_deref()
            .and_then(|t| TaskId::from_str(t).ok()),
        actions: serde_json::from_str(&row.get::<_, String>(5)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?,
        mode: match mode.as_str() {
            "READ" => GrantMode::Read,
            "PROPOSE" => GrantMode::Propose,
            "ACT_WITH_APPROVAL" => GrantMode::ActWithApproval,
            "ACT_AUTONOMOUSLY" => GrantMode::ActAutonomously,
            other => panic!("grants.mode holds an unknown value `{other}`"),
        },
        status: match status.as_str() {
            "live" => GrantStatus::Live,
            "revoked" => GrantStatus::Revoked,
            other => panic!("grants.status holds an unknown value `{other}`"),
        },
        expires_at: row.get(8)?,
        created_at: row.get(9)?,
        revoked_at: row.get(10)?,
        revoke_reason: row.get(11)?,
    })
}

/// Tool mode / sandbox capability reporting. Honesty rule: `read_only` is
/// reported as an enforced restriction only when the adapter carries real
/// proof; a claim without proof is reported as unverified, never as a
/// guarantee. OS-level isolation claims stay out of scope: a same-user
/// terminal is not a security sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityReport {
    pub tool: String,
    pub mode: String,
    /// "enforced" only with real adapter evidence; otherwise "unverified".
    pub restriction_state: RestrictionState,
    pub adapter_evidence: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestrictionState {
    Enforced,
    Unverified,
}

impl CapabilityReport {
    /// Build a report from a claimed read-only mode. Without adapter
    /// evidence the restriction stays unverified — the report must not
    /// display "restricted" on a promise.
    pub fn claimed_read_only(tool: impl Into<String>, adapter_evidence: Option<String>) -> Self {
        Self {
            tool: tool.into(),
            mode: "read_only".into(),
            restriction_state: if adapter_evidence.is_some() {
                RestrictionState::Enforced
            } else {
                RestrictionState::Unverified
            },
            adapter_evidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{DOMAIN_FOUNDATION, FOUNDATION_V1_SQL};

    fn store() -> Store {
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(DOMAIN_AUTHORITY, 1, "authority v1", AUTHORITY_V1_SQL)
            .freeze()
            .expect("registry");
        Store::open_in_memory(&frozen).expect("store")
    }

    fn member_actor(grant: Option<GrantId>) -> Actor {
        Actor::Member {
            member: MemberId::new(),
            grant,
        }
    }

    #[test]
    fn protected_actions_are_never_grantable_or_checkable() {
        let store = store();
        let engine = AuthorityEngine::new(&store);
        let err = engine
            .issue_root_grant(
                None,
                None,
                vec!["merge_pull_request".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .expect_err("protected action must not be grantable");
        assert!(err.to_string().contains("protected"));

        // Even a live grant cannot check them through.
        let grant = engine
            .issue_root_grant(
                None,
                None,
                vec!["read_github".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .expect("grant");
        let actor = member_actor(Some(grant.grant_id));
        let decision = engine
            .check(&actor, "push_protected_branch", None)
            .expect("check runs");
        assert_eq!(decision, Err(DenialReason::ProtectedAction));
        assert_eq!(engine.recent_denials(10).expect("denials").len(), 1);
    }

    #[test]
    fn chat_without_grant_may_read_but_not_dispatch() {
        let store = store();
        let engine = AuthorityEngine::new(&store);
        let actor = member_actor(None);

        // Reading allowed material works without a grant.
        let read = engine.check(&actor, "read_github", None).expect("check");
        assert_eq!(read, Ok(()));

        // Dispatching requires a live grant.
        let dispatch = engine
            .check(&actor, "dispatch_delegated", None)
            .expect("check");
        assert_eq!(dispatch, Err(DenialReason::NoGrant));
    }

    #[test]
    fn child_grant_cannot_widen_parent() {
        let store = store();
        let engine = AuthorityEngine::new(&store);
        let task = TaskId::new();
        let parent = engine
            .issue_root_grant(
                None,
                Some(task.clone()),
                vec!["dispatch_delegated".into(), "read_task".into()],
                GrantMode::ActWithApproval,
                None,
            )
            .expect("parent");

        // Wider action set is rejected.
        let err = engine
            .delegate_grant(
                &parent.grant_id,
                MemberId::new(),
                vec!["dispatch_delegated".into(), "curate_knowledge".into()],
                GrantMode::ActWithApproval,
                None,
            )
            .expect_err("widening actions must fail");
        assert!(err.to_string().contains("widen"), "got: {err}");

        // Higher mode is rejected.
        let err = engine
            .delegate_grant(
                &parent.grant_id,
                MemberId::new(),
                vec!["read_task".into()],
                GrantMode::ActAutonomously,
                None,
            )
            .expect_err("widening mode must fail");
        assert!(err.to_string().contains("exceed"), "got: {err}");

        // An equal-mode child within scope succeeds and keeps the parent's
        // task scope.
        let child = engine
            .delegate_grant(
                &parent.grant_id,
                MemberId::new(),
                vec!["read_task".into()],
                GrantMode::ActWithApproval,
                None,
            )
            .expect("child");
        assert_eq!(child.task_id, Some(task.clone()));
    }

    #[test]
    fn cross_task_and_scope_and_mode_denials() {
        let store = store();
        let engine = AuthorityEngine::new(&store);
        let task = TaskId::new();
        let other_task = TaskId::new();
        let grant = engine
            .issue_root_grant(
                None,
                Some(task.clone()),
                vec!["read_task".into(), "dispatch_delegated".into()],
                GrantMode::Read,
                None,
            )
            .expect("grant");
        let actor = member_actor(Some(grant.grant_id));

        assert_eq!(
            engine
                .check(&actor, "read_task", Some(&other_task))
                .expect("check"),
            Err(DenialReason::CrossTask)
        );
        assert_eq!(
            engine
                .check(&actor, "curate_knowledge", Some(&task))
                .expect("check"),
            Err(DenialReason::ActionOutsideScope)
        );
        assert_eq!(
            engine
                .check(&actor, "dispatch_delegated", Some(&task))
                .expect("check"),
            Err(DenialReason::ModeInsufficient),
            "dispatch under a READ-mode grant is insufficient"
        );
        assert_eq!(engine.recent_denials(10).expect("denials").len(), 3);
    }

    #[test]
    fn revocation_and_expiry_take_effect_at_the_final_check() {
        let store = store();
        let engine = AuthorityEngine::new(&store);
        let task = TaskId::new();
        let grant = engine
            .issue_root_grant(
                None,
                Some(task.clone()),
                vec!["dispatch_delegated".into()],
                GrantMode::ActWithApproval,
                None,
            )
            .expect("grant");
        let actor = member_actor(Some(grant.grant_id.clone()));

        // Live grant dispatches (the launch-time authorization point).
        assert_eq!(
            engine
                .check(&actor, "dispatch_delegated", Some(&task))
                .expect("check"),
            Ok(())
        );

        // A revoke racing dispatch: the final check sees the revoked grant.
        engine
            .revoke(&grant.grant_id, "owner stopped the delegation")
            .expect("revoke");
        assert_eq!(
            engine
                .check(&actor, "dispatch_delegated", Some(&task))
                .expect("check"),
            Err(DenialReason::GrantNotLive)
        );

        // An expired grant is denied too.
        let expired = engine
            .issue_root_grant(
                None,
                Some(task.clone()),
                vec!["read_task".into()],
                GrantMode::Read,
                Some("2000-01-01T00:00:00Z".into()),
            )
            .expect("expired grant");
        let expired_actor = member_actor(Some(expired.grant_id));
        assert_eq!(
            engine
                .check(&expired_actor, "read_task", Some(&task))
                .expect("check"),
            Err(DenialReason::GrantExpired)
        );
    }

    #[test]
    fn worker_credentials_cannot_self_declare_or_impersonate_the_user() {
        let store = store();
        let engine = AuthorityEngine::new(&store);
        let member = MemberId::new();
        let channel = ChannelId::new();
        let credential = engine
            .issue_worker_credential(&channel, Some(ExecutionId::new()), member.clone())
            .expect("issue");

        // A live credential may act as a worker.
        assert_eq!(
            engine
                .check_worker_credential(&credential, false)
                .expect("check"),
            Ok(())
        );

        // No worker credential can act as the user — even a live one.
        assert_eq!(
            engine
                .check_worker_credential(&credential, true)
                .expect("check"),
            Err(DenialReason::WorkerImpersonatesUser)
        );

        // A fabricated credential id is untrusted.
        assert_eq!(
            engine
                .check_worker_credential("cred-forged", false)
                .expect("check"),
            Err(DenialReason::UntrustedWorker)
        );

        // A revoked credential is untrusted.
        engine
            .revoke_worker_credential(&credential)
            .expect("revoke");
        assert_eq!(
            engine
                .check_worker_credential(&credential, false)
                .expect("check"),
            Err(DenialReason::UntrustedWorker)
        );
        assert!(engine.recent_denials(10).expect("denials").len() >= 3);
    }

    #[test]
    fn read_only_is_reported_enforced_only_with_adapter_evidence() {
        let verified = CapabilityReport::claimed_read_only(
            "git-adapter",
            Some("adapter constrains writes by construction (unit-proof #123)".into()),
        );
        assert_eq!(verified.restriction_state, RestrictionState::Enforced);

        let promised = CapabilityReport::claimed_read_only("shell-adapter", None);
        assert_eq!(
            promised.restriction_state,
            RestrictionState::Unverified,
            "a promised restriction must not display as enforced"
        );
    }
}
