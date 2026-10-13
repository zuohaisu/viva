//! Workspace composition for navigation (#74, M1 S1).
//!
//! A navigation *workspace* is an ordered list of folders, optionally backed
//! by a VS Code `.code-workspace` file. It only decides which Projects — and
//! therefore which repo → worktree nodes — the workbench shows. It reuses
//! `ProjectRegistry` and Git discovery and never copies Tasks, Executions,
//! terminals or grants: a Project that sits in two workspaces is one stable
//! row, and removing it from a composition changes membership only.
//!
//! ## Selection contract (what S3 #75 and S4 #76 build on)
//!
//! * [`Navigation::selected`] is the current workspace id, or `None` for the
//!   unfiltered "All projects" view; [`Navigation::selection`] resolves it to
//!   a [`WorkspaceSelection`] (ordered folders, project ids, diagnostics).
//! * `WorkbenchModel.workspace_id` / `.workspaces` / `.projects` /
//!   `.worktrees` carry the same selection to clients. `projects` and
//!   `worktrees` are already filtered to the selection and ordered by
//!   composition order; every `WorktreeRow` has a non-empty `project_id` that
//!   is a member of `projects`, and `worktree_id` is the stable key for
//!   per-worktree state (discovered checkouts keep their id per path).
//! * Switching never touches `task_worktrees`, terminals or grants, so a
//!   scene keyed by `worktree_id` survives A → B → A unchanged.
//!
//! ## `.code-workspace` compatibility
//!
//! Files are JSONC (comments and trailing commas). Only `folders` is read:
//! `path` (absolute, or relative to the file's directory) and `name`.
//! `settings`, `tasks`, `launch`, `extensions` … are data we keep, never
//! interpret or execute. Opening never writes. An explicit save replaces the
//! text of the `folders` value only, so comments and every other field stay
//! byte-identical; unchanged compositions are not rewritten at all.

use super::WorkspaceRegistry;
use crate::foundation::ids::{WorkspaceId, WorktreeId, utc_now};
use crate::foundation::store::Store;
use crate::foundation::{OfficeError, OfficeResult};
use crate::git::worktrees::{DiscoveredWorktree, ProtectedRefs, WorktreeService};
use crate::projects::{ProjectRecord, ProjectRegistry};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
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

/// Shown for a folder that exists but is not inside a Git repository: the
/// workbench offers a plain shell there and no worktree tree.
pub const DIAG_NOT_GIT: &str = "non-Git directory: shell only";
const DIAG_MISSING: &str = "unavailable: missing directory";
const DIAG_NOT_DIR: &str = "unavailable: not a directory";
const DIAG_URI: &str = "unsupported folder URI: only local paths are supported";
const WORKSPACE_EXT: &str = ".code-workspace";
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_FOLDERS: usize = 128;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub workspace_id: String,
    pub name: String,
    pub source_path: Option<String>,
    /// Last time this workspace was opened or selected (RFC 3339); the
    /// "recent" ordering. The switcher itself uses creation order.
    #[serde(default)]
    pub opened_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRow {
    pub project_id: Option<String>,
    /// Display name: the entry's `name`, else the directory's name (what
    /// VS Code shows), never a raw relative path.
    pub name: String,
    pub path: Option<String>,
    pub diagnostic: Option<String>,
}

/// The resolved current workspace; see the module docs for the contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSelection {
    pub workspace_id: String,
    pub name: String,
    pub source_path: Option<String>,
    pub folders: Vec<FolderRow>,
}

impl WorkspaceSelection {
    /// Member project ids in composition order, each once.
    pub fn project_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        for id in self.folders.iter().filter_map(|f| f.project_id.as_deref()) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    pub fn contains_project(&self, project_id: &str) -> bool {
        self.folders
            .iter()
            .any(|f| f.project_id.as_deref() == Some(project_id))
    }

    /// The name the composition gives a project (its first folder entry).
    pub fn project_name(&self, project_id: &str) -> Option<&str> {
        self.folders
            .iter()
            .find(|f| f.project_id.as_deref() == Some(project_id))
            .map(|f| f.name.as_str())
    }

    /// `name: reason` lines for every folder that is unavailable or limited.
    pub fn diagnostics(&self) -> Vec<String> {
        self.folders
            .iter()
            .filter_map(|f| f.diagnostic.as_ref().map(|d| format!("{}: {d}", f.name)))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    Added,
    /// The repo (or exact path) is already part of the composition.
    AlreadyPresent,
}

/// What a folder entry resolved to on disk and in the registries.
struct EntryState {
    project_id: Option<String>,
    resolved: Option<PathBuf>,
    diagnostic: Option<String>,
}

struct StoredFolder {
    ordinal: i64,
    project_id: Option<String>,
    folder: Value,
    resolved_path: Option<String>,
    diagnostic: Option<String>,
}

// ---------------------------------------------------------------------------
// JSONC
// ---------------------------------------------------------------------------

/// Strip comments and trailing commas without interpreting anything inside strings.
/// VS Code configuration is data; tasks/launch/settings are never executed.
pub fn parse_jsonc(text: &str) -> OfficeResult<Value> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
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

/// Index after whitespace and `//` / `/* */` comments.
fn skip_trivia(b: &[u8], mut i: usize) -> usize {
    loop {
        match b.get(i) {
            Some(c) if c.is_ascii_whitespace() => i += 1,
            Some(b'/') if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            Some(b'/') if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < b.len() && !b[i..].starts_with(b"*/") {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            _ => return i,
        }
    }
}

/// Index after the string whose opening quote is at `start`.
fn string_end(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Index after the JSON value starting at `start`.
fn value_end(b: &[u8], start: usize) -> Option<usize> {
    match *b.get(start)? {
        b'"' => string_end(b, start),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = start;
            while i < b.len() {
                match b[i] {
                    b'"' => {
                        i = string_end(b, i)?;
                        continue;
                    }
                    b'/' if matches!(b.get(i + 1), Some(b'/' | b'*')) => {
                        i = skip_trivia(b, i);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let mut i = start;
            while i < b.len()
                && !matches!(b[i], b',' | b'}' | b']')
                && !b[i].is_ascii_whitespace()
                && !(b[i] == b'/' && matches!(b.get(i + 1), Some(b'/' | b'*')))
            {
                i += 1;
            }
            Some(i)
        }
    }
}

/// Byte range of the value of the top-level `"folders"` key (the last one,
/// as a JSON parser would pick).
fn folders_span(text: &str) -> Option<(usize, usize)> {
    let b = text.as_bytes();
    let mut i = skip_trivia(b, 0);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    let mut found = None;
    loop {
        i = skip_trivia(b, i);
        match b.get(i)? {
            b',' => i += 1,
            b'}' => return found,
            b'"' => {
                let after = string_end(b, i)?;
                let key = &text[i + 1..after - 1];
                i = skip_trivia(b, after);
                if b.get(i) != Some(&b':') {
                    return None;
                }
                i = skip_trivia(b, i + 1);
                let end = value_end(b, i)?;
                if key == "folders" {
                    found = Some((i, end));
                }
                i = end;
            }
            _ => return None,
        }
    }
}

fn render_folders(entries: &[Value], indent: &str, unit: &str) -> String {
    if entries.is_empty() {
        return "[]".into();
    }
    let mut out = String::from("[\n");
    for (n, entry) in entries.iter().enumerate() {
        out.push_str(indent);
        out.push_str(unit);
        out.push_str(&entry.to_string());
        out.push_str(if n + 1 < entries.len() { ",\n" } else { "\n" });
    }
    out.push_str(indent);
    out.push(']');
    out
}

/// The file text for `entries`. With a template (the file this workspace was
/// opened from) only the `folders` value is replaced; everything else —
/// comments, formatting, `settings`, `tasks` — is carried over verbatim, and
/// a composition that did not change leaves the text untouched. Without a
/// template the stored document gets a fresh `folders` array.
fn render_workspace(template: Option<&str>, fallback: &Value, entries: &[Value]) -> String {
    if let Some(full) = template {
        let (bom, text) = match full.strip_prefix('\u{feff}') {
            Some(rest) => ("\u{feff}", rest),
            None => ("", full),
        };
        if let Some((start, end)) = folders_span(text) {
            let current = parse_jsonc(&text[start..end]).ok();
            if current
                .as_ref()
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                == Some(entries)
            {
                return full.to_string();
            }
            let line_start = text[..start].rfind('\n').map_or(0, |n| n + 1);
            let indent: String = text[line_start..]
                .chars()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .collect();
            let unit = if indent.contains('\t') { "\t" } else { "    " };
            return format!(
                "{bom}{}{}{}",
                &text[..start],
                render_folders(entries, &indent, unit),
                &text[end..]
            );
        }
    }
    let mut doc = match template.map(parse_jsonc) {
        Some(Ok(doc)) if doc.is_object() => doc,
        _ if fallback.is_object() => fallback.clone(),
        _ => json!({}),
    };
    doc["folders"] = Value::Array(entries.to_vec());
    let mut text = serde_json::to_string_pretty(&doc).expect("JSON value serializes");
    text.push('\n');
    text
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Resolve `.` and `..` lexically (no filesystem access).
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `target` expressed relative to the directory `from` (both absolute and
/// normalized).
fn relative_path(from: &Path, target: &Path) -> PathBuf {
    let a: Vec<_> = from.components().collect();
    let b: Vec<_> = target.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..a.len() {
        out.push("..");
    }
    for component in &b[common..] {
        out.push(component.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// What the user typed → an absolute path: `~` and `~/…` expand to `$HOME`,
/// relative input is taken from `cwd`.
pub fn absolutize(input: &str, cwd: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let expanded = match (input, home) {
        ("~", Some(home)) => Some(home),
        (_, Some(home)) => input.strip_prefix("~/").map(|rest| home.join(rest)),
        _ => None,
    };
    normalize_lexical(&expanded.unwrap_or_else(|| cwd.join(input)))
}

fn basename(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

fn display_name(folder: &Value, resolved: Option<&str>) -> String {
    if let Some(name) = folder
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        return name.to_string();
    }
    let path = folder.get("path").and_then(Value::as_str);
    resolved
        .and_then(|p| basename(Path::new(p)))
        .or_else(|| path.and_then(|p| basename(Path::new(p))))
        .or_else(|| {
            folder
                .get("uri")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| path.map(str::to_string))
        .unwrap_or_else(|| "unavailable folder".into())
}

/// Resolve one folder entry against `base` (the workspace file's directory).
fn resolve_folder(folder: &Value, base: &Path) -> (Option<PathBuf>, Option<String>) {
    match folder.get("path").and_then(Value::as_str) {
        Some(p) if !p.trim().is_empty() => {
            let joined = normalize_lexical(&base.join(p));
            match std::fs::canonicalize(&joined) {
                Ok(c) if c.is_dir() => (Some(c), None),
                Ok(c) => (Some(c), Some(DIAG_NOT_DIR.to_string())),
                Err(_) => (Some(joined), Some(DIAG_MISSING.to_string())),
            }
        }
        Some(_) => (None, Some("folder entry has an empty path".to_string())),
        None if folder.get("uri").is_some() => (None, Some(DIAG_URI.to_string())),
        None => (None, Some("folder entry has no path".to_string())),
    }
}

fn read_limited(path: &Path) -> OfficeResult<String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| OfficeError::Validation(format!("cannot read {}: {e}", path.display())))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(OfficeError::Validation(format!(
            "{} exceeds 1 MiB",
            path.display()
        )));
    }
    std::fs::read_to_string(path)
        .map_err(|e| OfficeError::Validation(format!("cannot read {}: {e}", path.display())))
}

/// Write via a sibling temp file and rename, keeping an existing file's
/// permissions (a new file gets the user's umask default, not 0600).
fn write_atomic(target: &Path, text: &str) -> OfficeResult<()> {
    use std::io::Write;
    let name = basename(target)
        .ok_or_else(|| OfficeError::Validation("Save As needs a file name".into()))?;
    let tmp = target.with_file_name(format!(".{name}.viva-tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        if let Ok(meta) = std::fs::metadata(target) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

/// A relative `path` keeps pointing at the same directory after the file
/// moves to `new_dir`; entries that are absolute (or when nothing moved) are
/// returned untouched.
fn rebase_entry(
    folder: &Value,
    resolved: Option<&str>,
    source_dir: Option<&Path>,
    new_dir: &Path,
) -> Value {
    let mut entry = folder.clone();
    let Some(path) = folder.get("path").and_then(Value::as_str) else {
        return entry;
    };
    if Path::new(path).is_absolute() || source_dir == Some(new_dir) {
        return entry;
    }
    if let Some(resolved) = resolved {
        entry["path"] = json!(relative_path(new_dir, Path::new(resolved)).to_string_lossy());
    }
    entry
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

pub struct Navigation<'a> {
    pub store: &'a Store,
}

impl<'a> Navigation<'a> {
    /// Every workspace in creation order — stable across switches, which is
    /// what the top switcher cycles through.
    pub fn list(&self) -> OfficeResult<Vec<WorkspaceRow>> {
        self.workspace_rows("ORDER BY w.rowid", None)
    }

    /// Most recently opened first; the "open recent" list.
    pub fn recent(&self, limit: usize) -> OfficeResult<Vec<WorkspaceRow>> {
        self.workspace_rows(
            "ORDER BY COALESCE(c.opened_at, w.created_at) DESC, w.rowid DESC",
            Some(limit),
        )
    }

    fn workspace_rows(&self, order: &str, limit: Option<usize>) -> OfficeResult<Vec<WorkspaceRow>> {
        let sql = format!(
            "SELECT w.workspace_id, w.display_name, c.source_path,
                    COALESCE(c.opened_at, w.created_at)
             FROM workspaces w LEFT JOIN workspace_compositions c USING(workspace_id)
             {order} LIMIT {}",
            limit.map_or(-1, |n| n as i64)
        );
        let mut stmt = self.store.connection().prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(WorkspaceRow {
                workspace_id: r.get(0)?,
                name: r.get(1)?,
                source_path: r.get(2)?,
                opened_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The current workspace id; `None` is the unfiltered "All projects" view.
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

    /// The current workspace resolved to its folders.
    pub fn selection(&self) -> OfficeResult<Option<WorkspaceSelection>> {
        let Some(id) = self.selected()? else {
            return Ok(None);
        };
        let row = self
            .list()?
            .into_iter()
            .find(|w| w.workspace_id == id)
            .ok_or_else(|| OfficeError::NotFound {
                entity: "workspace",
                id: id.clone(),
            })?;
        Ok(Some(WorkspaceSelection {
            folders: self.folders(&id)?,
            workspace_id: id,
            name: row.name,
            source_path: row.source_path,
        }))
    }

    pub fn select(&self, id: &str) -> OfficeResult<()> {
        self.set_selection(id)?;
        // A switch re-reads the repos so checkouts created or removed outside
        // Viva show up. Best effort: failures are visible as diagnostics.
        let _ = self.rescan(id);
        Ok(())
    }

    fn set_selection(&self, id: &str) -> OfficeResult<()> {
        WorkspaceRegistry::new(self.store).require(&WorkspaceId::from_str(id)?)?;
        self.store.connection().execute(
            "INSERT INTO navigation_selection VALUES(1,?1)
             ON CONFLICT(singleton) DO UPDATE SET workspace_id=excluded.workspace_id",
            [id],
        )?;
        self.store.connection().execute(
            "UPDATE workspace_compositions SET opened_at=?2 WHERE workspace_id=?1",
            rusqlite::params![id, utc_now()],
        )?;
        Ok(())
    }

    /// Back to the unfiltered "All projects" view. Nothing is closed or deleted.
    pub fn select_all(&self) -> OfficeResult<()> {
        self.store.connection().execute(
            "INSERT INTO navigation_selection VALUES(1,NULL)
             ON CONFLICT(singleton) DO UPDATE SET workspace_id=NULL",
            [],
        )?;
        Ok(())
    }

    fn name_of(&self, id: &str) -> OfficeResult<String> {
        Ok(WorkspaceRegistry::new(self.store)
            .require(&WorkspaceId::from_str(id)?)?
            .display_name)
    }

    fn stored_folders(&self, id: &str) -> OfficeResult<Vec<StoredFolder>> {
        let mut st = self.store.connection().prepare(
            "SELECT ordinal,project_id,folder,resolved_path,diagnostic
             FROM workspace_folders WHERE workspace_id=?1 ORDER BY ordinal",
        )?;
        let rows = st.query_map([id], |r| {
            let folder: String = r.get(2)?;
            Ok(StoredFolder {
                ordinal: r.get(0)?,
                project_id: r.get(1)?,
                folder: serde_json::from_str(&folder).unwrap_or_default(),
                resolved_path: r.get(3)?,
                diagnostic: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn folders(&self, id: &str) -> OfficeResult<Vec<FolderRow>> {
        Ok(self
            .stored_folders(id)?
            .into_iter()
            .map(|f| FolderRow {
                name: display_name(&f.folder, f.resolved_path.as_deref()),
                project_id: f.project_id,
                path: f.resolved_path,
                diagnostic: f.diagnostic,
            })
            .collect())
    }

    pub fn create(&self, name: &str) -> OfficeResult<String> {
        let w = WorkspaceRegistry::new(self.store).create(name)?;
        self.store.connection().execute(
            "INSERT INTO workspace_compositions(workspace_id,opened_at) VALUES(?1,?2)",
            rusqlite::params![w.workspace_id.as_str(), utc_now()],
        )?;
        self.set_selection(w.workspace_id.as_str())?;
        Ok(w.workspace_id.to_string())
    }

    /// Open a directory (a single repo, no ceremony) or a `.code-workspace`
    /// file. The same directory or file always maps to the same workspace.
    pub fn open(&self, path: &Path) -> OfficeResult<String> {
        let path = std::fs::canonicalize(path).map_err(|e| {
            OfficeError::Validation(format!(
                "workspace path not found: {} ({e})",
                path.display()
            ))
        })?;
        let id = if path.is_dir() {
            self.open_directory(&path)?
        } else {
            self.open_file(&path)?
        };
        Ok(id)
    }

    fn open_directory(&self, dir: &Path) -> OfficeResult<String> {
        let name = basename(dir).unwrap_or_else(|| "Workspace".into());
        let existing: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT w.workspace_id FROM workspaces w
                 JOIN workspace_compositions c USING(workspace_id)
                 WHERE c.source_path IS NULL AND w.display_name=?1
                   AND (SELECT COUNT(*) FROM workspace_folders f
                        WHERE f.workspace_id=w.workspace_id)=1
                   AND EXISTS (SELECT 1 FROM workspace_folders f
                        WHERE f.workspace_id=w.workspace_id AND f.resolved_path=?2)
                 ORDER BY c.opened_at DESC LIMIT 1",
                rusqlite::params![name, dir.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            self.select(&id)?;
            return Ok(id);
        }
        let id = self.create(&name)?;
        self.add(&id, json!({"path": dir}), Path::new("/"))?;
        Ok(id)
    }

    fn open_file(&self, file: &Path) -> OfficeResult<String> {
        let doc = parse_jsonc(&read_limited(file)?)?;
        let folders = doc
            .get("folders")
            .and_then(Value::as_array)
            .ok_or_else(|| OfficeError::Validation("workspace needs a folders array".into()))?;
        if folders.len() > MAX_FOLDERS {
            return Err(OfficeError::Validation(format!(
                "workspace exceeds {MAX_FOLDERS} folders"
            )));
        }
        let base = file.parent().unwrap_or(Path::new("/"));
        let known: Option<(String, String)> = self
            .store
            .connection()
            .query_row(
                "SELECT workspace_id, document FROM workspace_compositions WHERE source_path=?1",
                [file.to_string_lossy().as_ref()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((id, stored)) = known {
            // Unchanged file: restore the composition as it was, including
            // edits not yet saved. A file edited elsewhere is the source of
            // truth again, so reload its folders (same workspace id).
            if serde_json::from_str::<Value>(&stored).ok().as_ref() != Some(&doc) {
                self.reload_folders(&id, &doc, folders, base)?;
            }
            self.select(&id)?;
            return Ok(id);
        }
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.strip_suffix(WORKSPACE_EXT).unwrap_or(n))
            .filter(|n| !n.is_empty())
            .unwrap_or("Workspace");
        let id = self.create(name)?;
        self.store.connection().execute(
            "UPDATE workspace_compositions SET source_path=?2,document=?3 WHERE workspace_id=?1",
            rusqlite::params![id, file.to_string_lossy(), doc.to_string()],
        )?;
        for folder in folders {
            self.add_entry(&id, folder.clone(), base, false)?;
        }
        Ok(id)
    }

    fn reload_folders(
        &self,
        id: &str,
        doc: &Value,
        folders: &[Value],
        base: &Path,
    ) -> OfficeResult<()> {
        self.store
            .connection()
            .execute("DELETE FROM workspace_folders WHERE workspace_id=?1", [id])?;
        self.store.connection().execute(
            "UPDATE workspace_compositions SET document=?2 WHERE workspace_id=?1",
            rusqlite::params![id, doc.to_string()],
        )?;
        for folder in folders {
            self.add_entry(id, folder.clone(), base, false)?;
        }
        Ok(())
    }

    /// Add a folder to a composition. Adding a repo that is already part of
    /// it (through any of its checkouts) changes nothing.
    pub fn add(&self, id: &str, folder: Value, base: &Path) -> OfficeResult<AddOutcome> {
        self.add_entry(id, folder, base, true)
    }

    fn add_entry(
        &self,
        id: &str,
        folder: Value,
        base: &Path,
        dedupe: bool,
    ) -> OfficeResult<AddOutcome> {
        self.name_of(id)?;
        let count: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM workspace_folders WHERE workspace_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if count as usize >= MAX_FOLDERS {
            return Err(OfficeError::Validation(format!(
                "workspace exceeds {MAX_FOLDERS} folders"
            )));
        }
        let state = self.resolve_entry(&folder, base)?;
        if dedupe {
            let present: bool = self.store.connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM workspace_folders WHERE workspace_id=?1
                   AND ((?2 IS NOT NULL AND project_id=?2) OR (?3 IS NOT NULL AND resolved_path=?3)))",
                rusqlite::params![
                    id,
                    state.project_id,
                    state.resolved.as_ref().map(|p| p.to_string_lossy().into_owned())
                ],
                |r| r.get(0),
            )?;
            if present {
                return Ok(AddOutcome::AlreadyPresent);
            }
        }
        self.store.connection().execute(
            "INSERT INTO workspace_folders(workspace_id,ordinal,project_id,folder,resolved_path,diagnostic)
             SELECT ?1,COALESCE(MAX(ordinal)+1,0),?2,?3,?4,?5 FROM workspace_folders WHERE workspace_id=?1",
            rusqlite::params![
                id,
                state.project_id,
                folder.to_string(),
                state.resolved.map(|p| p.to_string_lossy().into_owned()),
                state.diagnostic
            ],
        )?;
        Ok(AddOutcome::Added)
    }

    /// Resolve one entry on disk and, when it is a usable directory, attach
    /// its Project and checkouts. Reads only; nothing in the entry (or the
    /// workspace file around it) is ever executed.
    fn resolve_entry(&self, folder: &Value, base: &Path) -> OfficeResult<EntryState> {
        let (resolved, mut diagnostic) = resolve_folder(folder, base);
        let mut project_id = None;
        if let (Some(dir), None) = (&resolved, &diagnostic) {
            let service = WorktreeService::new(self.store, ProtectedRefs::new(vec![]));
            let listed = service.list_registered(dir);
            let repo = match &listed {
                Ok(all) => all.first().map_or_else(|| dir.clone(), |w| w.path.clone()),
                Err(_) => {
                    diagnostic = Some(DIAG_NOT_GIT.to_string());
                    dir.clone()
                }
            };
            let repo = std::fs::canonicalize(&repo).unwrap_or(repo);
            let registry = ProjectRegistry::new(self.store);
            let record = match registry.list(false)?.into_iter().find(|r| {
                std::fs::canonicalize(&r.repo_path).unwrap_or_else(|_| r.repo_path.clone()) == repo
            }) {
                Some(existing) => existing,
                None => registry.register(
                    None,
                    basename(&repo).unwrap_or_else(|| "Project".into()),
                    repo.clone(),
                )?,
            };
            if !record.active && diagnostic.is_none() {
                diagnostic = Some("project is deactivated".to_string());
            }
            if let Ok(all) = &listed {
                self.record_checkouts(&record, all)?;
            }
            project_id = Some(record.project_id.to_string());
        }
        Ok(EntryState {
            project_id,
            resolved,
            diagnostic,
        })
    }

    /// Mirror what git lists for the project: new checkouts are added (a
    /// task-owned one keeps its worktree id), vanished ones are dropped.
    fn record_checkouts(
        &self,
        project: &ProjectRecord,
        listed: &[DiscoveredWorktree],
    ) -> OfficeResult<()> {
        let conn = self.store.connection();
        let mut live = Vec::new();
        for w in listed.iter().filter(|w| !w.bare && w.prunable.is_none()) {
            let path = w.path.to_string_lossy().into_owned();
            let recorded: Option<String> = conn
                .query_row(
                    "SELECT worktree_id FROM task_worktrees
                     WHERE worktree_path=?1 AND released_at IS NULL",
                    [path.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            conn.execute(
                "INSERT INTO navigation_checkouts(worktree_id,project_id,path,branch)
                 VALUES(?1,?2,?3,?4)
                 ON CONFLICT(path) DO UPDATE SET branch=excluded.branch",
                rusqlite::params![
                    recorded.unwrap_or_else(|| WorktreeId::new().to_string()),
                    project.project_id.as_str(),
                    path,
                    w.branch.clone().unwrap_or_else(|| "(detached)".into())
                ],
            )?;
            live.push(path);
        }
        let mut st = conn.prepare("SELECT path FROM navigation_checkouts WHERE project_id=?1")?;
        let known: Vec<String> = st
            .query_map([project.project_id.as_str()], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for path in known.into_iter().filter(|p| !live.contains(p)) {
            conn.execute("DELETE FROM navigation_checkouts WHERE path=?1", [path])?;
        }
        Ok(())
    }

    /// Re-check every folder of a workspace against the disk and git:
    /// repaired paths become usable, vanished ones become diagnostics, and
    /// the checkout list follows what git reports now. Returns the
    /// diagnostics that remain.
    pub fn rescan(&self, id: &str) -> OfficeResult<Vec<String>> {
        let base = self.base_dir(id)?;
        let tx = self.store.transaction()?;
        for stored in self.stored_folders(id)? {
            let state = self.resolve_entry(&stored.folder, &base)?;
            tx.execute(
                "UPDATE workspace_folders SET project_id=?3,resolved_path=?4,diagnostic=?5
                 WHERE workspace_id=?1 AND ordinal=?2",
                rusqlite::params![
                    id,
                    stored.ordinal,
                    state.project_id,
                    state.resolved.map(|p| p.to_string_lossy().into_owned()),
                    state.diagnostic
                ],
            )?;
        }
        tx.commit()?;
        Ok(self
            .folders(id)?
            .into_iter()
            .filter_map(|f| f.diagnostic.map(|d| format!("{}: {d}", f.name)))
            .collect())
    }

    /// Relative folder entries resolve against the workspace file's directory.
    fn base_dir(&self, id: &str) -> OfficeResult<PathBuf> {
        let source: Option<String> = self.store.connection().query_row(
            "SELECT source_path FROM workspace_compositions WHERE workspace_id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(source
            .as_deref()
            .and_then(|s| Path::new(s).parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("/")))
    }

    /// Remove folders from a composition by project id, resolved path or the
    /// entry's own `path` text. Membership only: files, history, processes
    /// and records are untouched.
    pub fn remove(&self, id: &str, key: &str) -> OfficeResult<usize> {
        let n = self.store.connection().execute(
            "DELETE FROM workspace_folders WHERE workspace_id=?1
               AND (project_id=?2 OR resolved_path=?2 OR json_extract(folder,'$.path')=?2)",
            rusqlite::params![id, key],
        )?;
        if n == 0 {
            return Err(OfficeError::Validation(format!(
                "nothing in this workspace matches `{key}`"
            )));
        }
        Ok(n)
    }

    /// Save the composition as a VS Code workspace file and bind the
    /// workspace to it (the file is what reopening restores). Returns the
    /// path actually written (`.code-workspace` is appended when missing).
    pub fn save_as(&self, id: &str, path: &Path) -> OfficeResult<PathBuf> {
        if !path.is_absolute() {
            return Err(OfficeError::Validation(
                "Save As needs an absolute path".into(),
            ));
        }
        self.name_of(id)?;
        let mut path = normalize_lexical(path);
        let name = basename(&path)
            .ok_or_else(|| OfficeError::Validation("Save As needs a file name".into()))?;
        if !name.ends_with(WORKSPACE_EXT) {
            path.set_file_name(format!("{name}{WORKSPACE_EXT}"));
        }
        let parent = path.parent().unwrap_or(Path::new("/"));
        let dir = std::fs::canonicalize(parent).map_err(|_| {
            OfficeError::Validation(format!(
                "cannot save into {}: the directory does not exist",
                parent.display()
            ))
        })?;
        let target = match std::fs::canonicalize(&path) {
            Ok(existing) => existing,
            Err(_) => dir.join(path.file_name().expect("file name checked above")),
        };
        self.guard_overwrite(&target)?;
        let bound_elsewhere: Option<String> = self
            .store
            .connection()
            .query_row(
                "SELECT w.display_name FROM workspace_compositions c
                 JOIN workspaces w USING(workspace_id)
                 WHERE c.source_path=?1 AND c.workspace_id<>?2",
                rusqlite::params![target.to_string_lossy(), id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = bound_elsewhere {
            return Err(OfficeError::Validation(format!(
                "{} is already open as workspace `{other}`",
                target.display()
            )));
        }

        let (source, document): (Option<String>, String) = self.store.connection().query_row(
            "SELECT source_path,document FROM workspace_compositions WHERE workspace_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let source_dir = source
            .as_deref()
            .and_then(|s| Path::new(s).parent().map(Path::to_path_buf));
        let stored = self.stored_folders(id)?;
        let entries: Vec<Value> = stored
            .iter()
            .map(|f| {
                rebase_entry(
                    &f.folder,
                    f.resolved_path.as_deref(),
                    source_dir.as_deref(),
                    &dir,
                )
            })
            .collect();
        let fallback: Value = serde_json::from_str(&document).unwrap_or_else(|_| json!({}));
        let template = source
            .as_deref()
            .and_then(|s| read_limited(Path::new(s)).ok());
        let text = render_workspace(template.as_deref(), &fallback, &entries);
        write_atomic(&target, &text)?;

        let saved = parse_jsonc(&text).unwrap_or(fallback);
        let tx = self.store.transaction()?;
        tx.execute(
            "UPDATE workspace_compositions SET source_path=?2,document=?3,opened_at=?4
             WHERE workspace_id=?1",
            rusqlite::params![id, target.to_string_lossy(), saved.to_string(), utc_now()],
        )?;
        for (f, entry) in stored.iter().zip(&entries) {
            tx.execute(
                "UPDATE workspace_folders SET folder=?3 WHERE workspace_id=?1 AND ordinal=?2",
                rusqlite::params![id, f.ordinal, entry.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(target)
    }

    /// An explicit save may replace a workspace file (or an empty
    /// placeholder), never an arbitrary file the user mistyped.
    fn guard_overwrite(&self, target: &Path) -> OfficeResult<()> {
        let Ok(meta) = std::fs::metadata(target) else {
            return Ok(());
        };
        let is_workspace = meta.is_file()
            && (meta.len() == 0
                || read_limited(target)
                    .ok()
                    .and_then(|t| parse_jsonc(&t).ok())
                    .is_some_and(|d| d.get("folders").is_some_and(Value::is_array)));
        if is_workspace {
            Ok(())
        } else {
            Err(OfficeError::Validation(format!(
                "refusing to overwrite {}: it is not a workspace file",
                target.display()
            )))
        }
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

    /// The owner-only control surface behind `viva workspace …` and the TUI:
    /// `open | new | select | add | remove | save | refresh | list`. The
    /// reply always carries the workspace list and the current composition
    /// so a client can show what happened, including unavailable folders.
    pub fn apply(
        &self,
        action: &str,
        value: Option<&str>,
        project_id: Option<&str>,
    ) -> OfficeResult<Value> {
        let need = || {
            value
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| OfficeError::Validation(format!("workspace {action} needs a value")))
        };
        let current = || {
            self.selected()?
                .ok_or_else(|| OfficeError::Validation("select or open a workspace first".into()))
        };
        let message = match action {
            "open" => {
                let id = self.open(Path::new(need()?))?;
                let folders = self.folders(&id)?;
                let limited = folders.iter().filter(|f| f.diagnostic.is_some()).count();
                let mut m = format!("opened {}: {} folder(s)", self.name_of(&id)?, folders.len());
                if limited > 0 {
                    m.push_str(&format!(", {limited} unavailable or limited"));
                }
                m
            }
            "new" => format!(
                "created workspace {}",
                self.name_of(&self.create(need()?)?)?
            ),
            "select" => match need()? {
                "all" => {
                    self.select_all()?;
                    "showing all projects".to_string()
                }
                id => {
                    self.select(id)?;
                    format!("workspace: {}", self.name_of(id)?)
                }
            },
            "add" => {
                let id = current()?;
                let path = need()?;
                match self.add(&id, json!({ "path": path }), Path::new("/"))? {
                    AddOutcome::Added => format!("added {path}"),
                    AddOutcome::AlreadyPresent => format!("{path} is already in this workspace"),
                }
            }
            "remove" => {
                let id = current()?;
                let key = project_id
                    .or(value)
                    .filter(|k| !k.is_empty())
                    .ok_or_else(|| {
                        OfficeError::Validation("remove needs a project id or path".into())
                    })?;
                let n = self.remove(&id, key)?;
                format!(
                    "removed {n} folder(s) from the composition; nothing was deleted or stopped"
                )
            }
            "save" => {
                let id = current()?;
                format!("saved {}", self.save_as(&id, Path::new(need()?))?.display())
            }
            "refresh" => {
                let id = current()?;
                self.rescan(&id)?;
                format!("rescanned {}", self.name_of(&id)?)
            }
            "list" => String::new(),
            _ => {
                return Err(OfficeError::Validation(
                    "workspace action: open | new | select | add | remove | save | refresh | list"
                        .into(),
                ));
            }
        };
        let selection = self.selection()?;
        Ok(json!({
            "workspace_id": selection.as_ref().map(|s| &s.workspace_id),
            "workspaces": self.list()?,
            "recent": self.recent(8)?,
            "folders": selection.as_ref().map(|s| &s.folders),
            "diagnostics": selection.as_ref().map(WorkspaceSelection::diagnostics).unwrap_or_default(),
            "message": message,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_accepts_a_byte_order_mark_and_keeps_strings_intact() {
        let v =
            parse_jsonc("\u{feff}{ \"a\": \"//not a comment\", /* c */ \"b\": [1,], }").unwrap();
        assert_eq!(v["a"], "//not a comment");
        assert_eq!(v["b"][0], 1);
    }

    #[test]
    fn folders_span_ignores_lookalikes_and_finds_the_top_level_key() {
        let text = "{\n // \"folders\": [1]\n \"settings\": {\"folders\": [2], \"s\": \"}\"},\n \"folders\": [ {\"path\": \"a\"} /* ] */ ],\n}";
        let (s, e) = folders_span(text).unwrap();
        assert_eq!(&text[s..e], "[ {\"path\": \"a\"} /* ] */ ]");
        assert!(folders_span("{ \"settings\": {} }").is_none());
        assert!(folders_span("[1,2]").is_none());
        assert!(folders_span("{ \"folders\": [ ").is_none());
    }

    #[test]
    fn rendering_replaces_only_the_folder_list() {
        let template = "{\n\t// keep me\n\t\"folders\": [ { \"path\": \"a\" } ],\n\t\"settings\": { \"x\": 1 } // tail\n}\n";
        let same = [json!({"path": "a"})];
        assert_eq!(
            render_workspace(Some(template), &json!({}), &same),
            template
        );
        let changed = [json!({"path": "a"}), json!({"path": "/b", "name": "B"})];
        let out = render_workspace(Some(template), &json!({}), &changed);
        assert!(out.starts_with("{\n\t// keep me\n\t\"folders\": [\n\t\t{\"path\":\"a\"},\n\t\t{\"name\":\"B\",\"path\":\"/b\"}\n\t],\n"));
        assert!(out.ends_with("\"settings\": { \"x\": 1 } // tail\n}\n"));
        assert_eq!(parse_jsonc(&out).unwrap()["folders"][1]["name"], "B");
        let fresh = render_workspace(None, &json!({"settings": {"y": 2}}), &changed);
        let doc = parse_jsonc(&fresh).unwrap();
        assert_eq!(
            (
                doc["settings"]["y"].clone(),
                doc["folders"][0]["path"].clone()
            ),
            (json!(2), json!("a"))
        );
        assert_eq!(
            render_workspace(Some("\u{feff}{\"folders\":[]}"), &json!({}), &[]),
            "\u{feff}{\"folders\":[]}"
        );
    }

    #[test]
    fn paths_normalize_and_rebase_lexically() {
        assert_eq!(
            normalize_lexical(Path::new("/a/b/../c/./d")),
            Path::new("/a/c/d")
        );
        assert_eq!(normalize_lexical(Path::new("/../x")), Path::new("/x"));
        assert_eq!(
            relative_path(Path::new("/w/ws"), Path::new("/w/two/repo")),
            Path::new("../two/repo")
        );
        assert_eq!(
            relative_path(Path::new("/w"), Path::new("/w")),
            Path::new(".")
        );
        assert_eq!(
            relative_path(Path::new("/w"), Path::new("/w/a/b")),
            Path::new("a/b")
        );
        let rel = json!({"path": "../two/repo", "name": "R"});
        let moved = rebase_entry(
            &rel,
            Some("/w/two/repo"),
            Some(Path::new("/w/ws")),
            Path::new("/w/ws/deep"),
        );
        assert_eq!(moved, json!({"path": "../../two/repo", "name": "R"}));
        let kept = rebase_entry(
            &rel,
            Some("/w/two/repo"),
            Some(Path::new("/w/ws")),
            Path::new("/w/ws"),
        );
        assert_eq!(kept, rel);
        let abs = json!({"path": "/w/two/repo"});
        assert_eq!(
            rebase_entry(&abs, Some("/w/two/repo"), None, Path::new("/x")),
            abs
        );
    }

    #[test]
    fn typed_paths_expand_home_and_resolve_against_the_cwd() {
        let cwd = Path::new("/work/here");
        assert_eq!(absolutize("sub/../x", cwd), Path::new("/work/here/x"));
        assert_eq!(absolutize("/abs/./y", cwd), Path::new("/abs/y"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(absolutize("~/p", cwd), Path::new(&home).join("p"));
            assert_eq!(absolutize("~", cwd), Path::new(&home));
        }
    }

    fn store() -> Store {
        Store::open_in_memory(crate::office::office_migrations()).expect("store")
    }

    #[test]
    fn selection_is_ordered_resolved_and_can_return_to_all_projects() {
        let store = store();
        let nav = Navigation { store: &store };
        assert!(nav.selection().unwrap().is_none());
        let one = nav.create("one").unwrap();
        let two = nav.create("two").unwrap();
        assert_eq!(
            nav.list()
                .unwrap()
                .iter()
                .map(|w| w.workspace_id.clone())
                .collect::<Vec<_>>(),
            [one.clone(), two.clone()],
            "creation order, whatever was opened last"
        );
        nav.select(&one).unwrap();
        assert_eq!(
            nav.recent(8).unwrap()[0].workspace_id,
            one,
            "recent order moves"
        );
        assert_eq!(
            nav.list().unwrap()[0].workspace_id,
            one,
            "switcher order does not"
        );
        assert_eq!(nav.selection().unwrap().unwrap().name, "one");
        nav.select_all().unwrap();
        assert_eq!(nav.selected().unwrap(), None);
        assert!(nav.selection().unwrap().is_none());
    }

    #[test]
    fn apply_rejects_unknown_actions_and_missing_values_with_a_reason() {
        let store = store();
        let nav = Navigation { store: &store };
        assert!(
            nav.apply("open", None, None)
                .unwrap_err()
                .to_string()
                .contains("needs a value")
        );
        assert!(
            nav.apply("add", Some("/x"), None)
                .unwrap_err()
                .to_string()
                .contains("first")
        );
        assert!(
            nav.apply("explode", None, None)
                .unwrap_err()
                .to_string()
                .contains("open | new")
        );
        let made = nav.apply("new", Some("scratch"), None).unwrap();
        assert_eq!(made["message"], "created workspace scratch");
        assert!(made["workspace_id"].is_string());
        let all = nav.apply("select", Some("all"), None).unwrap();
        assert!(all["workspace_id"].is_null());
    }
}
