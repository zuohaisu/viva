//! Team skill bodies with append-only versioning (issue #140, ADR-0013 Decision 6).
//!
//! Bodies live in SQLite; each write appends a version row. Rollback copies an
//! older body forward as a new version — history is never deleted.

use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{ExecutionId, utc_now};
use crate::foundation::store::{DOMAIN_SKILLS, MigrationRegistry, Store};

pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(DOMAIN_SKILLS, 1, "skills v1 body versions", SKILLS_V1_SQL)
}

pub const SKILLS_V1_SQL: &str = r#"
CREATE TABLE team_skills (
    skill_id        TEXT PRIMARY KEY,
    name            TEXT NOT NULL CHECK (length(trim(name)) > 0),
    current_version INTEGER NOT NULL CHECK (current_version >= 1),
    created_at      TEXT NOT NULL
);

CREATE TABLE skill_body_versions (
    skill_id     TEXT NOT NULL REFERENCES team_skills(skill_id),
    version      INTEGER NOT NULL CHECK (version >= 1),
    body         TEXT NOT NULL,
    write_kind   TEXT NOT NULL CHECK (write_kind IN ('create', 'modify', 'sync_rewrite', 'rollback')),
    operator     TEXT NOT NULL CHECK (length(trim(operator)) > 0),
    execution_id TEXT,
    recorded_at  TEXT NOT NULL,
    rollback_from_version INTEGER,
    PRIMARY KEY (skill_id, version),
    CHECK (
        (execution_id IS NOT NULL AND length(trim(execution_id)) > 0)
        OR operator = 'haisu:manual'
    ),
    CHECK (write_kind <> 'rollback' OR rollback_from_version IS NOT NULL)
);

CREATE INDEX skill_body_versions_skill_recorded
    ON skill_body_versions(skill_id, recorded_at);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillBodyWriteKind {
    Create,
    Modify,
    SyncRewrite,
    Rollback,
}

impl SkillBodyWriteKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Modify => "modify",
            Self::SyncRewrite => "sync_rewrite",
            Self::Rollback => "rollback",
        }
    }

    fn parse_stored(s: &str) -> Result<Self> {
        match s {
            "create" => Ok(Self::Create),
            "modify" => Ok(Self::Modify),
            "sync_rewrite" => Ok(Self::SyncRewrite),
            "rollback" => Ok(Self::Rollback),
            other => Err(Error::Validation(format!(
                "skill_body_versions.write_kind holds unknown value `{other}`"
            ))),
        }
    }
}

/// Where a write came from. Manual edits must not carry an execution id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillBodySource {
    Execution { execution_id: ExecutionId },
    HaisuManual,
}

impl SkillBodySource {
    pub fn operator_label(&self) -> String {
        match self {
            Self::Execution { execution_id } => format!("execution:{}", execution_id.as_str()),
            Self::HaisuManual => "haisu:manual".into(),
        }
    }

    fn execution_id(&self) -> Option<&ExecutionId> {
        match self {
            Self::Execution { execution_id } => Some(execution_id),
            Self::HaisuManual => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillRecord {
    pub skill_id: String,
    pub name: String,
    pub current_version: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillBodyVersion {
    pub skill_id: String,
    pub version: u32,
    pub body: String,
    pub write_kind: SkillBodyWriteKind,
    pub operator: String,
    pub execution_id: Option<ExecutionId>,
    pub recorded_at: String,
    pub rollback_from_version: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillBodyCurrent {
    pub skill: SkillRecord,
    pub version: SkillBodyVersion,
}

pub struct SkillBodyStore<'a> {
    store: &'a Store,
}

impl<'a> SkillBodyStore<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Create a team skill and its version 1 body.
    pub fn create(
        &self,
        name: impl Into<String>,
        body: impl Into<String>,
        source: SkillBodySource,
    ) -> Result<SkillBodyCurrent> {
        let name = name.into();
        let body = body.into();
        if body.trim().is_empty() {
            return Err(Error::Validation("skill body must not be empty".into()));
        }
        self.validate_source(&source)?;
        let skill_id = format!("skill-{}", uuid::Uuid::new_v4().simple());
        let now = utc_now();
        let conn = self.store.connection();
        conn.execute(
            "INSERT INTO team_skills(skill_id, name, current_version, created_at)
             VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![skill_id, name, now],
        )?;
        let version_row = self.insert_version(
            &skill_id,
            1,
            &body,
            SkillBodyWriteKind::Create,
            &source,
            None,
        )?;
        Ok(SkillBodyCurrent {
            skill: SkillRecord {
                skill_id: skill_id.clone(),
                name,
                current_version: 1,
                created_at: now,
            },
            version: version_row,
        })
    }

    fn fetch_skill(&self, skill_id: &str) -> Result<SkillRecord> {
        let mut stmt = self.store.connection().prepare(
            "SELECT skill_id, name, current_version, created_at
             FROM team_skills WHERE skill_id = ?1",
        )?;
        stmt.query_row([skill_id], |row| {
            Ok(SkillRecord {
                skill_id: row.get(0)?,
                name: row.get(1)?,
                current_version: row.get::<_, i64>(2)? as u32,
                created_at: row.get(3)?,
            })
        })
        .optional()?
        .ok_or_else(|| Error::NotFound {
            entity: "team_skill",
            id: skill_id.to_string(),
        })
    }

    /// Append a modified body (manual or execution-sourced).
    pub fn write_body(
        &self,
        skill_id: &str,
        body: impl Into<String>,
        write_kind: SkillBodyWriteKind,
        source: SkillBodySource,
    ) -> Result<SkillBodyCurrent> {
        let body = body.into();
        if body.trim().is_empty() {
            return Err(Error::Validation("skill body must not be empty".into()));
        }
        if write_kind == SkillBodyWriteKind::Create {
            return Err(Error::Validation(
                "use create() for the first version, not write_body".into(),
            ));
        }
        if write_kind == SkillBodyWriteKind::Rollback {
            return Err(Error::Validation(
                "rollback goes through rollback(); do not call write_body".into(),
            ));
        }
        self.validate_source(&source)?;
        let skill = self.fetch_skill(skill_id)?;
        let next = skill.current_version + 1;
        let version_row = self.insert_version(skill_id, next, &body, write_kind, &source, None)?;
        self.store.connection().execute(
            "UPDATE team_skills SET current_version = ?2 WHERE skill_id = ?1",
            rusqlite::params![skill_id, next as i64],
        )?;
        Ok(SkillBodyCurrent {
            skill: SkillRecord {
                current_version: next,
                ..skill
            },
            version: version_row,
        })
    }

    /// Sync rewrite path (#139): same as modify but tagged `sync_rewrite`.
    pub fn write_sync_rewrite(
        &self,
        skill_id: &str,
        body: impl Into<String>,
        source: SkillBodySource,
    ) -> Result<SkillBodyCurrent> {
        self.write_body(skill_id, body, SkillBodyWriteKind::SyncRewrite, source)
    }

    /// Current body — used by `viva skill get` and member projection (#114).
    pub fn current_body(&self, skill_id: &str) -> Result<Option<SkillBodyCurrent>> {
        let skill = match self.fetch_skill(skill_id).ok() {
            Some(s) => s,
            None => return Ok(None),
        };
        let version = self
            .fetch_version(skill_id, skill.current_version)?
            .ok_or_else(|| {
                Error::Validation(format!(
                    "team_skill {skill_id} points at missing version {}",
                    skill.current_version
                ))
            })?;
        Ok(Some(SkillBodyCurrent { skill, version }))
    }

    pub fn version_body(&self, skill_id: &str, version: u32) -> Result<Option<SkillBodyVersion>> {
        if self.fetch_skill(skill_id).is_err() {
            return Ok(None);
        }
        self.fetch_version(skill_id, version)
    }

    pub fn list_versions(&self, skill_id: &str) -> Result<Vec<SkillBodyVersion>> {
        self.fetch_skill(skill_id)?;
        let mut stmt = self.store.connection().prepare(
            "SELECT skill_id, version, body, write_kind, operator, execution_id, recorded_at,
                    rollback_from_version
             FROM skill_body_versions WHERE skill_id = ?1 ORDER BY version",
        )?;
        let rows = stmt.query_map([skill_id], map_version)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Restore `target_version` as the new current body; appends a rollback row.
    pub fn rollback(&self, skill_id: &str, target_version: u32) -> Result<SkillBodyCurrent> {
        let skill = self.fetch_skill(skill_id)?;
        let target = self
            .fetch_version(skill_id, target_version)?
            .ok_or_else(|| Error::NotFound {
                entity: "skill_body_version",
                id: format!("{skill_id}@{target_version}"),
            })?;
        if target_version == skill.current_version {
            return Err(Error::Validation(
                "rollback target is already the current version".into(),
            ));
        }
        let next = skill.current_version + 1;
        let source = SkillBodySource::HaisuManual;
        let version_row = self.insert_version(
            skill_id,
            next,
            &target.body,
            SkillBodyWriteKind::Rollback,
            &source,
            Some(target_version),
        )?;
        self.store.connection().execute(
            "UPDATE team_skills SET current_version = ?2 WHERE skill_id = ?1",
            rusqlite::params![skill_id, next as i64],
        )?;
        Ok(SkillBodyCurrent {
            skill: SkillRecord {
                current_version: next,
                ..skill
            },
            version: version_row,
        })
    }

    /// Write the current body to `dest` for harness projection (#114).
    pub fn write_current_projection_file(
        &self,
        skill_id: &str,
        dest: &std::path::Path,
    ) -> Result<()> {
        let current = self
            .current_body(skill_id)?
            .ok_or_else(|| Error::NotFound {
                entity: "team_skill",
                id: skill_id.to_string(),
            })?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, current.version.body.as_bytes())?;
        Ok(())
    }

    fn validate_source(&self, source: &SkillBodySource) -> Result<()> {
        match source {
            SkillBodySource::HaisuManual => Ok(()),
            SkillBodySource::Execution { execution_id } => {
                if execution_id.as_str().trim().is_empty() {
                    return Err(Error::Validation(
                        "execution-sourced writes need a non-empty execution id".into(),
                    ));
                }
                Ok(())
            }
        }
    }

    fn insert_version(
        &self,
        skill_id: &str,
        version: u32,
        body: &str,
        write_kind: SkillBodyWriteKind,
        source: &SkillBodySource,
        rollback_from: Option<u32>,
    ) -> Result<SkillBodyVersion> {
        let operator = source.operator_label();
        let execution_id = source.execution_id().map(|id| id.as_str().to_string());
        let recorded_at = utc_now();
        self.store.connection().execute(
            "INSERT INTO skill_body_versions(
                skill_id, version, body, write_kind, operator, execution_id,
                recorded_at, rollback_from_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                skill_id,
                version as i64,
                body,
                write_kind.as_str(),
                operator,
                execution_id,
                recorded_at,
                rollback_from.map(|v| v as i64),
            ],
        )?;
        Ok(SkillBodyVersion {
            skill_id: skill_id.to_string(),
            version,
            body: body.to_string(),
            write_kind,
            operator,
            execution_id: source.execution_id().cloned(),
            recorded_at,
            rollback_from_version: rollback_from,
        })
    }

    fn fetch_version(&self, skill_id: &str, version: u32) -> Result<Option<SkillBodyVersion>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT skill_id, version, body, write_kind, operator, execution_id, recorded_at,
                    rollback_from_version
             FROM skill_body_versions WHERE skill_id = ?1 AND version = ?2",
        )?;
        stmt.query_row(rusqlite::params![skill_id, version as i64], map_version)
            .optional()
            .map_err(Into::into)
    }
}

fn map_version(row: &rusqlite::Row<'_>) -> rusqlite::Result<SkillBodyVersion> {
    let execution_raw: Option<String> = row.get(5)?;
    let execution_id = execution_raw
        .as_deref()
        .map(ExecutionId::from_str)
        .transpose()
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(e))
        })?;
    let rollback_from: Option<i64> = row.get(7)?;
    Ok(SkillBodyVersion {
        skill_id: row.get(0)?,
        version: row.get::<_, i64>(1)? as u32,
        body: row.get(2)?,
        write_kind: SkillBodyWriteKind::parse_stored(&row.get::<_, String>(3)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        operator: row.get(4)?,
        execution_id,
        recorded_at: row.get(6)?,
        rollback_from_version: rollback_from.map(|v| v as u32),
    })
}
