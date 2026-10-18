//! Member callback CLI (issue #117).
//!
//! Recall, remember, dialog state, and skill reads run as the member named
//! by the current execution and its live grant. A `--member` flag that
//! disagrees with that execution is a refusal. Dialog state is only what
//! Haisu or the command wrote; it cannot widen ADR-0010 visibility.

use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::authority::{
    ACTION_CALLBACK_REMEMBER, ACTION_CALLBACK_STATE_WRITE, Actor, AuthorityEngine, DenialReason,
};
use crate::foundation::error::{Error, Result};
use crate::foundation::events::{self, NewEvent};
use crate::foundation::ids::{ExecutionId, GrantId, MemberId, ProjectId};
use crate::foundation::records::{
    AttributionSnapshot, ExecutionRecord, get_execution, insert_execution,
};
use crate::foundation::store::{DOMAIN_AUTHORITY, DOMAIN_MEMORY, Store};
use crate::harness::pi::{ENV_EXECUTION_ID, ENV_GRANT_ID};
use crate::members::{MemberBinding, MemberRecord};
use crate::memory::{KnowledgeKind, MemoryService, RecallFilter, RecalledFact};
use crate::tasks::TaskRecord;

const RECALL_BUDGET: usize = 16 * 1024;

/// True when this process was launched inside a member execution or was
/// handed a grant. Either signal selects the callback gate; a half-set
/// environment must not fall through to a trusted `--member`.
pub fn context_active() -> bool {
    env_present(ENV_EXECUTION_ID) || env_present(ENV_GRANT_ID)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackIdentity {
    pub member_id: MemberId,
    pub execution_id: ExecutionId,
    pub grant_id: GrantId,
    pub task_id: crate::foundation::ids::TaskId,
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogState {
    pub execution_id: ExecutionId,
    pub member_id: MemberId,
    pub domain: String,
    pub project_id: Option<ProjectId>,
    pub interlocutor: Option<String>,
    pub activity: Option<String>,
    pub since_at: String,
    pub written_by: String,
}

pub enum RecallFinish {
    /// `not_ready`, `no_match`, or `matched`. Exit 0.
    Done(Value),
    /// `backend_error`. The caller prints this and exits non-zero.
    Backend(Value),
}

/// Start the execution a harness session will call back as.
pub fn open_member_execution(
    store: &Store,
    member: &MemberRecord,
    task: &TaskRecord,
    binding: Option<&MemberBinding>,
    tool_binding: &str,
) -> Result<ExecutionRecord> {
    let mut attribution = AttributionSnapshot::capture(
        member.member_id.clone(),
        binding.map(|b| b.role.as_str()).unwrap_or("member"),
        binding.map(|b| b.model_binding.as_str()).unwrap_or(""),
        tool_binding,
    );
    if let Some(project) = &task.project_id {
        attribution = attribution.in_project(project.clone());
    }
    let execution = ExecutionRecord::start(task.task_id.clone(), attribution);
    insert_execution(store, &execution)?;
    Ok(execution)
}

pub fn authorize(
    store: &Store,
    action: &str,
    claimed_member: Option<&str>,
) -> Result<CallbackIdentity> {
    let Some(execution_text) = env_value(ENV_EXECUTION_ID) else {
        return Err(refuse_text(
            store,
            "execution:missing",
            action,
            "callback identity requires VIVA_EXECUTION_ID from the current execution",
        ));
    };
    let execution_id = ExecutionId::from_str(&execution_text).map_err(|_| {
        refuse_text(
            store,
            "execution:invalid",
            action,
            "VIVA_EXECUTION_ID is not an execution id",
        )
    })?;
    let execution = get_execution(store, &execution_id)?.ok_or_else(|| {
        refuse_text(
            store,
            &format!("execution:{execution_text}"),
            action,
            "VIVA_EXECUTION_ID does not name an execution",
        )
    })?;
    let member_id = execution.attribution.member_id.clone();
    if let Some(claimed) = claimed_member.map(str::trim).filter(|s| !s.is_empty()) {
        if claimed != member_id.as_str() {
            let grant_note = env_value(ENV_GRANT_ID).unwrap_or_else(|| "none".into());
            return Err(refuse_text(
                store,
                &format!("member:{claimed} grant:{grant_note}"),
                action,
                &DenialReason::NotPrincipal.to_string(),
            ));
        }
    }

    let authority = AuthorityEngine::new(store);
    let Some(grant_text) = env_value(ENV_GRANT_ID) else {
        let actor = Actor::Member {
            member: member_id,
            grant: None,
        };
        refuse_check(store, &authority, &actor, action, None)?;
        return Err(Error::Validation(
            "authority.refused: callback requires a live grant".into(),
        ));
    };
    let grant_id = match GrantId::from_str(&grant_text) {
        Ok(id) => id,
        Err(_) => {
            return Err(refuse_text(
                store,
                &format!("member:{} grant:{grant_text}", member_id.as_str()),
                action,
                &DenialReason::NoGrant.to_string(),
            ));
        }
    };
    if authority.get_grant(&grant_id)?.is_none() {
        return Err(refuse_text(
            store,
            &format!("member:{} grant:{}", member_id.as_str(), grant_id.as_str()),
            action,
            &DenialReason::NoGrant.to_string(),
        ));
    }
    let actor = Actor::Member {
        member: member_id.clone(),
        grant: Some(grant_id.clone()),
    };
    refuse_check(store, &authority, &actor, action, Some(&execution.task_id))?;
    let tasks = crate::tasks::TaskRegistry::new(store);
    let project_id = tasks
        .require_task(&execution.task_id)
        .ok()
        .and_then(|task| task.project_id);
    Ok(CallbackIdentity {
        member_id,
        execution_id,
        grant_id,
        task_id: execution.task_id,
        project_id,
    })
}

pub fn recall(
    store: &Store,
    query: Option<&str>,
    claimed_member: Option<&str>,
    claimed_project: Option<&str>,
) -> Result<RecallFinish> {
    let identity = authorize(
        store,
        crate::authority::ACTION_CALLBACK_RECALL,
        claimed_member,
    )?;
    let query = query.unwrap_or("").trim();
    if query.is_empty() {
        return Err(Error::Validation("memory recall needs --query".into()));
    }
    let project = match claimed_project {
        Some(text) => {
            let project = ProjectId::from_str(text)?;
            if identity.project_id.as_ref() != Some(&project) {
                return Err(refuse_text(
                    store,
                    &format!("member:{}", identity.member_id.as_str()),
                    crate::authority::ACTION_CALLBACK_RECALL,
                    "dialog scope cannot name a project this execution is not on",
                ));
            }
            Some(project)
        }
        None => identity.project_id.clone(),
    };

    let service = MemoryService::new(store);
    let ready = match service.curation_ready() {
        Ok(ready) => ready,
        Err(err) => return Ok(backend(err.to_string())),
    };
    if !ready {
        return Ok(RecallFinish::Done(json!({
            "result": "not_ready",
            "facts": [],
        })));
    }

    let include_personal = match load_state(store, &identity.execution_id) {
        Ok(state) => state.is_some_and(|row| row.domain == "personal"),
        Err(err) => return Ok(backend(err.to_string())),
    };
    let shared = RecallFilter {
        include_personal: false,
        include_project: true,
        include_team: true,
        ..RecallFilter::default()
    };
    let mut facts = match service.search_filtered(
        &identity.member_id,
        project.as_ref(),
        query,
        RECALL_BUDGET,
        &shared,
    ) {
        Ok(crate::memory::MemorySearch::Fetched { facts, .. }) => facts,
        Ok(crate::memory::MemorySearch::Unavailable { reason }) => return Ok(backend(reason)),
        Err(Error::Storage(err)) => return Ok(backend(err.to_string())),
        Err(err) => return Err(err),
    };
    if include_personal {
        let personal = RecallFilter {
            include_personal: true,
            include_project: false,
            include_team: false,
            ..RecallFilter::default()
        };
        match service.search_filtered(&identity.member_id, None, query, RECALL_BUDGET, &personal) {
            Ok(crate::memory::MemorySearch::Fetched { facts: own, .. }) => {
                for fact in own {
                    if !facts.iter().any(|have| have.fact_id == fact.fact_id) {
                        facts.push(fact);
                    }
                }
            }
            Ok(crate::memory::MemorySearch::Unavailable { reason }) => {
                return Ok(backend(reason));
            }
            Err(Error::Storage(err)) => return Ok(backend(err.to_string())),
            Err(err) => return Err(err),
        }
    }
    if facts.is_empty() {
        return Ok(RecallFinish::Done(json!({
            "result": "no_match",
            "facts": [],
        })));
    }
    Ok(RecallFinish::Done(json!({
        "result": "matched",
        "facts": facts.iter().map(fact_json).collect::<Vec<_>>(),
    })))
}

pub struct RememberInput<'a> {
    pub content: &'a str,
    pub source: Option<&'a str>,
    pub category: &'a str,
    pub tags: &'a str,
    pub kind: KnowledgeKind,
    pub emphasis: bool,
    pub claimed_member: Option<&'a str>,
    pub claimed_project: Option<&'a str>,
}

pub fn remember(store: &Store, input: RememberInput<'_>) -> Result<Value> {
    let identity = authorize(store, ACTION_CALLBACK_REMEMBER, input.claimed_member)?;
    let project = match input.claimed_project {
        Some(text) => {
            let project = ProjectId::from_str(text)?;
            if identity.project_id.as_ref() != Some(&project) {
                return Err(refuse_text(
                    store,
                    &format!("member:{}", identity.member_id.as_str()),
                    ACTION_CALLBACK_REMEMBER,
                    "cannot write project knowledge outside this execution's project",
                ));
            }
            Some(project)
        }
        None => None,
    };
    if matches!(input.kind, KnowledgeKind::ProjectKnowledge) && project.is_none() {
        return Err(Error::Validation(
            "project_knowledge requires --project, and it must be this execution's project".into(),
        ));
    }
    let service = MemoryService::new(store);
    if !service.curation_ready()? {
        return Err(Error::Validation(
            "remember only creates a proposed candidate; the curation ledger is not ready".into(),
        ));
    }
    let provenance = json!({
        "execution_id": identity.execution_id.as_str(),
        "grant_id": identity.grant_id.as_str(),
        "note": input.source.unwrap_or(""),
    })
    .to_string();
    let source = format!("execution:{}", identity.execution_id.as_str());
    let result = service.remember_with_options(
        &identity.member_id,
        project.as_ref(),
        input.content,
        &source,
        input.category,
        input.tags,
        input.kind,
        input.emphasis,
        Some(&provenance),
        None,
    )?;
    if result.candidate_status != "proposed" || result.fact_id.is_some() {
        return Err(Error::Validation(
            "remember must stay proposed and must not become knowledge".into(),
        ));
    }
    Ok(json!({
        "candidate_id": result.candidate_id,
        "candidate_status": result.candidate_status,
        "execution_id": identity.execution_id.as_str(),
        "fact_id": Value::Null,
        "link": Value::Null,
    }))
}

pub fn read_state(store: &Store, claimed_member: Option<&str>) -> Result<Value> {
    let identity = authorize(
        store,
        crate::authority::ACTION_CALLBACK_STATE_READ,
        claimed_member,
    )?;
    match load_state(store, &identity.execution_id)? {
        Some(state) => Ok(json!({
            "result": "set",
            "state": state_json(&state),
        })),
        None => Ok(json!({
            "result": "unset",
            "state": Value::Null,
        })),
    }
}

pub struct StateWrite<'a> {
    pub domain: &'a str,
    pub project: Option<&'a str>,
    pub interlocutor: Option<&'a str>,
    pub activity: Option<&'a str>,
    pub written_by: &'a str,
    pub claimed_member: Option<&'a str>,
}

pub fn write_state(store: &Store, input: StateWrite<'_>) -> Result<Value> {
    let identity = authorize(store, ACTION_CALLBACK_STATE_WRITE, input.claimed_member)?;
    let domain = input.domain.trim();
    if domain != "work" && domain != "personal" {
        return Err(Error::Validation(
            "dialog state domain must be work or personal; it cannot widen visibility".into(),
        ));
    }
    let written_by = input.written_by.trim();
    if written_by != "command" && written_by != "haisu" {
        return Err(Error::Validation(
            "dialog state written_by must be command or haisu".into(),
        ));
    }
    let project = match input.project {
        Some(text) => {
            let project = ProjectId::from_str(text)?;
            if identity.project_id.as_ref() != Some(&project) {
                return Err(refuse_text(
                    store,
                    &format!("member:{}", identity.member_id.as_str()),
                    ACTION_CALLBACK_STATE_WRITE,
                    "a work session cannot take a project domain it is not on",
                ));
            }
            Some(project)
        }
        None => None,
    };
    // Personal is the visibility ceiling (ADR-0010): own personal memories
    // only, and only after an explicit personal write. Nothing here grants
    // another member's personal domain or a project outside the execution.
    if domain == "work" && project.is_some() && identity.project_id.is_none() {
        return Err(refuse_text(
            store,
            &format!("member:{}", identity.member_id.as_str()),
            ACTION_CALLBACK_STATE_WRITE,
            "cannot attach a project to an execution that has none",
        ));
    }
    let now = crate::foundation::ids::utc_now();
    let execution =
        get_execution(store, &identity.execution_id)?.ok_or_else(|| Error::NotFound {
            entity: "execution",
            id: identity.execution_id.to_string(),
        })?;
    store.connection().execute(
        "INSERT INTO dialog_states(
            execution_id, session_id, member_id, domain, project_id, interlocutor,
            activity, since_at, written_by, recorded_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(execution_id) DO UPDATE SET
            domain = excluded.domain,
            project_id = excluded.project_id,
            interlocutor = excluded.interlocutor,
            activity = excluded.activity,
            since_at = excluded.since_at,
            written_by = excluded.written_by,
            recorded_at = excluded.recorded_at",
        rusqlite::params![
            identity.execution_id.as_str(),
            execution.session_id.as_ref().map(|s| s.as_str()),
            identity.member_id.as_str(),
            domain,
            project.as_ref().map(|p| p.as_str()),
            input.interlocutor,
            input.activity,
            now,
            written_by,
            now,
        ],
    )?;
    let _ = events::append(
        store,
        NewEvent {
            domain: DOMAIN_MEMORY,
            kind: "dialog_state_set".into(),
            subject_type: "execution".into(),
            subject_id: identity.execution_id.to_string(),
            origin: "cli".into(),
            payload: json!({
                "domain": domain,
                "written_by": written_by,
                "member_id": identity.member_id.as_str(),
            }),
        },
    );
    match load_state(store, &identity.execution_id)? {
        Some(state) => Ok(json!({
            "result": "set",
            "state": state_json(&state),
        })),
        None => Err(Error::Validation(
            "dialog state was not stored on this execution".into(),
        )),
    }
}

/// Owner memory commands (admit, archive, link, …) are not member callbacks.
/// Identity is still checked first, so a forged member is the same refusal
/// as recall; a matching member is then refused because the command itself
/// is outside the callback.
pub fn refuse_owner_command(
    store: &Store,
    command: &str,
    claimed_member: Option<&str>,
) -> Result<()> {
    let identity = authorize(
        store,
        crate::authority::ACTION_CALLBACK_RECALL,
        claimed_member,
    )?;
    Err(refuse_text(
        store,
        &format!("member:{}", identity.member_id.as_str()),
        command,
        "member sessions use recall, remember, and state; owner memory commands stay outside the callback",
    ))
}

fn load_state(store: &Store, execution_id: &ExecutionId) -> Result<Option<DialogState>> {
    let mut stmt = store.connection().prepare(
        "SELECT member_id, domain, project_id, interlocutor, activity, since_at, written_by
         FROM dialog_states WHERE execution_id = ?1",
    )?;
    let row = stmt
        .query_row([execution_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .optional()?;
    let Some((member, domain, project, interlocutor, activity, since_at, written_by)) = row else {
        return Ok(None);
    };
    Ok(Some(DialogState {
        execution_id: execution_id.clone(),
        member_id: MemberId::from_str(&member)?,
        domain,
        project_id: project.map(|p| ProjectId::from_str(&p)).transpose()?,
        interlocutor,
        activity,
        since_at,
        written_by,
    }))
}

fn fact_json(fact: &RecalledFact) -> Value {
    json!({
        "fact_id": fact.fact_id,
        "content": fact.content,
        "summary": fact.summary,
        "knowledge_kind": fact.knowledge_kind,
        "member_id": fact.member_id.as_str(),
        "project_id": fact.project_id.as_ref().map(|p| p.as_str()),
        "source": fact.source,
    })
}

fn state_json(state: &DialogState) -> Value {
    json!({
        "execution_id": state.execution_id.as_str(),
        "member_id": state.member_id.as_str(),
        "domain": state.domain,
        "project_id": state.project_id.as_ref().map(|p| p.as_str()),
        "interlocutor": state.interlocutor,
        "activity": state.activity,
        "since": state.since_at,
        "written_by": state.written_by,
    })
}

fn backend(reason: String) -> RecallFinish {
    RecallFinish::Backend(json!({
        "result": "backend_error",
        "facts": [],
        "reason": reason,
    }))
}

fn env_present(key: &str) -> bool {
    env_value(key).is_some()
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn refuse_check(
    store: &Store,
    authority: &AuthorityEngine<'_>,
    actor: &Actor,
    action: &str,
    task_id: Option<&crate::foundation::ids::TaskId>,
) -> Result<()> {
    match authority.check(actor, action, task_id)? {
        Ok(()) => Ok(()),
        Err(reason) => Err(note_event(store, action, &reason.to_string())),
    }
}

fn refuse_text(store: &Store, subject: &str, action: &str, reason: &str) -> Error {
    let _ = AuthorityEngine::new(store).audit_denial(subject, action, reason);
    note_event(store, action, reason)
}

fn note_event(store: &Store, action: &str, reason: &str) -> Error {
    let subject = env_value(ENV_EXECUTION_ID).unwrap_or_else(|| "missing".into());
    let _ = events::append(
        store,
        NewEvent {
            domain: DOMAIN_AUTHORITY,
            kind: "authority.refused".into(),
            subject_type: "callback".into(),
            subject_id: subject,
            origin: "cli".into(),
            payload: json!({
                "action": action,
                "reason": reason,
            }),
        },
    );
    Error::Validation(format!("authority.refused: {action}: {reason}"))
}
