//! Workspaces: the primary work context that groups projects (V02, #11).
//!
//! A workspace is an office-level selection of work context. Members and
//! tasks reference workspaces by id; switching a workspace never rewrites
//! history (V03 attributes executions at creation time).

use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{WorkspaceId, utc_now};
use crate::foundation::store::{DOMAIN_WORKSPACES_PROJECTS, MigrationRegistry, Store};

/// Shared schema: workspaces and projects share the `workspaces_projects`
/// domain migration (both are V02's namespace per the G0 contract §5), so
/// one version carries both table families. The project-side tables are
/// documented in `crate::projects`.
pub const WORKSPACES_PROJECTS_V1_SQL: &str = r#"
CREATE TABLE workspaces (
    workspace_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    created_at   TEXT NOT NULL
);

CREATE TABLE projects (
    project_id      TEXT PRIMARY KEY,
    workspace_id    TEXT REFERENCES workspaces(workspace_id),
    display_name    TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    repo_path       TEXT NOT NULL UNIQUE CHECK (repo_path LIKE '/%'),
    active          INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at      TEXT NOT NULL,
    deactivated_at  TEXT,
    CHECK (active = 1 OR deactivated_at IS NOT NULL)
);

CREATE TABLE project_references (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id      TEXT NOT NULL REFERENCES projects(project_id),
    reference_path  TEXT NOT NULL CHECK (reference_path LIKE '/%'),
    kind            TEXT NOT NULL CHECK (kind IN ('reference', 'dependency')),
    note            TEXT NOT NULL DEFAULT '',
    added_at        TEXT NOT NULL,
    removed_at      TEXT
);
"#;

/// Register the `workspaces_projects` domain migrations (this domain also
/// serves the project tables — see `crate::projects`).
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(
        DOMAIN_WORKSPACES_PROJECTS,
        1,
        "workspaces and projects v1",
        WORKSPACES_PROJECTS_V1_SQL,
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub workspace_id: WorkspaceId,
    pub display_name: String,
    pub created_at: String,
}

pub struct WorkspaceRegistry<'a> {
    store: &'a Store,
}

impl<'a> WorkspaceRegistry<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn create(&self, display_name: impl Into<String>) -> OfficeResult<WorkspaceRecord> {
        let display_name = display_name.into();
        if display_name.trim().is_empty() {
            return Err(OfficeError::Validation(
                "workspace display_name must not be empty".into(),
            ));
        }
        let record = WorkspaceRecord {
            workspace_id: WorkspaceId::new(),
            display_name,
            created_at: utc_now(),
        };
        self.store.connection().execute(
            "INSERT INTO workspaces(workspace_id, display_name, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                record.workspace_id.as_str(),
                record.display_name,
                record.created_at
            ],
        )?;
        Ok(record)
    }

    pub fn get(&self, workspace_id: &WorkspaceId) -> OfficeResult<Option<WorkspaceRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT workspace_id, display_name, created_at FROM workspaces WHERE workspace_id = ?1",
        )?;
        let row = stmt
            .query_row([workspace_id.as_str()], |row| {
                Ok(WorkspaceRecord {
                    workspace_id: WorkspaceId::from_str(&row.get::<_, String>(0)?).map_err(
                        |e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        },
                    )?,
                    display_name: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn require(&self, workspace_id: &WorkspaceId) -> OfficeResult<WorkspaceRecord> {
        self.get(workspace_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "workspace",
                id: workspace_id.to_string(),
            })
    }

    pub fn list(&self) -> OfficeResult<Vec<WorkspaceRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT workspace_id, display_name, created_at
             FROM workspaces ORDER BY created_at, workspace_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(WorkspaceRecord {
                workspace_id: WorkspaceId::from_str(&row.get::<_, String>(0)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                display_name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        let mut workspaces = Vec::new();
        for row in rows {
            workspaces.push(row?);
        }
        Ok(workspaces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{DOMAIN_FOUNDATION, FOUNDATION_V1_SQL};

    fn store() -> Store {
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(
                DOMAIN_WORKSPACES_PROJECTS,
                1,
                "workspaces and projects v1",
                WORKSPACES_PROJECTS_V1_SQL,
            )
            .freeze()
            .expect("registry");
        Store::open_in_memory(&frozen).expect("store")
    }

    #[test]
    fn workspaces_roundtrip_and_survive_reopen() {
        let dir = tempfile::TempDir::new().expect("dir");
        let db = dir.path().join("office.db");
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(
                DOMAIN_WORKSPACES_PROJECTS,
                1,
                "workspaces and projects v1",
                WORKSPACES_PROJECTS_V1_SQL,
            )
            .freeze()
            .expect("registry");
        let created = {
            let store = Store::open(&db, &frozen).expect("open");
            WorkspaceRegistry::new(&store)
                .create("Haisu's office")
                .expect("create")
        };
        let reopened = Store::open(&db, &frozen).expect("reopen");
        let got = WorkspaceRegistry::new(&reopened)
            .require(&created.workspace_id)
            .expect("workspace survives restart");
        assert_eq!(got, created);
    }

    #[test]
    fn empty_workspace_name_is_rejected() {
        let store = store();
        assert!(WorkspaceRegistry::new(&store).create(" ").is_err());
    }
}
