//! Local workspace composition only. Reuses ProjectRegistry and Git discovery;
//! navigation membership never changes task attribution or grants.
use super::WorkspaceRegistry;
use crate::foundation::ids::{WorkspaceId, WorktreeId};
use crate::foundation::store::Store;
use crate::foundation::{OfficeError, OfficeResult};
use crate::projects::ProjectRegistry;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const V2_SQL: &str = r#"
CREATE TABLE workspace_compositions (
 workspace_id TEXT PRIMARY KEY REFERENCES workspaces(workspace_id),
 source_path TEXT UNIQUE, document TEXT NOT NULL DEFAULT '{}', opened_at TEXT NOT NULL
);
CREATE TABLE workspace_folders (
 workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
 ordinal INTEGER NOT NULL, project_id TEXT REFERENCES projects(project_id),
 folder TEXT NOT NULL, resolved_path TEXT, diagnostic TEXT,
 PRIMARY KEY(workspace_id, ordinal)
);
INSERT INTO workspace_compositions(workspace_id,opened_at)
 SELECT workspace_id,created_at FROM workspaces;
INSERT INTO workspace_folders(workspace_id,ordinal,project_id,folder,resolved_path)
 SELECT workspace_id,rowid,project_id,json_object('path',repo_path,'name',display_name),repo_path
 FROM projects WHERE workspace_id IS NOT NULL;
CREATE TABLE navigation_checkouts (
 worktree_id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(project_id),
 path TEXT NOT NULL UNIQUE, branch TEXT NOT NULL
);
CREATE TABLE navigation_selection (singleton INTEGER PRIMARY KEY CHECK(singleton=1), workspace_id TEXT REFERENCES workspaces(workspace_id));
"#;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub workspace_id: String,
    pub name: String,
    pub source_path: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderRow {
    pub project_id: Option<String>,
    pub name: String,
    pub path: Option<String>,
    pub diagnostic: Option<String>,
}

/// Strip comments and trailing commas without interpreting anything inside strings.
/// VS Code configuration is data; tasks/launch/settings are never executed.
pub fn parse_jsonc(text: &str) -> OfficeResult<Value> {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    let mut string = false;
    while i < b.len() {
        if string {
            if b[i] == b'\\' {
                i += 2;
                continue;
            }
            if b[i] == b'"' {
                string = false;
            }
            i += 1;
            continue;
        }
        if b[i] == b'"' {
            string = true;
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                out[i] = b' ';
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            out[i] = b' ';
            out[i + 1] = b' ';
            i += 2;
            let mut closed = false;
            while i + 1 < b.len() {
                if b[i..].starts_with(b"*/") {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    closed = true;
                    break;
                }
                if b[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            if !closed {
                return Err(OfficeError::Validation("unterminated JSONC comment".into()));
            }
            continue;
        }
        i += 1;
    }
    string = false;
    i = 0;
    while i < out.len() {
        if string {
            if out[i] == b'\\' {
                i += 2;
                continue;
            }
            if out[i] == b'"' {
                string = false;
            }
        } else if out[i] == b'"' {
            string = true;
        } else if out[i] == b',' {
            let mut j = i + 1;
            while j < out.len() && out[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < out.len() && matches!(out[j], b'}' | b']') {
                out[i] = b' ';
            }
        }
        i += 1;
    }
    serde_json::from_slice(&out)
        .map_err(|e| OfficeError::Validation(format!("invalid workspace JSONC: {e}")))
}

pub struct Navigation<'a> {
    pub store: &'a Store,
}
impl<'a> Navigation<'a> {
    pub fn list(&self) -> OfficeResult<Vec<WorkspaceRow>> {
        let mut st=self.store.connection().prepare("SELECT w.workspace_id,w.display_name,c.source_path FROM workspaces w LEFT JOIN workspace_compositions c USING(workspace_id) ORDER BY c.opened_at DESC,w.created_at DESC")?;
        Ok(st
            .query_map([], |r| {
                Ok(WorkspaceRow {
                    workspace_id: r.get(0)?,
                    name: r.get(1)?,
                    source_path: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }
    pub fn selected(&self) -> OfficeResult<Option<String>> {
        Ok(self
            .store
            .connection()
            .query_row(
                "SELECT workspace_id FROM navigation_selection WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }
    pub fn select(&self, id: &str) -> OfficeResult<()> {
        WorkspaceRegistry::new(self.store).require(&WorkspaceId::from_str(id)?)?;
        self.store.connection().execute("INSERT INTO navigation_selection VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET workspace_id=excluded.workspace_id",[id])?;
        self.store.connection().execute(
            "UPDATE workspace_compositions SET opened_at=?2 WHERE workspace_id=?1",
            rusqlite::params![id, crate::foundation::ids::utc_now()],
        )?;
        Ok(())
    }
    pub fn folders(&self, id: &str) -> OfficeResult<Vec<FolderRow>> {
        let mut st=self.store.connection().prepare("SELECT project_id,folder,resolved_path,diagnostic FROM workspace_folders WHERE workspace_id=?1 ORDER BY ordinal")?;
        Ok(st
            .query_map([id], |r| {
                let folder: String = r.get(1)?;
                let v: Value = serde_json::from_str(&folder).unwrap_or_default();
                Ok(FolderRow {
                    project_id: r.get(0)?,
                    name: v
                        .get("name")
                        .or_else(|| v.get("path"))
                        .or_else(|| v.get("uri"))
                        .and_then(Value::as_str)
                        .unwrap_or("unavailable folder")
                        .to_string(),
                    path: r.get(2)?,
                    diagnostic: r.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }
    pub fn create(&self, name: &str) -> OfficeResult<String> {
        let w = WorkspaceRegistry::new(self.store).create(name)?;
        self.store.connection().execute(
            "INSERT INTO workspace_compositions(workspace_id,opened_at) VALUES(?1,?2)",
            rusqlite::params![w.workspace_id.as_str(), crate::foundation::ids::utc_now()],
        )?;
        self.select(w.workspace_id.as_str())?;
        Ok(w.workspace_id.to_string())
    }
    pub fn open(&self, path: &Path) -> OfficeResult<String> {
        let path = std::fs::canonicalize(path)?;
        if path.is_dir() {
            let id = self.create(
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Workspace"),
            )?;
            self.add(&id, json!({"path":path}), Path::new("/"))?;
            return Ok(id);
        }
        if std::fs::metadata(&path)?.len() > 1024 * 1024 {
            return Err(OfficeError::Validation(
                "workspace file exceeds 1 MiB".into(),
            ));
        }
        let doc = parse_jsonc(&std::fs::read_to_string(&path)?)?;
        let folders = doc
            .get("folders")
            .and_then(Value::as_array)
            .ok_or_else(|| OfficeError::Validation("workspace needs a folders array".into()))?;
        if folders.len() > 128 {
            return Err(OfficeError::Validation(
                "workspace exceeds 128 folders".into(),
            ));
        }
        // An already opened file restores the composition, including unsaved edits.
        let old: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT workspace_id FROM workspace_compositions WHERE source_path=?1",
                [path.to_string_lossy().as_ref()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = old {
            self.select(&id)?;
            return Ok(id);
        }
        let id = self.create(
            path.file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("Workspace"),
        )?;
        self.store.connection().execute(
            "UPDATE workspace_compositions SET source_path=?2,document=?3 WHERE workspace_id=?1",
            rusqlite::params![id, path.to_string_lossy(), doc.to_string()],
        )?;
        for f in folders {
            self.add(&id, f.clone(), path.parent().unwrap_or(Path::new("/")))?;
        }
        Ok(id)
    }
    pub fn add(&self, id: &str, folder: Value, base: &Path) -> OfficeResult<()> {
        WorkspaceRegistry::new(self.store).require(&WorkspaceId::from_str(id)?)?;
        let count: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM workspace_folders WHERE workspace_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if count >= 128 {
            return Err(OfficeError::Validation(
                "workspace exceeds 128 folders".into(),
            ));
        }
        let (path, diagnostic) = match folder.get("path").and_then(Value::as_str) {
            Some(p) => {
                let p = base.join(p);
                match std::fs::canonicalize(&p) {
                    Ok(p) if p.is_dir() => (Some(p), None),
                    _ => (Some(p), Some("unavailable: missing directory".to_string())),
                }
            }
            None => (
                None,
                Some("unsupported folder URI: only local path is supported".to_string()),
            ),
        };
        let mut diagnostic = diagnostic;
        let mut project = None;
        if let Some(p) = path.as_ref().filter(|_| diagnostic.is_none()) {
            let service = crate::git::worktrees::WorktreeService::new(
                self.store,
                crate::git::worktrees::ProtectedRefs::new(vec![]),
            );
            let discovered = service.discover(p);
            let repo = match &discovered {
                Ok(ws) => ws.first().map(|w| w.path.clone()).unwrap_or(p.clone()),
                Err(_) => {
                    diagnostic = Some("non-Git directory: shell only".into());
                    p.clone()
                }
            };
            let registry = ProjectRegistry::new(self.store);
            let existing = registry.list(false)?.into_iter().find(|r| {
                std::fs::canonicalize(&r.repo_path).unwrap_or(r.repo_path.clone()) == repo
            });
            let record = match existing {
                Some(r) => r,
                None => registry.register(
                    None,
                    repo.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Project"),
                    repo.clone(),
                )?,
            };
            project = Some(record.project_id.to_string());
            if let Ok(ws) = discovered {
                for w in ws.into_iter().filter(|w| !w.bare) {
                    let recorded:Option<String>=self.store.connection().query_row("SELECT worktree_id FROM task_worktrees WHERE worktree_path=?1 AND released_at IS NULL",[w.path.to_string_lossy().as_ref()],|r|r.get(0)).optional()?;
                    let wid = recorded.unwrap_or_else(|| WorktreeId::new().to_string());
                    self.store.connection().execute("INSERT INTO navigation_checkouts(worktree_id,project_id,path,branch) VALUES(?1,?2,?3,?4) ON CONFLICT(path) DO UPDATE SET branch=excluded.branch",rusqlite::params![wid,record.project_id.as_str(),w.path.to_string_lossy(),w.branch.unwrap_or_else(||"(detached)".into())])?;
                }
            }
        }
        // Avoid duplicate composition references without changing the Project itself.
        if let Some(pid) = &project {
            let exists:bool=self.store.connection().query_row("SELECT EXISTS(SELECT 1 FROM workspace_folders WHERE workspace_id=?1 AND project_id=?2)",rusqlite::params![id,pid],|r|r.get(0))?;
            if exists {
                return Ok(());
            }
        }
        self.store.connection().execute("INSERT INTO workspace_folders(workspace_id,ordinal,project_id,folder,resolved_path,diagnostic) SELECT ?1,COALESCE(MAX(ordinal)+1,0),?2,?3,?4,?5 FROM workspace_folders WHERE workspace_id=?1",rusqlite::params![id,project,folder.to_string(),path.map(|p|p.to_string_lossy().to_string()),diagnostic])?;
        Ok(())
    }
    pub fn remove(&self, id: &str, project: &str) -> OfficeResult<()> {
        self.store.connection().execute(
            "DELETE FROM workspace_folders WHERE workspace_id=?1 AND project_id=?2",
            rusqlite::params![id, project],
        )?;
        Ok(())
    }
    pub fn save_as(&self, id: &str, path: &Path) -> OfficeResult<()> {
        if !path.is_absolute() {
            return Err(OfficeError::Validation(
                "Save As needs an absolute path".into(),
            ));
        }
        let (source, document): (Option<String>, String) = self.store.connection().query_row(
            "SELECT source_path,document FROM workspace_compositions WHERE workspace_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut doc: Value =
            serde_json::from_str(&document).map_err(|e| OfficeError::Validation(e.to_string()))?;
        let base = source.as_deref().and_then(|p| Path::new(p).parent());
        let mut st = self.store.connection().prepare(
            "SELECT folder FROM workspace_folders WHERE workspace_id=?1 ORDER BY ordinal",
        )?;
        let mut folders = Vec::new();
        for row in st.query_map([id], |r| r.get::<_, String>(0))? {
            let mut f: Value =
                serde_json::from_str(&row?).map_err(|e| OfficeError::Validation(e.to_string()))?;
            if let Some(p) = f.get("path").and_then(Value::as_str) {
                let p = PathBuf::from(p);
                if !p.is_absolute() {
                    if let Some(base) = base {
                        f["path"] = json!(base.join(p));
                    }
                }
            }
            folders.push(f);
        }
        doc["folders"] = json!(folders);
        let mut tmp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new("/")))?;
        use std::io::Write;
        tmp.write_all(serde_json::to_string_pretty(&doc).unwrap().as_bytes())?;
        tmp.persist(path).map_err(|e| OfficeError::Io(e.error))?;
        Ok(())
    }
    pub fn checkout(&self, id: &str) -> OfficeResult<Option<(PathBuf, String)>> {
        Ok(self
            .store
            .connection()
            .query_row(
                "SELECT path,branch FROM navigation_checkouts WHERE worktree_id=?1",
                [id],
                |r| Ok((PathBuf::from(r.get::<_, String>(0)?), r.get(1)?)),
            )
            .optional()?)
    }
}
