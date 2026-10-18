//! Session-start projection (issue #114, Resident-Container C0).
//!
//! One member execution gets one directory under `VIVA_HOME`. Pi and Claude
//! Code are pointed at that directory: `soul.md` is a symlink to the member's
//! canonical file, and skill files are symlinks to the team skill bodies.
//! Nothing is written into the session work directory, and `AGENTS.md` /
//! `CLAUDE.md` are never created or replaced.
//!
//! Approved principles stay an empty list until issue #120. Skill weights stay
//! unread until issue #119; ordering then is natal skills first, then relevance
//! to the task, then existing usage. Dialog-state filtering uses
//! [`RecallFilter`](crate::memory::RecallFilter): personal knowledge is included
//! only for an explicit personal state.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{ExecutionId, MemberId, ProjectId};
use crate::foundation::paths::ensure_private_dir;
use crate::foundation::records::get_execution;
use crate::foundation::store::Store;
use crate::harness::{ENV_EXECUTION_ID, ENV_PROJECTION_DIR, ENV_SOUL_PATH, HarnessSpec};
use crate::knowledge::{KnowledgeRegistry, SkillSetService};
use crate::memory::{MemorySearch, MemoryService, RecallFilter};
use crate::skills::SkillBodyStore;
use crate::tasks::TaskRegistry;

/// Hard cap from Resident-Container §7.1. Natal skills over this abort the
/// projection; they are not truncated.
pub const MAX_PROJECTED_SKILLS: usize = 10;

const KNOWLEDGE_BUDGET: usize = 8 * 1024;

const NATAL_HEADING: &str = "本命技能";

/// Who is allowed to replace `soul.md`. Member sessions are refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoulEditor {
    Owner,
    MemberSession,
}

/// Dialog state for this projection. Anything other than explicit personal
/// is the restricted scope (no personal domain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogState {
    Unconfirmed,
    Personal,
}

impl DialogState {
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("personal") => Self::Personal,
            _ => Self::Unconfirmed,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfirmed => "unconfirmed",
            Self::Personal => "personal",
        }
    }

    /// Same filter #116 and #118 share. Unconfirmed does not widen it.
    pub fn recall_filter(self) -> RecallFilter {
        match self {
            Self::Unconfirmed => RecallFilter::default(),
            Self::Personal => RecallFilter {
                include_personal: true,
                ..RecallFilter::default()
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionHarness {
    Pi,
    ClaudeCode,
}

impl SessionHarness {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "pi" => Ok(Self::Pi),
            "claude" | "claude-code" => Ok(Self::ClaudeCode),
            other => Err(Error::Validation(format!(
                "harness `{other}` has no C0 projection; use pi or claude-code"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::ClaudeCode => "claude-code",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedPrinciple {
    pub principle_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedSkill {
    pub skill_id: String,
    pub natal: bool,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedKnowledge {
    pub id: String,
    pub scope: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionManifest {
    pub member_id: String,
    pub execution_id: String,
    pub harness: String,
    pub dialog_state: String,
    pub include_personal: bool,
    pub projection_dir: PathBuf,
    pub soul_path: PathBuf,
    pub work_dir: PathBuf,
    pub principles: Vec<ApprovedPrinciple>,
    pub skills: Vec<ProjectedSkill>,
    pub knowledge: Vec<ProjectedKnowledge>,
    /// The execution's existing task brief, when one was already stored.
    /// Projection does not invent a work-package document.
    pub task_brief: Option<serde_json::Value>,
    pub memory_note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectRequest<'a> {
    pub member_id: &'a MemberId,
    pub execution_id: &'a ExecutionId,
    pub harness: SessionHarness,
    pub dialog: DialogState,
    pub work_dir: &'a Path,
}

pub fn soul_path(home: &Path, member_id: &MemberId) -> PathBuf {
    home.join("members")
        .join(member_id.as_str())
        .join("soul.md")
}

pub fn projection_dir(home: &Path, member_id: &MemberId, execution_id: &ExecutionId) -> PathBuf {
    home.join("projections")
        .join(member_id.as_str())
        .join(execution_id.as_str())
}

/// Replace the member's canonical `soul.md`. Only [`SoulEditor::Owner`] succeeds.
pub fn write_soul(home: &Path, member_id: &MemberId, body: &str, editor: SoulEditor) -> Result<()> {
    if editor == SoulEditor::MemberSession {
        return Err(Error::Validation(
            "soul.md is only modified by Haisu; a member session cannot change it".into(),
        ));
    }
    if body.trim().is_empty() {
        return Err(Error::Validation("soul.md must not be empty".into()));
    }
    let path = soul_path(home, member_id);
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let tmp = path.with_extension("md.partial");
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn read_soul(home: &Path, member_id: &MemberId) -> Result<String> {
    let path = soul_path(home, member_id);
    fs::read_to_string(&path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::Validation(format!(
                "member `{}` has no soul.md; Haisu must write it before projection starts",
                member_id.as_str()
            ))
        } else {
            Error::Io(err)
        }
    })
}

/// Skill weight for ordering. Issue #119 owns the stored weight.
/// Until then this is `None`, and projection uses usage as the second key.
pub fn current_skill_weight(
    _store: &Store,
    _member_id: &MemberId,
    _skill_id: &str,
) -> Result<Option<f64>> {
    Ok(None)
}

/// Approved principles. Issue #120 owns approval and storage.
/// Until then the list is empty, and that is not a projection failure.
pub fn approved_principles(
    _store: &Store,
    _member_id: &MemberId,
) -> Result<Vec<ApprovedPrinciple>> {
    Ok(Vec::new())
}

pub fn natal_skill_ids(soul: &str) -> Vec<String> {
    let mut in_section = false;
    let mut ids = Vec::new();
    for line in soul.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("## ") {
            in_section = title.trim() == NATAL_HEADING;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        let id = item.trim().trim_matches('`').trim();
        if id.is_empty() || ids.iter().any(|existing| existing == id) {
            continue;
        }
        ids.push(id.to_string());
    }
    ids
}

/// Project this member into `VIVA_HOME` for one execution.
/// Validation failures leave no projection directory behind.
pub fn project(
    store: &Store,
    home: &Path,
    request: &ProjectRequest<'_>,
) -> Result<ProjectionManifest> {
    if !request.work_dir.is_absolute() {
        return Err(Error::Validation(
            "projection work directory must be absolute".into(),
        ));
    }
    crate::members::MemberRegistry::new(store).require(request.member_id)?;
    let execution = get_execution(store, request.execution_id)?.ok_or_else(|| Error::NotFound {
        entity: "execution",
        id: request.execution_id.to_string(),
    })?;
    if &execution.attribution.member_id != request.member_id {
        return Err(Error::Validation(format!(
            "execution `{}` belongs to member `{}`, not `{}`; projection did not start",
            request.execution_id.as_str(),
            execution.attribution.member_id.as_str(),
            request.member_id.as_str()
        )));
    }
    let task = TaskRegistry::new(store).require_task(&execution.task_id)?;
    let soul = read_soul(home, request.member_id)?;
    let natal = natal_skill_ids(&soul);
    if natal.len() > MAX_PROJECTED_SKILLS {
        return Err(Error::Validation(format!(
            "本命技能有 {} 个，超过上限 {}。这次投影没有开始，没有写入投影文件。请 Haisu 修改 soul.md。",
            natal.len(),
            MAX_PROJECTED_SKILLS
        )));
    }

    let canonical_soul = soul_path(home, request.member_id);
    let dest = projection_dir(home, request.member_id, request.execution_id);
    refuse_protected_write(&dest, request.work_dir)?;

    let filter = request.dialog.recall_filter();
    let principles = approved_principles(store, request.member_id)?;
    let task_terms = token_set(&format!("{} {}", task.goal, task.constraints.join(" ")));
    let selected_skills = select_skills(store, request.member_id, &natal, &task_terms)?;
    let (knowledge, memory_note) = select_knowledge(
        store,
        request.member_id,
        task.project_id.as_ref(),
        &task.goal,
        &task_terms,
        &filter,
    )?;
    let task_brief = TaskRegistry::new(store).latest_brief(&task.task_id)?;

    let parent = dest
        .parent()
        .ok_or_else(|| Error::Validation("projection path has no parent".into()))?;
    ensure_private_dir(parent)?;
    let tmp = parent.join(format!(".{}.partial", request.execution_id.as_str()));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    let published = write_tree(TreeWrite {
        dir: &tmp,
        published_dir: &dest,
        canonical_soul: &canonical_soul,
        skills: &selected_skills,
        knowledge: &knowledge,
        principles: &principles,
        request,
        filter: &filter,
        task_brief,
        memory_note,
    });
    let manifest = match published {
        Ok(manifest) => manifest,
        Err(err) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(err);
        }
    };
    let backup = parent.join(format!(".{}.backup", request.execution_id.as_str()));
    if dest.exists() {
        let _ = fs::remove_dir_all(&backup);
        fs::rename(&dest, &backup)?;
    }
    if let Err(err) = fs::rename(&tmp, &dest) {
        if backup.exists() {
            let _ = fs::rename(&backup, &dest);
        }
        let _ = fs::remove_dir_all(&tmp);
        return Err(err.into());
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(manifest)
}

pub fn inject_projection_env(mut spec: HarnessSpec, manifest: &ProjectionManifest) -> HarnessSpec {
    spec = spec
        .with_env(
            ENV_PROJECTION_DIR,
            manifest.projection_dir.to_string_lossy(),
        )
        .with_env(ENV_SOUL_PATH, manifest.soul_path.to_string_lossy())
        .with_env(ENV_EXECUTION_ID, &manifest.execution_id);
    spec
}

struct SkillAsset {
    skill_id: String,
    text: String,
    body_path: Option<PathBuf>,
    inline_body: Option<String>,
}

fn select_skills(
    store: &Store,
    member_id: &MemberId,
    natal: &[String],
    task_terms: &HashSet<String>,
) -> Result<Vec<(SkillAsset, bool)>> {
    let mut chosen: Vec<(SkillAsset, bool)> = Vec::new();
    for skill_id in natal {
        let Some(asset) = resolve_skill(store, skill_id, true)? else {
            return Err(Error::Validation(format!(
                "本命技能 `{skill_id}` 不能投影。这次投影没有开始，没有写入投影文件。"
            )));
        };
        chosen.push((asset, true));
    }
    let remaining = MAX_PROJECTED_SKILLS - chosen.len();
    if remaining == 0 {
        return Ok(chosen);
    }
    let service = SkillSetService::new(store);
    let mut ranked = Vec::new();
    for membership in service.list_member_skills(member_id)? {
        if natal.iter().any(|id| id == &membership.skill_id) {
            continue;
        }
        let Some(asset) = resolve_skill(store, &membership.skill_id, false)? else {
            continue;
        };
        let relevance = relevance(task_terms, &asset.text);
        let weight = current_skill_weight(store, member_id, &membership.skill_id)?;
        let usage = usage_score(store, member_id, &membership.skill_id)?;
        let secondary = weight.unwrap_or(usage);
        ranked.push((relevance, secondary, asset));
    }
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| a.2.skill_id.cmp(&b.2.skill_id))
    });
    ranked.truncate(remaining);
    chosen.extend(ranked.into_iter().map(|(_, _, asset)| (asset, false)));
    Ok(chosen)
}

fn resolve_skill(store: &Store, skill_id: &str, natal: bool) -> Result<Option<SkillAsset>> {
    let knowledge = KnowledgeRegistry::new(store);
    if let Some(registration) = knowledge.skill(skill_id)? {
        if !registration.enabled {
            if natal {
                return Err(Error::Validation(format!(
                    "本命技能 `{skill_id}` 已停用。这次投影没有开始，没有写入投影文件。"
                )));
            }
            return Ok(None);
        }
        if !registration.entry_path.is_file() {
            return Err(Error::Validation(format!(
                "skill `{skill_id}` entry file is missing; projection did not start"
            )));
        }
        let title = knowledge
            .get_item(&registration.item_id)?
            .map(|item| item.title)
            .unwrap_or_else(|| skill_id.to_string());
        let body = fs::read_to_string(&registration.entry_path)?;
        return Ok(Some(SkillAsset {
            skill_id: skill_id.to_string(),
            text: format!("{title}\n{body}"),
            body_path: Some(registration.entry_path),
            inline_body: None,
        }));
    }
    if let Some(current) = SkillBodyStore::new(store).current_body(skill_id)? {
        return Ok(Some(SkillAsset {
            skill_id: skill_id.to_string(),
            text: format!("{}\n{}", current.skill.name, current.version.body),
            body_path: None,
            inline_body: Some(current.version.body),
        }));
    }
    if natal {
        return Err(Error::Validation(format!(
            "本命技能 `{skill_id}` 不在技能库中。这次投影没有开始，没有写入投影文件。"
        )));
    }
    Ok(None)
}

fn usage_score(store: &Store, member_id: &MemberId, skill_id: &str) -> Result<f64> {
    let score: Option<f64> = store.connection().query_row(
        "SELECT SUM(score) FROM skill_used_in WHERE member_id = ?1 AND skill_id = ?2",
        rusqlite::params![member_id.as_str(), skill_id],
        |row| row.get(0),
    )?;
    Ok(score.unwrap_or(0.0))
}

struct KnowledgeAsset {
    id: String,
    scope: String,
    body_path: Option<PathBuf>,
    inline_body: Option<String>,
    bytes: usize,
}

fn select_knowledge(
    store: &Store,
    member_id: &MemberId,
    project_id: Option<&ProjectId>,
    task_goal: &str,
    task_terms: &HashSet<String>,
    filter: &RecallFilter,
) -> Result<(Vec<KnowledgeAsset>, Option<String>)> {
    let mut ranked = knowledge_items(store, member_id, project_id, task_terms, filter)?;
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    let mut selected = Vec::new();
    let mut used = 0usize;
    for (_score, asset) in ranked {
        if asset.bytes == 0 || used + asset.bytes > KNOWLEDGE_BUDGET {
            continue;
        }
        used += asset.bytes;
        selected.push(asset);
    }

    let mut memory_note = None;
    if !task_goal.trim().is_empty() {
        match MemoryService::new(store).search_filtered(
            member_id,
            project_id,
            task_goal,
            KNOWLEDGE_BUDGET.saturating_sub(used),
            filter,
        )? {
            MemorySearch::Unavailable { reason } => {
                memory_note = Some(format!("memory recall unavailable: {reason}"));
            }
            MemorySearch::Fetched { facts, .. } => {
                for fact in facts {
                    if !filter.include_personal && fact.knowledge_kind == "personal_memory" {
                        continue;
                    }
                    let body = format!("{}\n{}", fact.summary, fact.content);
                    let bytes = body.len();
                    if bytes == 0 || used + bytes > KNOWLEDGE_BUDGET {
                        continue;
                    }
                    used += bytes;
                    selected.push(KnowledgeAsset {
                        id: format!("fact-{}", fact.fact_id),
                        scope: fact.knowledge_kind,
                        body_path: None,
                        inline_body: Some(body),
                        bytes,
                    });
                }
            }
        }
    }
    Ok((selected, memory_note))
}

fn knowledge_items(
    store: &Store,
    member_id: &MemberId,
    project_id: Option<&ProjectId>,
    task_terms: &HashSet<String>,
    filter: &RecallFilter,
) -> Result<Vec<(i64, KnowledgeAsset)>> {
    let mut stmt = store.connection().prepare(
        "SELECT item_id, title, scope, owner_member_id, project_id, tags_json, body_path
         FROM knowledge_items WHERE status = 'active'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut ranked = Vec::new();
    for row in rows {
        let (id, title, scope, owner, item_project, tags_json, body_path) = row?;
        if !knowledge_visible(
            &scope,
            owner.as_deref(),
            item_project.as_deref(),
            filter,
            member_id,
            project_id,
        ) {
            continue;
        }
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap_or_default();
        let path = PathBuf::from(&body_path);
        let body = fs::read_to_string(&path).unwrap_or_default();
        let text = format!("{title}\n{}\n{body}", tags.join(" "));
        let score = relevance(task_terms, &text);
        if score == 0 {
            continue;
        }
        let bytes = body.len();
        let on_disk = path.is_file();
        ranked.push((
            score,
            KnowledgeAsset {
                id,
                scope,
                body_path: if on_disk { Some(path) } else { None },
                inline_body: if on_disk { None } else { Some(body) },
                bytes,
            },
        ));
    }
    Ok(ranked)
}

fn knowledge_visible(
    scope: &str,
    owner: Option<&str>,
    item_project: Option<&str>,
    filter: &RecallFilter,
    viewer: &MemberId,
    task_project: Option<&ProjectId>,
) -> bool {
    match scope {
        "personal" => filter.include_personal && owner == Some(viewer.as_str()),
        "project" => {
            filter.include_project
                && task_project.is_some()
                && item_project == task_project.map(|id| id.as_str())
        }
        "team" => filter.include_team,
        _ => false,
    }
}

struct TreeWrite<'a> {
    dir: &'a Path,
    published_dir: &'a Path,
    canonical_soul: &'a Path,
    skills: &'a [(SkillAsset, bool)],
    knowledge: &'a [KnowledgeAsset],
    principles: &'a [ApprovedPrinciple],
    request: &'a ProjectRequest<'a>,
    filter: &'a RecallFilter,
    task_brief: Option<serde_json::Value>,
    memory_note: Option<String>,
}

fn write_tree(input: TreeWrite<'_>) -> Result<ProjectionManifest> {
    let TreeWrite {
        dir,
        published_dir,
        canonical_soul,
        skills,
        knowledge,
        principles,
        request,
        filter,
        task_brief,
        memory_note,
    } = input;
    refuse_protected_write(dir, request.work_dir)?;
    let soul_link = dir.join("soul.md");
    std::os::unix::fs::symlink(canonical_soul, &soul_link)?;
    fs::write(
        dir.join("principles.json"),
        serde_json::to_vec_pretty(principles)?,
    )?;

    let skills_root = dir.join("skills");
    fs::create_dir_all(&skills_root)?;
    let mut projected_skills = Vec::new();
    for (asset, natal) in skills {
        let skill_dir = skills_root.join(&asset.skill_id);
        fs::create_dir_all(&skill_dir)?;
        let dest = skill_dir.join("SKILL.md");
        if let Some(source) = &asset.body_path {
            std::os::unix::fs::symlink(source, &dest)?;
        } else if let Some(body) = &asset.inline_body {
            fs::write(&dest, body)?;
        }
        projected_skills.push(ProjectedSkill {
            skill_id: asset.skill_id.clone(),
            natal: *natal,
            path: format!("skills/{}/SKILL.md", asset.skill_id),
        });
    }

    let knowledge_root = dir.join("knowledge");
    fs::create_dir_all(&knowledge_root)?;
    let mut projected_knowledge = Vec::new();
    for asset in knowledge {
        let dest = knowledge_root.join(format!("{}.md", asset.id));
        if let Some(source) = &asset.body_path {
            std::os::unix::fs::symlink(source, &dest)?;
        } else if let Some(body) = &asset.inline_body {
            fs::write(&dest, body)?;
        }
        projected_knowledge.push(ProjectedKnowledge {
            id: asset.id.clone(),
            scope: asset.scope.clone(),
            path: format!("knowledge/{}.md", asset.id),
        });
    }

    fs::create_dir_all(dir.join(".claude-plugin"))?;
    fs::write(
        dir.join(".claude-plugin/plugin.json"),
        serde_json::to_vec_pretty(&json!({
            "name": "viva-member-projection",
            "description": "Skills projected for one member execution. Bodies are the member's skill files, not a second copy."
        }))?,
    )?;

    let manifest = ProjectionManifest {
        member_id: request.member_id.to_string(),
        execution_id: request.execution_id.to_string(),
        harness: request.harness.as_str().to_string(),
        dialog_state: request.dialog.as_str().to_string(),
        include_personal: filter.include_personal,
        projection_dir: published_dir.to_path_buf(),
        soul_path: canonical_soul.to_path_buf(),
        work_dir: request.work_dir.to_path_buf(),
        principles: principles.to_vec(),
        skills: projected_skills,
        knowledge: projected_knowledge,
        task_brief,
        memory_note,
    };
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(manifest)
}

fn refuse_protected_write(projection: &Path, work_dir: &Path) -> Result<()> {
    if projection.starts_with(work_dir) {
        return Err(Error::Validation(
            "projection directory must stay outside the session work directory".into(),
        ));
    }
    for name in ["AGENTS.md", "CLAUDE.md", "CLAUDE.local.md"] {
        if projection.ends_with(name) {
            return Err(Error::Validation(format!(
                "projection must not write `{name}`"
            )));
        }
    }
    Ok(())
}

fn relevance(terms: &HashSet<String>, text: &str) -> i64 {
    let hay = token_set(text);
    terms.intersection(&hay).count() as i64
}

fn token_set(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natal_ids_come_only_from_the_heading() {
        let soul = "\
## 职责
- stay inside the grant

## 本命技能
- skill-a
- skill-b
* skill-a

## 边界
- skill-not-natal
";
        assert_eq!(natal_skill_ids(soul), vec!["skill-a", "skill-b"]);
    }

    #[test]
    fn unconfirmed_dialog_excludes_personal() {
        assert!(!DialogState::parse(None).recall_filter().include_personal);
        assert!(
            !DialogState::parse(Some("work"))
                .recall_filter()
                .include_personal
        );
        assert!(
            DialogState::parse(Some("personal"))
                .recall_filter()
                .include_personal
        );
    }
}
