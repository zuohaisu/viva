//! Backend-neutral preparation reused by runtime composition.
//! This does not authenticate a caller or spawn a process. The ingress must
//! first bind its caller, honor pause and call immediately before the effect;
//! a prepared intent is not a cached authorization lease or launch success.
use super::{ExecutionStart, TaskRegistry, TaskStatus};
use crate::{
    authority::{Actor, AuthorityEngine},
    foundation::{
        error::{Error, Result},
        ids::{GrantId, MemberId, TaskId},
        records::AttributionSnapshot,
    },
    members::MemberRegistry,
};
use std::path::Path;

pub struct DispatchRequest<'a> {
    pub task_id: &'a TaskId,
    pub member_id: &'a MemberId,
    pub grant_id: &'a GrantId,
    pub request_key: &'a str,
    pub cwd: &'a Path,
    pub worktree_id: Option<&'a str>,
}

impl TaskRegistry<'_> {
    /// Preserve the existing dispatch checks/attribution/idempotency without
    /// constructing the legacy Host or owning any PTY/backend.
    pub fn prepare_member_dispatch(&self, request: DispatchRequest<'_>) -> Result<ExecutionStart> {
        let DispatchRequest {
            task_id,
            member_id,
            grant_id,
            request_key,
            cwd,
            worktree_id,
        } = request;
        let members = MemberRegistry::new(self.store);
        members.require(member_id)?;
        let binding = members.binding(member_id)?.ok_or_else(|| {
            Error::Validation(format!(
                "member `{member_id}` has no model/tool binding; dispatch needs one"
            ))
        })?;
        let authority = AuthorityEngine::new(self.store);
        let grant = authority.require_grant(grant_id)?;
        if grant.principal_member_id.as_ref() != Some(member_id) {
            return Err(Error::Validation(format!(
                "dispatch denied: grant `{grant_id}` was not issued to member `{member_id}` \
                 (principal: {:?}) — grants are not transferable between members",
                grant
                    .principal_member_id
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "<none>".into())
            )));
        }
        match authority.check(
            &Actor::Member {
                member: member_id.clone(),
                grant: Some(grant_id.clone()),
            },
            "dispatch_delegated",
            Some(task_id),
        )? {
            Ok(()) => {}
            Err(reason) => return Err(Error::Validation(format!("dispatch denied: {reason}"))),
        }
        let task = self.require_task(task_id)?;
        if matches!(task.status, TaskStatus::Done | TaskStatus::Cancelled) {
            return Err(Error::Validation(format!(
                "task `{}` is {}; a finished task is never executed again",
                task.task_id,
                task.status.as_str()
            )));
        }
        if !cwd.is_absolute() {
            return Err(Error::Validation(format!(
                "dispatch cwd must be absolute: {}",
                cwd.display()
            )));
        }
        if let Some(worktree) = worktree_id {
            let known: i64 = self.store.connection().query_row(
                "SELECT COUNT(*) FROM task_worktrees WHERE worktree_id = ?1 AND released_at IS NULL",
                [worktree], |row| row.get(0))?;
            if known == 0 {
                return Err(Error::NotFound {
                    entity: "worktree",
                    id: worktree.to_string(),
                });
            }
        }
        let mut attribution = AttributionSnapshot::capture(
            member_id.clone(),
            binding.role,
            binding.model_binding,
            binding.tools.join(","),
        );
        if let Some(workspace) = task.workspace_id {
            attribution = attribution.in_workspace(workspace);
        }
        if let Some(project) = task.project_id {
            attribution = attribution.in_project(project);
        }
        let start = self.begin_execution(task_id, &attribution, request_key)?;
        if let ExecutionStart::Replayed(intent) = &start {
            if &intent.task_id != task_id {
                return Err(Error::Validation(format!(
                    "request key `{request_key}` already belongs to task `{}`; \
                     keys are replayed per task — use a different key for task `{task_id}`",
                    intent.task_id
                )));
            }
            if &intent.member_id != member_id {
                return Err(Error::Validation(format!(
                    "request key `{request_key}` was already used by member `{}`; \
                     keys are replayed per (task, member) — use a different key",
                    intent.member_id
                )));
            }
        }
        Ok(start)
    }
}
