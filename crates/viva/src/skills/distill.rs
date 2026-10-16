//! Distill team skills from successful executions (issue #141, ADR-0013 Decision 6).
//!
//! Trigger rules are explicit — the model does not invent categories:
//! - one completed execution with ≥ 3 recorded steps; or
//! - the same member reaches 2 successes for a caller-supplied task-type key.

use std::path::PathBuf;
use std::process::Command;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{ExecutionId, MemberId, TaskId};
use crate::foundation::paths;
use crate::foundation::records::{ExecutionStatus, get_execution};
use crate::foundation::store::Store;
use crate::knowledge::{KnowledgeRegistry, SkillSetService};
use crate::tasks::TaskRegistry;

use super::{SkillBodySource, SkillBodyStore};

pub const SKILLS_V2_DISTILL_SQL: &str = r#"
CREATE TABLE skill_distill_task_successes (
    member_id       TEXT NOT NULL,
    task_type_key   TEXT NOT NULL,
    execution_id    TEXT NOT NULL,
    recorded_at     TEXT NOT NULL,
    PRIMARY KEY (member_id, task_type_key, execution_id)
);

CREATE INDEX skill_distill_member_type
    ON skill_distill_task_successes(member_id, task_type_key);

CREATE TABLE skill_distill_outcomes (
    execution_id TEXT PRIMARY KEY,
    skill_id     TEXT NOT NULL,
    recorded_at  TEXT NOT NULL
);
"#;

const MIN_STEPS_FOR_DISTILL: usize = 3;
const MIN_SUCCESSES_FOR_TASK_TYPE: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DistillOutcome {
    Created {
        skill_id: String,
        name: String,
        execution_id: ExecutionId,
        member_id: MemberId,
        body_version: u32,
        trigger: DistillTrigger,
    },
    AlreadyDistilled {
        skill_id: String,
        execution_id: ExecutionId,
    },
    NotEligible {
        execution_id: ExecutionId,
        reason: String,
        step_count: usize,
        task_type_successes: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistillTrigger {
    StepCount {
        steps: usize,
    },
    RepeatedTaskType {
        task_type_key: String,
        successes: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistillContext {
    pub execution_id: ExecutionId,
    pub member_id: MemberId,
    pub task_id: TaskId,
    pub task_goal: String,
    pub completion_evidence: String,
    pub step_count: usize,
    pub step_summaries: Vec<String>,
}

pub struct DistillService<'a> {
    store: &'a Store,
    home: PathBuf,
}

impl<'a> DistillService<'a> {
    pub fn new(store: &'a Store, home: impl Into<PathBuf>) -> Self {
        Self {
            store,
            home: home.into(),
        }
    }

    /// Propose and persist a team skill from a completed execution.
    pub fn distill_from_execution(
        &self,
        execution_id: &ExecutionId,
        task_type_key: Option<&str>,
    ) -> Result<DistillOutcome> {
        if let Some(existing) = self.existing_outcome(execution_id)? {
            return Ok(DistillOutcome::AlreadyDistilled {
                skill_id: existing,
                execution_id: execution_id.clone(),
            });
        }

        let execution =
            get_execution(self.store, execution_id)?.ok_or_else(|| Error::NotFound {
                entity: "execution",
                id: execution_id.as_str().to_string(),
            })?;
        if execution.status != ExecutionStatus::Completed {
            return Err(Error::Validation(format!(
                "skill distill requires a completed execution; got {}",
                execution.status.as_str()
            )));
        }
        let evidence = execution.completion_evidence.clone().unwrap_or_default();
        if evidence.trim().is_empty() {
            return Err(Error::Validation(
                "completed execution is missing completion evidence".into(),
            ));
        }

        let member_id = execution.attribution.member_id.clone();
        let task_id = execution.task_id.clone();
        let tasks = TaskRegistry::new(self.store);
        let task = tasks.get_task(&task_id)?.ok_or_else(|| Error::NotFound {
            entity: "task",
            id: task_id.as_str().to_string(),
        })?;
        let step_count = self.count_steps_for_execution(execution_id, &task_id)?;
        let type_key = task_type_key
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| task_id.as_str().to_string());

        self.record_task_success(&member_id, &type_key, execution_id)?;

        let successes = self.task_type_success_count(&member_id, &type_key)?;
        let trigger = if step_count >= MIN_STEPS_FOR_DISTILL {
            Some(DistillTrigger::StepCount { steps: step_count })
        } else if successes >= MIN_SUCCESSES_FOR_TASK_TYPE {
            Some(DistillTrigger::RepeatedTaskType {
                task_type_key: type_key.clone(),
                successes,
            })
        } else {
            None
        };
        let Some(trigger) = trigger else {
            return Ok(DistillOutcome::NotEligible {
                execution_id: execution_id.clone(),
                reason: format!(
                    "need ≥{MIN_STEPS_FOR_DISTILL} steps or ≥{MIN_SUCCESSES_FOR_TASK_TYPE} \
                     successes for task type `{type_key}`"
                ),
                step_count,
                task_type_successes: successes,
            });
        };

        let context = DistillContext {
            execution_id: execution_id.clone(),
            member_id: member_id.clone(),
            task_id: task_id.clone(),
            task_goal: task.goal.clone(),
            completion_evidence: evidence,
            step_count,
            step_summaries: self.step_summaries_for_execution(execution_id, &task_id)?,
        };
        let draft = generate_skill_draft(&context)?;
        if !draft_contains_execution_source(&draft, execution_id) {
            return Err(Error::Validation(
                "distilled draft must cite its source execution; refusing to write".into(),
            ));
        }

        let name = distill_skill_name(&task.goal);
        let source = SkillBodySource::Execution {
            execution_id: execution_id.clone(),
        };
        let bodies = SkillBodyStore::new(self.store);
        let created = bodies.create(&name, draft, source)?;
        let skill_id = created.skill.skill_id.clone();

        let entry_path = self.project_skill_file(&skill_id, &created.version.body)?;
        let provenance = format!("distilled from execution {}", execution_id.as_str());
        KnowledgeRegistry::new(self.store).register_skill_with_id(
            &skill_id,
            &name,
            entry_path,
            provenance,
            None,
            vec!["distilled".into()],
        )?;

        SkillSetService::new(self.store).sync_existing(
            &member_id,
            &skill_id,
            Some(execution_id),
        )?;

        self.record_outcome(execution_id, &skill_id)?;

        Ok(DistillOutcome::Created {
            skill_id,
            name,
            execution_id: execution_id.clone(),
            member_id,
            body_version: created.skill.current_version,
            trigger,
        })
    }

    fn existing_outcome(&self, execution_id: &ExecutionId) -> Result<Option<String>> {
        let mut stmt = self
            .store
            .connection()
            .prepare("SELECT skill_id FROM skill_distill_outcomes WHERE execution_id = ?1")?;
        stmt.query_row([execution_id.as_str()], |row| row.get(0))
            .optional()
            .map_err(Into::into)
    }

    fn record_outcome(&self, execution_id: &ExecutionId, skill_id: &str) -> Result<()> {
        self.store.connection().execute(
            "INSERT INTO skill_distill_outcomes(execution_id, skill_id, recorded_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![
                execution_id.as_str(),
                skill_id,
                crate::foundation::ids::utc_now()
            ],
        )?;
        Ok(())
    }

    fn record_task_success(
        &self,
        member: &MemberId,
        task_type_key: &str,
        execution_id: &ExecutionId,
    ) -> Result<()> {
        self.store.connection().execute(
            "INSERT OR IGNORE INTO skill_distill_task_successes(
                member_id, task_type_key, execution_id, recorded_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                member.as_str(),
                task_type_key,
                execution_id.as_str(),
                crate::foundation::ids::utc_now()
            ],
        )?;
        Ok(())
    }

    fn task_type_success_count(&self, member: &MemberId, task_type_key: &str) -> Result<usize> {
        let count: i64 = self.store.connection().query_row(
            "SELECT COUNT(1) FROM skill_distill_task_successes
             WHERE member_id = ?1 AND task_type_key = ?2",
            rusqlite::params![member.as_str(), task_type_key],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    fn count_steps_for_execution(
        &self,
        execution_id: &ExecutionId,
        task_id: &TaskId,
    ) -> Result<usize> {
        let by_execution: i64 = self.store.connection().query_row(
            "SELECT COUNT(1) FROM task_results WHERE execution_id = ?1",
            [execution_id.as_str()],
            |row| row.get(0),
        )?;
        if by_execution > 0 {
            return Ok(by_execution as usize);
        }
        let by_task: i64 = self.store.connection().query_row(
            "SELECT COUNT(1) FROM computer_actions
             WHERE task_id = ?1 AND state = 'executed'",
            [task_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(by_task as usize)
    }

    fn step_summaries_for_execution(
        &self,
        execution_id: &ExecutionId,
        task_id: &TaskId,
    ) -> Result<Vec<String>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT kind, payload FROM task_results
             WHERE execution_id = ?1 ORDER BY recorded_at, result_id",
        )?;
        let rows = stmt.query_map([execution_id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (kind, payload) = row?;
            out.push(format!("{kind}: {payload}"));
        }
        if !out.is_empty() {
            return Ok(out);
        }
        let mut stmt = self.store.connection().prepare(
            "SELECT tool, target, reason FROM computer_actions
             WHERE task_id = ?1 AND state = 'executed' ORDER BY rowid",
        )?;
        let rows = stmt.query_map([task_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (tool, target, reason) = row?;
            out.push(format!("{tool} on {target} ({reason})"));
        }
        Ok(out)
    }

    fn project_skill_file(&self, skill_id: &str, body: &str) -> Result<PathBuf> {
        let dir = paths::ensure_private_dir(&self.home.join("skills").join(skill_id))?;
        let entry = dir.join("SKILL.md");
        std::fs::write(&entry, body.as_bytes())?;
        entry
            .canonicalize()
            .map_err(|e| Error::Validation(format!("could not canonicalize skill path: {e}")))
    }
}

fn distill_skill_name(goal: &str) -> String {
    let trimmed = goal.trim();
    if trimmed.is_empty() {
        return "distilled skill".into();
    }
    let mut name = trimmed.lines().next().unwrap_or(trimmed).to_string();
    if name.chars().count() > 80 {
        name = name.chars().take(77).collect::<String>() + "...";
    }
    name
}

fn draft_contains_execution_source(body: &str, execution_id: &ExecutionId) -> bool {
    body.contains(execution_id.as_str())
}

/// Cheap-model path: optional external command, otherwise a deterministic draft
/// from execution facts (used in tests and offline runs).
pub fn generate_skill_draft(context: &DistillContext) -> Result<String> {
    if let Ok(cmd) = std::env::var("VIVA_SKILL_DISTILL_CMD") {
        if !cmd.trim().is_empty() {
            return generate_via_command(&cmd, context);
        }
    }
    Ok(cheap_draft_from_context(context))
}

fn generate_via_command(command: &str, context: &DistillContext) -> Result<String> {
    let input = serde_json::to_string(context)?;
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("VIVA_SKILL_DISTILL_INPUT", input)
        .output()
        .map_err(|e| Error::Validation(format!("VIVA_SKILL_DISTILL_CMD failed to start: {e}")))?;
    if !output.status.success() {
        return Err(Error::Validation(format!(
            "VIVA_SKILL_DISTILL_CMD exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let body = String::from_utf8(output.stdout)
        .map_err(|e| Error::Validation(format!("distill command stdout was not utf-8: {e}")))?;
    if body.trim().is_empty() {
        return Err(Error::Validation(
            "distill command returned an empty body".into(),
        ));
    }
    Ok(body)
}

fn cheap_draft_from_context(context: &DistillContext) -> String {
    let mut lines = vec![
        "---".to_string(),
        "name: distilled-skill".to_string(),
        format!("source_execution: {}", context.execution_id.as_str()),
        "---".to_string(),
        String::new(),
        format!("# {}", distill_skill_name(&context.task_goal)),
        String::new(),
        "## Source".to_string(),
        format!(
            "Distilled from execution `{}` (task `{}`).",
            context.execution_id.as_str(),
            context.task_id.as_str()
        ),
        String::new(),
        "## Outcome".to_string(),
        context.completion_evidence.clone(),
        String::new(),
        "## Observed steps".to_string(),
    ];
    if context.step_summaries.is_empty() {
        lines.push(format!(
            "- {} recorded step(s) in task results or computer actions.",
            context.step_count
        ));
    } else {
        for (idx, step) in context.step_summaries.iter().enumerate() {
            lines.push(format!("{}. {step}", idx + 1));
        }
    }
    lines.push(String::new());
    lines.push("This document is a team skill draft — not a knowledge entry or principle.".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::*;

    #[test]
    fn cheap_draft_cites_execution() {
        let ctx = DistillContext {
            execution_id: ExecutionId::from_str("exec-test-1").unwrap(),
            member_id: MemberId::new(),
            task_id: TaskId::from_str("task-1").unwrap(),
            task_goal: "Ship the feature".into(),
            completion_evidence: "tests green".into(),
            step_count: 3,
            step_summaries: vec!["a".into(), "b".into(), "c".into()],
        };
        let body = cheap_draft_from_context(&ctx);
        assert!(draft_contains_execution_source(&body, &ctx.execution_id));
    }
}
