//! Projects: real directories the office works in, plus explicit
//! reference/dependency edges (V02, #11).
//!
//! Domain semantics:
//! - A project is any directory the user works in. Directories can be opened
//!   directly without registration; registration only adds office context
//!   (selection, worktrees, evidence) on top.
//! - Projects may live in different, unrelated parent directories; sibling
//!   `reference` edges record that one project references another path —
//!   they are office bookkeeping, never software dependencies.
//! - Deactivating a project or removing a reference never deletes history:
//!   rows keep their ids and gain a deactivation timestamp.

use std::path::{Path, PathBuf};
use std::str::FromStr as _;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{ProjectId, WorkspaceId, utc_now};
use crate::foundation::store::Store;

/// A project registered in the office.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub project_id: ProjectId,
    pub workspace_id: Option<WorkspaceId>,
    pub display_name: String,
    pub repo_path: PathBuf,
    pub active: bool,
    pub created_at: String,
    /// Set when the project was deactivated; history is never deleted.
    pub deactivated_at: Option<String>,
}

/// One explicit reference/dependency edge from a project to another path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectReference {
    pub project_id: ProjectId,
    pub reference_path: PathBuf,
    /// `reference` (a sibling checkout the office should know about) or
    /// `dependency` (a path this project depends on at build/run time).
    pub kind: ReferenceKind,
    pub note: String,
    pub added_at: String,
    /// Set when the reference was explicitly removed; the edge stays queryable.
    pub removed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Reference,
    Dependency,
}

impl ReferenceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReferenceKind::Reference => "reference",
            ReferenceKind::Dependency => "dependency",
        }
    }
}

/// A plain directory opened without registration: the office reads its path
/// as-is and creates no project record. This is the "目录直接进入" path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlainDirectory {
    pub path: PathBuf,
    pub opened_at: String,
}

pub struct ProjectRegistry<'a> {
    store: &'a Store,
}

impl<'a> ProjectRegistry<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Register a project directory. Projects may sit in unrelated parents;
    /// the path must be absolute so office facts stay unambiguous.
    pub fn register(
        &self,
        workspace_id: Option<WorkspaceId>,
        display_name: impl Into<String>,
        repo_path: impl Into<PathBuf>,
    ) -> OfficeResult<ProjectRecord> {
        let display_name = display_name.into();
        let repo_path = repo_path.into();
        if display_name.trim().is_empty() {
            return Err(OfficeError::Validation(
                "project display_name must not be empty".into(),
            ));
        }
        if !repo_path.is_absolute() {
            return Err(OfficeError::Validation(format!(
                "project path must be absolute: {}",
                repo_path.display()
            )));
        }
        let record = ProjectRecord {
            project_id: ProjectId::new(),
            workspace_id,
            display_name,
            repo_path,
            active: true,
            created_at: utc_now(),
            deactivated_at: None,
        };
        self.store.connection().execute(
            "INSERT INTO projects(project_id, workspace_id, display_name, repo_path, active, created_at, deactivated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                record.project_id.as_str(),
                record.workspace_id.as_ref().map(|w| w.as_str()),
                record.display_name,
                record.repo_path.to_string_lossy().as_ref(),
                if record.active { 1 } else { 0 },
                record.created_at,
                record.deactivated_at,
            ],
        )?;
        Ok(record)
    }

    /// Deactivate a project. The row, its id and everything referencing them
    /// survive; only `active` flips.
    pub fn deactivate(&self, project_id: &ProjectId) -> OfficeResult<()> {
        self.require(project_id)?;
        let n = self.store.connection().execute(
            "UPDATE projects SET active = 0, deactivated_at = ?2 WHERE project_id = ?1",
            rusqlite::params![project_id.as_str(), utc_now()],
        )?;
        debug_assert_eq!(n, 1);
        Ok(())
    }

    /// Re-activate a previously deactivated project.
    pub fn activate(&self, project_id: &ProjectId) -> OfficeResult<()> {
        self.require(project_id)?;
        self.store.connection().execute(
            "UPDATE projects SET active = 1, deactivated_at = NULL WHERE project_id = ?1",
            [project_id.as_str()],
        )?;
        Ok(())
    }

    pub fn get(&self, project_id: &ProjectId) -> OfficeResult<Option<ProjectRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT project_id, workspace_id, display_name, repo_path, active, created_at, deactivated_at
             FROM projects WHERE project_id = ?1",
        )?;
        let row = stmt
            .query_row([project_id.as_str()], map_project)
            .optional()?;
        Ok(row)
    }

    pub fn require(&self, project_id: &ProjectId) -> OfficeResult<ProjectRecord> {
        self.get(project_id)?.ok_or_else(|| OfficeError::NotFound {
            entity: "project",
            id: project_id.to_string(),
        })
    }

    /// All registered projects; `active_only` filters out deactivated ones.
    pub fn list(&self, active_only: bool) -> OfficeResult<Vec<ProjectRecord>> {
        let sql = format!(
            "SELECT project_id, workspace_id, display_name, repo_path, active, created_at, deactivated_at
             FROM projects {} ORDER BY created_at, project_id",
            if active_only { "WHERE active = 1" } else { "" }
        );
        let mut stmt = self.store.connection().prepare(&sql)?;
        let rows = stmt.query_map([], map_project)?;
        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?);
        }
        Ok(projects)
    }

    /// Add an explicit reference/dependency edge. Sibling references are
    /// office bookkeeping — nothing here makes one project depend on another
    /// at the software level.
    pub fn add_reference(
        &self,
        project_id: &ProjectId,
        reference_path: impl Into<PathBuf>,
        kind: ReferenceKind,
        note: impl Into<String>,
    ) -> OfficeResult<ProjectReference> {
        self.require(project_id)?;
        let reference_path = reference_path.into();
        if !reference_path.is_absolute() {
            return Err(OfficeError::Validation(format!(
                "reference path must be absolute: {}",
                reference_path.display()
            )));
        }
        let edge = ProjectReference {
            project_id: project_id.clone(),
            reference_path,
            kind,
            note: note.into(),
            added_at: utc_now(),
            removed_at: None,
        };
        self.store.connection().execute(
            "INSERT INTO project_references(project_id, reference_path, kind, note, added_at, removed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                edge.project_id.as_str(),
                edge.reference_path.to_string_lossy().as_ref(),
                edge.kind.as_str(),
                edge.note,
                edge.added_at,
                edge.removed_at,
            ],
        )?;
        Ok(edge)
    }

    /// Explicitly remove a reference edge. The edge keeps its history with a
    /// removal timestamp; it does not disappear.
    pub fn remove_reference(
        &self,
        project_id: &ProjectId,
        reference_path: &Path,
    ) -> OfficeResult<()> {
        let n = self.store.connection().execute(
            "UPDATE project_references SET removed_at = ?3
             WHERE project_id = ?1 AND reference_path = ?2 AND removed_at IS NULL",
            rusqlite::params![
                project_id.as_str(),
                reference_path.to_string_lossy().as_ref(),
                utc_now()
            ],
        )?;
        if n == 0 {
            return Err(OfficeError::NotFound {
                entity: "project reference",
                id: format!("{project_id} → {}", reference_path.display()),
            });
        }
        Ok(())
    }

    /// Live (not removed) reference edges of a project.
    pub fn references(&self, project_id: &ProjectId) -> OfficeResult<Vec<ProjectReference>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT project_id, reference_path, kind, note, added_at, removed_at
             FROM project_references
             WHERE project_id = ?1 AND removed_at IS NULL
             ORDER BY added_at, reference_path",
        )?;
        let rows = stmt.query_map([project_id.as_str()], map_reference)?;
        let mut edges = Vec::new();
        for row in rows {
            edges.push(row?);
        }
        Ok(edges)
    }

    /// All reference edges including removed ones (history view).
    pub fn reference_history(&self, project_id: &ProjectId) -> OfficeResult<Vec<ProjectReference>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT project_id, reference_path, kind, note, added_at, removed_at
             FROM project_references WHERE project_id = ?1 ORDER BY added_at, reference_path",
        )?;
        let rows = stmt.query_map([project_id.as_str()], map_reference)?;
        let mut edges = Vec::new();
        for row in rows {
            edges.push(row?);
        }
        Ok(edges)
    }

    /// Open a plain directory without registering it: no rows are written;
    /// the office simply returns a handle-shaped record for this session.
    pub fn open_plain_directory(&self, path: impl Into<PathBuf>) -> OfficeResult<PlainDirectory> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(OfficeError::Validation(format!(
                "directory path must be absolute: {}",
                path.display()
            )));
        }
        if !path.is_dir() {
            return Err(OfficeError::Validation(format!(
                "path is not a directory: {}",
                path.display()
            )));
        }
        Ok(PlainDirectory {
            path,
            opened_at: utc_now(),
        })
    }

    /// Export the domain as portable JSON (backup/migration). Ids, statuses
    /// and reference history are preserved verbatim.
    pub fn export_json(&self) -> OfficeResult<serde_json::Value> {
        Ok(json!({
            "projects": self.list(false)?,
            "reference_history": {
                "items": self.store.connection()
                    .prepare(
                        "SELECT project_id, reference_path, kind, note, added_at, removed_at
                         FROM project_references ORDER BY added_at",
                    )?
                    .query_map([], map_reference)?
                    .collect::<Result<Vec<_>, _>>()?,
            },
        }))
    }
}

fn map_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRecord> {
    let active: i64 = row.get(4)?;
    Ok(ProjectRecord {
        project_id: ProjectId::from_str(&row.get::<_, String>(0)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        workspace_id: row
            .get::<_, Option<String>>(1)?
            .as_deref()
            .and_then(|w| WorkspaceId::from_str(w).ok()),
        display_name: row.get(2)?,
        repo_path: PathBuf::from(row.get::<_, String>(3)?),
        active: active != 0,
        created_at: row.get(5)?,
        deactivated_at: row.get(6)?,
    })
}

fn map_reference(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectReference> {
    let kind: String = row.get(2)?;
    let kind = match kind.as_str() {
        "reference" => ReferenceKind::Reference,
        "dependency" => ReferenceKind::Dependency,
        other => panic!("project_references.kind holds an unknown value `{other}`"),
    };
    Ok(ProjectReference {
        project_id: ProjectId::from_str(&row.get::<_, String>(0)?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        reference_path: PathBuf::from(row.get::<_, String>(1)?),
        kind,
        note: row.get(3)?,
        added_at: row.get(4)?,
        removed_at: row.get(5)?,
    })
}

/// The `projects` tables live inside the shared `workspaces_projects` domain
/// migration; the SQL is owned by `crate::workspaces` so the domain has
/// exactly one version-1 migration.
pub use crate::workspaces::WORKSPACES_PROJECTS_V1_SQL;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{
        DOMAIN_FOUNDATION, DOMAIN_WORKSPACES_PROJECTS, FOUNDATION_V1_SQL, MigrationRegistry, Store,
    };
    use crate::workspaces::WorkspaceRegistry;

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

    fn absolute_tmp(name: &str) -> PathBuf {
        let dir = tempfile::TempDir::new().expect("dir");
        let path = dir.path().join(name);
        // Leak the tempdir so the path outlives this helper in assertions.
        std::fs::create_dir_all(&path).expect("mkdir");
        let path = path.canonicalize().expect("canonical");
        std::mem::forget(dir);
        path
    }

    #[test]
    fn projects_in_unrelated_parents_coexist() {
        let store = store();
        let registry = ProjectRegistry::new(&store);
        let a = registry
            .register(None, "viva", "/Users/hzuo/Documents/code/viva")
            .expect("a");
        let b = registry
            .register(None, "vic", "/Users/hzuo/VicTrader")
            .expect("b");
        assert_ne!(a.project_id, b.project_id);
        assert_eq!(registry.list(true).expect("list").len(), 2);
    }

    #[test]
    fn deactivation_preserves_history_and_reactivates() {
        let store = store();
        let registry = ProjectRegistry::new(&store);
        let project = registry
            .register(None, "viva", "/Users/hzuo/Documents/code/viva")
            .expect("register");

        registry
            .deactivate(&project.project_id)
            .expect("deactivate");
        let deactivated = registry.require(&project.project_id).expect("still there");
        assert!(!deactivated.active);
        assert!(
            deactivated.deactivated_at.is_some(),
            "deactivation timestamped"
        );
        assert!(
            registry.list(true).expect("active list").is_empty(),
            "deactivated project leaves the active projection"
        );
        assert_eq!(
            registry.list(false).expect("full list").len(),
            1,
            "history keeps the row"
        );

        registry.activate(&project.project_id).expect("reactivate");
        assert!(registry.require(&project.project_id).expect("p").active);
    }

    #[test]
    fn references_are_explicit_and_removal_keeps_history() {
        let store = store();
        let registry = ProjectRegistry::new(&store);
        let project = registry
            .register(None, "viva", "/Users/hzuo/Documents/code/viva")
            .expect("register");
        let sibling = absolute_tmp("sibling-checkout");

        registry
            .add_reference(
                &project.project_id,
                &sibling,
                ReferenceKind::Reference,
                "sibling checkout, not a software dependency",
            )
            .expect("add");
        assert_eq!(
            registry
                .references(&project.project_id)
                .expect("refs")
                .len(),
            1
        );

        registry
            .remove_reference(&project.project_id, &sibling)
            .expect("remove");
        assert!(
            registry
                .references(&project.project_id)
                .expect("refs")
                .is_empty()
        );
        let history = registry
            .reference_history(&project.project_id)
            .expect("history");
        assert_eq!(history.len(), 1, "removed edge stays in history");
        assert!(history[0].removed_at.is_some());
    }

    #[test]
    fn plain_directory_opens_without_registration() {
        let store = store();
        let registry = ProjectRegistry::new(&store);
        let dir = absolute_tmp("plain-dir");
        let opened = registry.open_plain_directory(&dir).expect("open");
        assert_eq!(opened.path, dir);
        assert_eq!(
            store.row_count("projects").expect("count"),
            0,
            "opening a plain directory must not register anything"
        );
        assert!(registry.open_plain_directory("relative/path").is_err());
        assert!(
            registry
                .open_plain_directory("/definitely/missing/dir-42")
                .is_err()
        );
    }

    #[test]
    fn relative_project_path_is_rejected() {
        let store = store();
        let registry = ProjectRegistry::new(&store);
        assert!(registry.register(None, "viva", "relative/path").is_err());
    }

    #[test]
    fn export_json_roundtrips_through_serde() {
        let store = store();
        let registry = ProjectRegistry::new(&store);
        let ws = WorkspaceRegistry::new(&store).create("office").expect("ws");
        let project = registry
            .register(
                Some(ws.workspace_id.clone()),
                "viva",
                "/Users/hzuo/Documents/code/viva",
            )
            .expect("register");
        registry
            .add_reference(
                &project.project_id,
                "/tmp/sibling",
                ReferenceKind::Dependency,
                "d",
            )
            .expect("ref");
        let exported = registry.export_json().expect("export");
        let text = serde_json::to_string(&exported).expect("serialize");
        let back: serde_json::Value = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(
            back["projects"][0]["project_id"],
            json!(project.project_id.as_str()),
            "export preserves ids verbatim"
        );
        assert_eq!(
            back["reference_history"]["items"]
                .as_array()
                .expect("items")
                .len(),
            1
        );
    }
}
