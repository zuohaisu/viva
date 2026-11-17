//! P2 glue over the existing process binding, ChannelRegistry and AuthorityEngine.
//! No caller-supplied role/member/task establishes a principal.
use super::*;
use crate::authority::{GrantMode, GrantStatus};
use herdr::embedding::DomainServices;
use sha2::{Digest, Sha256};

// Core validates non-expansion at delegation birth. This native effect seam
// additionally requires the whole original chain to remain live at use time.
fn live_grant_chain(store: &Store, start: &GrantId, required: Option<&GrantId>) -> Result<bool> {
    let engine = AuthorityEngine::new(store);
    let mut next = start.clone();
    let mut found = required.is_none();
    for _ in 0..64 {
        let record = engine.require_grant(&next)?;
        if record.status != GrantStatus::Live
            || record
                .expires_at
                .as_deref()
                .is_some_and(|until| until <= utc_now().as_str())
        {
            return Ok(false);
        }
        found |= required == Some(&next);
        match record.parent_grant_id {
            Some(parent) => next = parent,
            None => return Ok(found),
        }
    }
    Ok(false) // cyclic/corrupt or excessively deep lineage fails closed
}

impl DomainBridge {
    fn member_effect_denied(
        &self,
        reason: &'static str,
        peer: herdr::embedding::TransportPeer,
    ) -> io::Error {
        let audit = self.with_store(|store| {
            events::append(store, NewEvent {
                domain: crate::server::DOMAIN_HOST, kind: "native_member_effect_refused".into(),
                subject_type: "native_ingress".into(), subject_id: uuid::Uuid::new_v4().to_string(),
                origin: "native_member_process".into(),
                payload: serde_json::json!({"reason": reason, "peer_pid": peer.pid, "peer_uid": peer.uid, "human_presence_verified": false, "actor": "unbound_or_refused_member_request"}),
            })?;
            Ok(())
        });
        audit
            .err()
            .unwrap_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, reason))
    }

    fn bound_member(
        &self,
        credential: &str,
        peer: herdr::embedding::TransportPeer,
    ) -> io::Result<ExecutionBinding> {
        if credential.is_empty()
            || peer.uid != Some(unsafe { libc::geteuid() })
            || peer.pid.is_none()
        {
            return Err(self.member_effect_denied("untrusted_transport_or_credential", peer));
        }
        let binding = self
            .executions
            .lock()
            .map_err(|_| io::Error::other("execution state poisoned"))?
            .by_execution
            .values()
            .find(|binding| binding.credential_id == credential)
            .cloned();
        let Some(binding) = binding else {
            return Err(self.member_effect_denied("unknown_credential", peer));
        };
        let (Some(pid), Some(marker)) = (binding.pid, binding.pid_start_marker.as_ref()) else {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "execution is not confirmed yet",
            ));
        };
        if process_start_marker(pid).as_ref() != Some(marker)
            || !is_bound_process(pid, i64::from(peer.pid.expect("checked")))
        {
            return Err(self.member_effect_denied("peer_not_bound_to_execution", peer));
        }
        Ok(binding)
    }

    fn authorize_member(
        &self,
        credential: &str,
        grant: Option<&str>,
        peer: herdr::embedding::TransportPeer,
        action: &str,
        target_task: Option<&TaskId>,
    ) -> io::Result<(ExecutionBinding, GrantId)> {
        let binding = self.bound_member(credential, peer)?;
        let grant = match grant {
            Some(value) => GrantId::from_str(value)
                .map_err(|_| self.member_effect_denied("invalid_grant", peer))?,
            None => binding.grant_id.clone(),
        };
        let accepted = self
            .with_store(|store| {
                let caller = CallerIdentity {
                    channel_id: binding.channel_id.clone(),
                    claimed_role: CallerRole::MemberExecution(binding.execution_id.clone()),
                };
                let channels = self
                    .channels
                    .lock()
                    .map_err(|_| Error::Validation("channel registry poisoned".into()))?;
                let engine = AuthorityEngine::new(store);
                let allowed = engine.check_bound_worker_action(
                    &channels,
                    &caller,
                    credential,
                    Some(grant.clone()),
                    action,
                    Some(target_task.unwrap_or(&binding.task_id)),
                )?;
                Ok(allowed.is_ok()
                    && engine.require_grant(&grant)?.mode >= GrantMode::ActWithApproval
                    && live_grant_chain(store, &grant, None)?)
            })
            .inspect_err(|_| {
                let _ = self.member_effect_denied("source_grant_lookup_refused", peer);
            })?;
        if !accepted {
            return Err(self.member_effect_denied("live_grant_refused", peer));
        }
        Ok((binding, grant))
    }

    pub(super) fn prepare_bound_member_execution(
        &self,
        request: ExecutionRequest,
        credential: &str,
        grant: Option<&str>,
        peer: herdr::embedding::TransportPeer,
    ) -> io::Result<ExecutionPlan> {
        let task = TaskId::from_str(&request.task_id)
            .map_err(|_| self.member_effect_denied("invalid_target_task", peer))?;
        let (source, parent) =
            self.authorize_member(credential, grant, peer, "dispatch_delegated", Some(&task))?;
        let child = GrantId::from_str(&request.grant_id)
            .map_err(|_| self.member_effect_denied("invalid_target_grant", peer))?;
        let lineage = self
            .with_store(|store| {
                // Check through the root, not just until the selected source
                // grant: revoking its ancestor must not leave a native lease.
                live_grant_chain(store, &child, Some(&parent))
            })
            .inspect_err(|_| {
                let _ = self.member_effect_denied("target_grant_lookup_refused", peer);
            })?;
        if !lineage {
            return Err(self.member_effect_denied("target_grant_not_live_descendant", peer));
        }
        // Reuses pause, target principal/task/location, idempotent intent and
        // real native spawn-confirm-exit. Never enters the controller path.
        let plan = self.prepare_execution(request).inspect_err(|_| {
            let _ = self.member_effect_denied("target_preparation_refused", peer);
        })?;
        self.with_store(|store| {
            events::append(store, NewEvent {
                domain: crate::server::DOMAIN_HOST, kind: "native_member_dispatch_prepared".into(),
                subject_type: "execution".into(), subject_id: plan.execution_id.clone(),
                origin: "native_member_process".into(),
                payload: serde_json::json!({"requester_execution_id": source.execution_id, "requester_member_id": source.member_id, "source_grant_id": parent, "target_grant_id": child, "target_execution_id": plan.execution_id, "replayed": plan.replayed, "intent_state": plan.intent_state, "claim_scope": "preparation_not_spawn_confirmation", "peer_pid": peer.pid, "peer_uid": peer.uid, "channel_id": source.channel_id, "human_presence_verified": false}),
            })?;
            Ok(())
        })?;
        Ok(plan)
    }

    pub(super) fn record_member_handoff(
        &self,
        credential: &str,
        grant: Option<&str>,
        detail: &str,
        task: Option<&str>,
        member: Option<&str>,
        peer: herdr::embedding::TransportPeer,
    ) -> io::Result<i64> {
        if detail.trim().is_empty() || detail.len() > 64 * 1024 {
            return Err(self.member_effect_denied("handoff_needs_1_to_65536_bytes", peer));
        }
        let (source, grant) =
            self.authorize_member(credential, grant, peer, "terminal_control", None)?;
        if task.is_some_and(|id| id != source.task_id.as_str())
            || member.is_some_and(|id| id != source.member_id.as_str())
        {
            return Err(self.member_effect_denied("handoff_scope_mismatch", peer));
        }
        // Dedupe the public, redacted claim; do not persist a dictionary-test
        // fingerprint of a sensitive raw summary.
        let detail = self.persistence.text(detail)?;
        let key = format!(
            "{:x}",
            Sha256::digest(format!("{}:{detail}", source.execution_id).as_bytes())
        );
        self.with_store(|store| {
            let prior = store.connection().query_row("SELECT seq FROM events WHERE kind='native_member_handoff' AND subject_id=?1 AND json_extract(payload,'$.request_key')=?2", rusqlite::params![source.task_id.as_str(), key], |row| row.get::<_,i64>(0)).optional()?;
            if let Some(seq) = prior { return Ok(seq); }
            Ok(events::append(store, NewEvent {
                domain: crate::server::DOMAIN_HOST, kind: "native_member_handoff".into(),
                subject_type: "task".into(), subject_id: source.task_id.to_string(), origin: "native_member_process".into(),
                payload: serde_json::json!({"execution_id": source.execution_id, "member_id": source.member_id, "task_id": source.task_id, "grant_id": grant, "request_key": key, "summary": detail, "source": "native_member_process", "peer_pid": peer.pid, "peer_uid": peer.uid, "channel_id": source.channel_id, "human_presence_verified": false, "claim_scope": "member_report_not_task_completion"}),
            })?.seq)
        })
    }
}
