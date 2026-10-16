//! Mutations for the server-owned scene / tab / pane tree.
//!
//! The resident server is the authority. A client that is looking at one
//! tab commits that tab's grid, focus and zoom; other tabs stay as the
//! server last recorded them. Closing a pane records the terminal id and
//! does not stop the process — stopping stays on the explicit terminal
//! stop path. This is Viva's own tree, not a port of another workbench.

use super::scenes::{FLOATING, MAX_SCENES, MAX_TABS, Scene, SceneBook, TabView};
use crate::foundation::error::{Error, Result};
use crate::server::WorkbenchTreeOp;
use crate::tui::layout::{PaneContent, PaneNode, SplitAxis};
use serde_json::Value;
use std::collections::BTreeSet;

/// Read a stored layout. `Ok(None)` means the JSON is not a scene book
/// (the caller keeps the raw value). Versioned books must already be
/// valid. A pre-version `{grid, focus}` payload is migrated.
pub fn import_stored(
    value: &Value,
    scene_for_terminal: impl Fn(&str) -> String,
) -> Result<Option<SceneBook>> {
    if value.get("version").is_some() {
        let book: SceneBook = serde_json::from_value(value.clone())
            .map_err(|e| Error::Validation(format!("scene tree: {e}")))?;
        if !book.valid() {
            return Err(Error::Validation(
                "scene tree has invalid references or bounds".into(),
            ));
        }
        return Ok(Some(book));
    }
    if value.get("grid").is_none() {
        return Ok(None);
    }
    Ok(Some(migrate_legacy(value, scene_for_terminal)?))
}

fn migrate_legacy(value: &Value, scene_for_terminal: impl Fn(&str) -> String) -> Result<SceneBook> {
    let grid: PaneNode = serde_json::from_value(value.get("grid").cloned().unwrap_or(Value::Null))
        .map_err(|e| Error::Validation(format!("legacy layout grid: {e}")))?;
    let mut book = SceneBook {
        legacy: Some(value.clone()),
        ..SceneBook::default()
    };
    if let Some(width) = value.get("sidebar").and_then(|v| v.as_u64()) {
        if width <= u64::from(u16::MAX) {
            book.sidebar = width as u16;
        }
    }
    let mut keys = BTreeSet::new();
    for content in grid.leaves() {
        if let PaneContent::Terminal(id) = content {
            keys.insert(scene_for_terminal(&id));
        }
    }
    if keys.is_empty() {
        keys.insert(FLOATING.into());
    }
    for key in keys {
        let mut tree = grid.clone();
        let keep = tree
            .leaves()
            .into_iter()
            .filter_map(|content| match content {
                PaneContent::Terminal(id) if scene_for_terminal(&id) == key => Some(id),
                _ => None,
            })
            .collect();
        tree.prune_dead_terminals(&keep);
        let mut tab = TabView::new("Migrated".into());
        tab.grid = tree;
        tab.focus = value
            .get("focus")
            .and_then(|focus| serde_json::from_value(focus.clone()).ok())
            .filter(|focus: &PaneContent| tab.grid.leaves().contains(focus))
            .unwrap_or_else(|| tab.grid.leaves()[0].clone());
        book.scenes.insert(
            key.clone(),
            Scene {
                active: tab.id.clone(),
                tabs: vec![tab],
                ..Scene::default()
            },
        );
        book.selected = key;
    }
    if !book.valid() {
        return Err(Error::Validation(
            "migrated layout is not a valid scene tree".into(),
        ));
    }
    Ok(book)
}

/// Fold a client's book into the server book.
///
/// The tab the client is looking at (`selected` scene, its `active` tab)
/// is replaced — the later focus or size wins for that tab. Every other
/// tab keeps the server's grid, focus, zoom and name. A tab the client
/// has that the server lacks is inserted. `hidden: true` is applied so a
/// closed tab stays closed; a stale client cannot unhide a tab it is not
/// looking at.
pub fn merge_client_book(server: &mut SceneBook, client: &SceneBook) -> Result<()> {
    if !client.valid() {
        return Err(Error::Validation(
            "client scene tree has invalid references or bounds".into(),
        ));
    }
    let mut next = server.clone();
    next.sidebar = client.sidebar;
    if !client.selected.is_empty() {
        next.selected = client.selected.clone();
    }
    for (scene_id, client_scene) in &client.scenes {
        if !next.scenes.contains_key(scene_id) && next.scenes.len() >= MAX_SCENES {
            return Err(Error::Validation(format!(
                "scene limit reached ({MAX_SCENES})"
            )));
        }
        let server_scene = next.scenes.entry(scene_id.clone()).or_default();
        if !client_scene.name.is_empty() {
            server_scene.name = client_scene.name.clone();
        }
        if scene_id == &client.selected {
            server_scene.active = client_scene.active.clone();
        }
        for client_tab in &client_scene.tabs {
            let viewing = scene_id == &client.selected && client_scene.active == client_tab.id;
            if let Some(server_tab) = server_scene
                .tabs
                .iter_mut()
                .find(|tab| tab.id == client_tab.id)
            {
                if client_tab.hidden {
                    server_tab.hidden = true;
                }
                if viewing {
                    server_tab.grid = client_tab.grid.clone();
                    server_tab.focus = client_tab.focus.clone();
                    server_tab.zoom = client_tab.zoom;
                    server_tab.name = client_tab.name.clone();
                    server_tab.hidden = client_tab.hidden;
                    server_tab.closed = client_tab.closed.clone();
                    if !client_tab.pane_names.is_empty() {
                        server_tab.pane_names = client_tab.pane_names.clone();
                    }
                }
            } else {
                if server_scene.tabs.len() >= MAX_TABS {
                    return Err(Error::Validation(format!("tab limit reached ({MAX_TABS})")));
                }
                server_scene.tabs.push(client_tab.clone());
            }
        }
    }
    if !next.valid() {
        return Err(Error::Validation(
            "merged scene tree has invalid references or bounds".into(),
        ));
    }
    *server = next;
    Ok(())
}

/// Apply one tree edit. `Ok(false)` means the op is not a tree edit
/// (`List`, pane input, pane snapshot). Closing a pane only edits the
/// tree.
pub fn apply(book: &mut SceneBook, op: &WorkbenchTreeOp) -> Result<bool> {
    if !op.edits_tree() {
        return Ok(false);
    }
    let mut next = book.clone();
    match op {
        WorkbenchTreeOp::CreateScene { id, name } => create_scene(&mut next, id.as_deref(), name)?,
        WorkbenchTreeOp::RenameScene { id, name } => rename_scene(&mut next, id, name)?,
        WorkbenchTreeOp::CloseScene { id } => close_scene(&mut next, id)?,
        WorkbenchTreeOp::FocusScene { id } => focus_scene(&mut next, id)?,
        WorkbenchTreeOp::CreateTab { scene_id, name } => {
            create_tab(&mut next, scene_id.as_deref(), name)?;
        }
        WorkbenchTreeOp::RenameTab {
            scene_id,
            tab_id,
            name,
        } => rename_tab(&mut next, scene_id.as_deref(), tab_id, name)?,
        WorkbenchTreeOp::CloseTab { scene_id, tab_id } => {
            close_tab(&mut next, scene_id.as_deref(), tab_id)?;
        }
        WorkbenchTreeOp::FocusTab { scene_id, tab_id } => {
            focus_tab(&mut next, scene_id.as_deref(), tab_id)?;
        }
        WorkbenchTreeOp::CreatePane {
            scene_id,
            tab_id,
            terminal_id,
            direction,
        } => split_into(
            &mut next,
            scene_id.as_deref(),
            tab_id.as_deref(),
            None,
            direction.as_deref().unwrap_or("right"),
            terminal_id.as_deref(),
        )?,
        WorkbenchTreeOp::RenamePane {
            scene_id,
            tab_id,
            pane_id,
            name,
        } => rename_pane(
            &mut next,
            scene_id.as_deref(),
            tab_id.as_deref(),
            pane_id,
            name,
        )?,
        WorkbenchTreeOp::ClosePane {
            scene_id,
            tab_id,
            pane_id,
        } => close_pane(&mut next, scene_id.as_deref(), tab_id.as_deref(), pane_id)?,
        WorkbenchTreeOp::FocusPane {
            scene_id,
            tab_id,
            pane_id,
        } => focus_pane(&mut next, scene_id.as_deref(), tab_id.as_deref(), pane_id)?,
        WorkbenchTreeOp::Split {
            scene_id,
            tab_id,
            pane_id,
            direction,
            terminal_id,
        } => split_into(
            &mut next,
            scene_id.as_deref(),
            tab_id.as_deref(),
            pane_id.as_deref(),
            direction,
            terminal_id.as_deref(),
        )?,
        WorkbenchTreeOp::Zoom {
            scene_id,
            tab_id,
            on,
        } => zoom(&mut next, scene_id.as_deref(), tab_id.as_deref(), *on)?,
        WorkbenchTreeOp::Resize {
            scene_id,
            tab_id,
            path,
            ratio,
        } => resize(
            &mut next,
            scene_id.as_deref(),
            tab_id.as_deref(),
            path,
            *ratio,
        )?,
        WorkbenchTreeOp::List
        | WorkbenchTreeOp::PaneInput { .. }
        | WorkbenchTreeOp::PaneSnapshot { .. } => {}
    }
    if !next.valid() {
        return Err(Error::Validation(
            "scene tree edit was rejected (invalid references or bounds)".into(),
        ));
    }
    *book = next;
    Ok(true)
}

/// The terminal behind a pane. Browser panes have none. A closed pane
/// still resolves: closing did not stop the PTY.
pub fn resolve_pane_terminal(
    book: &SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
    pane_id: &str,
) -> Result<String> {
    if pane_id.is_empty() || pane_id == "browser" {
        return Err(Error::Validation(
            "pane `browser` has no terminal to read or write".into(),
        ));
    }
    let location = book.locate(pane_id).ok_or(Error::NotFound {
        entity: "pane",
        id: pane_id.to_string(),
    })?;
    if let Some(scene_id) = scene_id.filter(|id| !id.is_empty()) {
        if location.scene_id != scene_id {
            return Err(Error::NotFound {
                entity: "pane",
                id: pane_id.to_string(),
            });
        }
    }
    if let Some(tab_id) = tab_id.filter(|id| !id.is_empty()) {
        if location.tab_id != tab_id {
            return Err(Error::NotFound {
                entity: "pane",
                id: pane_id.to_string(),
            });
        }
    }
    Ok(pane_id.to_string())
}

fn check_label(kind: &str, text: &str, allow_empty: bool) -> Result<()> {
    if text.len() > 256 || (!allow_empty && text.trim().is_empty()) {
        return Err(Error::Validation(format!(
            "{kind} name must be 1..=256 characters"
        )));
    }
    Ok(())
}

fn create_scene(book: &mut SceneBook, id: Option<&str>, name: &str) -> Result<()> {
    check_label("scene", name, true)?;
    if book.scenes.len() >= MAX_SCENES {
        return Err(Error::Validation(format!(
            "scene limit reached ({MAX_SCENES})"
        )));
    }
    let id = match id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            if id.len() > 256 {
                return Err(Error::Validation(
                    "scene id must be 1..=256 characters".into(),
                ));
            }
            id.to_string()
        }
        None => format!("scene-{}", uuid::Uuid::new_v4().simple()),
    };
    if book.scenes.contains_key(&id) {
        return Err(Error::Validation(format!("scene `{id}` already exists")));
    }
    book.scenes.insert(
        id,
        Scene {
            name: name.trim().to_string(),
            ..Scene::default()
        },
    );
    Ok(())
}

fn rename_scene(book: &mut SceneBook, id: &str, name: &str) -> Result<()> {
    check_label("scene", name, false)?;
    let scene = book.scenes.get_mut(id).ok_or(Error::NotFound {
        entity: "scene",
        id: id.to_string(),
    })?;
    scene.name = name.trim().to_string();
    Ok(())
}

fn close_scene(book: &mut SceneBook, id: &str) -> Result<()> {
    if book.scenes.remove(id).is_none() {
        return Err(Error::NotFound {
            entity: "scene",
            id: id.to_string(),
        });
    }
    if book.selected == id {
        book.selected = book
            .scenes
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| FLOATING.into());
    }
    Ok(())
}

fn focus_scene(book: &mut SceneBook, id: &str) -> Result<()> {
    if !book.scenes.contains_key(id) {
        return Err(Error::NotFound {
            entity: "scene",
            id: id.to_string(),
        });
    }
    book.selected = id.to_string();
    Ok(())
}

fn ensure_scene(book: &mut SceneBook, scene_id: Option<&str>) -> Result<String> {
    let key = scene_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| book.selected.clone());
    if key.is_empty() || key.len() > 256 {
        return Err(Error::Validation(
            "scene id must be 1..=256 characters".into(),
        ));
    }
    if !book.scenes.contains_key(&key) {
        if book.scenes.len() >= MAX_SCENES {
            return Err(Error::Validation(format!(
                "scene limit reached ({MAX_SCENES})"
            )));
        }
        book.scenes.insert(key.clone(), Scene::default());
    }
    Ok(key)
}

fn create_tab(book: &mut SceneBook, scene_id: Option<&str>, name: &str) -> Result<()> {
    check_label("tab", name, false)?;
    let key = ensure_scene(book, scene_id)?;
    let scene = book.scenes.get_mut(&key).expect("scene just ensured");
    if scene.tabs.len() >= MAX_TABS {
        return Err(Error::Validation(format!("tab limit reached ({MAX_TABS})")));
    }
    let tab = TabView::new(name.trim().to_string());
    scene.active = tab.id.clone();
    scene.tabs.push(tab);
    book.selected = key;
    Ok(())
}

fn tab_mut<'a>(
    book: &'a mut SceneBook,
    scene_id: Option<&str>,
    tab_id: &str,
) -> Result<(String, &'a mut TabView)> {
    let key = scene_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| book.selected.clone());
    let scene = book.scenes.get_mut(&key).ok_or(Error::NotFound {
        entity: "scene",
        id: key.clone(),
    })?;
    if !scene.tabs.iter().any(|tab| tab.id == tab_id) {
        return Err(Error::NotFound {
            entity: "tab",
            id: tab_id.to_string(),
        });
    }
    let tab = scene
        .tabs
        .iter_mut()
        .find(|tab| tab.id == tab_id)
        .expect("tab present");
    Ok((key, tab))
}

fn rename_tab(
    book: &mut SceneBook,
    scene_id: Option<&str>,
    tab_id: &str,
    name: &str,
) -> Result<()> {
    check_label("tab", name, false)?;
    let (_, tab) = tab_mut(book, scene_id, tab_id)?;
    tab.name = name.trim().to_string();
    Ok(())
}

fn close_tab(book: &mut SceneBook, scene_id: Option<&str>, tab_id: &str) -> Result<()> {
    let key = scene_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| book.selected.clone());
    let scene = book.scenes.get_mut(&key).ok_or(Error::NotFound {
        entity: "scene",
        id: key,
    })?;
    let Some(tab) = scene.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
        return Err(Error::NotFound {
            entity: "tab",
            id: tab_id.to_string(),
        });
    };
    tab.hidden = true;
    if scene.active == tab_id {
        scene.active = scene
            .tabs
            .iter()
            .find(|tab| !tab.hidden)
            .map(|tab| tab.id.clone())
            .unwrap_or_default();
    }
    Ok(())
}

fn focus_tab(book: &mut SceneBook, scene_id: Option<&str>, tab_id: &str) -> Result<()> {
    let (key, _) = tab_mut(book, scene_id, tab_id)?;
    let scene = book.scenes.get_mut(&key).expect("scene present");
    let visible = scene.tabs.iter().any(|tab| tab.id == tab_id && !tab.hidden);
    if !visible {
        return Err(Error::Validation(format!(
            "tab `{tab_id}` is hidden; focus a visible tab"
        )));
    }
    scene.active = tab_id.to_string();
    book.selected = key;
    Ok(())
}

fn parse_axis(direction: &str) -> Result<SplitAxis> {
    match direction {
        "right" => Ok(SplitAxis::Horizontal),
        "down" => Ok(SplitAxis::Vertical),
        other => Err(Error::Validation(format!(
            "split direction `{other}` must be right or down"
        ))),
    }
}

fn pane_content(pane_id: &str) -> Result<PaneContent> {
    if pane_id.is_empty() {
        return Err(Error::Validation("pane id is empty".into()));
    }
    if pane_id == "browser" {
        Ok(PaneContent::Browser)
    } else {
        Ok(PaneContent::Terminal(pane_id.to_string()))
    }
}

fn terminal_in_grid(book: &SceneBook, terminal_id: &str) -> bool {
    let needle = PaneContent::Terminal(terminal_id.to_string());
    book.scenes.values().any(|scene| {
        scene
            .tabs
            .iter()
            .any(|tab| tab.grid.leaves().contains(&needle))
    })
}

fn forget_closed(book: &mut SceneBook, terminal_id: &str) {
    for scene in book.scenes.values_mut() {
        for tab in &mut scene.tabs {
            tab.closed.retain(|id| id != terminal_id);
        }
    }
}

fn split_into(
    book: &mut SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
    target_pane: Option<&str>,
    direction: &str,
    terminal_id: Option<&str>,
) -> Result<()> {
    let axis = parse_axis(direction)?;
    let content = match terminal_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            if id.len() > 256 {
                return Err(Error::Validation("pane terminal id is too long".into()));
            }
            if terminal_in_grid(book, id) {
                return Err(Error::Validation(format!(
                    "terminal `{id}` is already placed in the tree"
                )));
            }
            forget_closed(book, id);
            PaneContent::Terminal(id.to_string())
        }
        None => PaneContent::Browser,
    };
    let (scene_key, tab) = require_tab(book, scene_id, tab_id)?;
    if tab.hidden {
        return Err(Error::Validation(
            "cannot split a hidden tab; focus it first".into(),
        ));
    }
    if let Some(target) = target_pane.map(str::trim).filter(|id| !id.is_empty()) {
        let focus = pane_content(target)?;
        if !tab.grid.leaves().contains(&focus) {
            return Err(Error::NotFound {
                entity: "pane",
                id: target.to_string(),
            });
        }
        tab.focus = focus;
    }
    if !tab.grid.leaves().contains(&tab.focus) {
        tab.focus = tab.grid.leaves()[0].clone();
    }
    let placed = tab.grid.split(&tab.focus, axis, content.clone());
    if !placed {
        return Err(Error::Validation(format!(
            "pane limit reached ({})",
            crate::tui::layout::MAX_PANES
        )));
    }
    if let PaneContent::Terminal(id) = &content {
        tab.closed.retain(|closed| closed != id);
    }
    let tab_key = tab.id.clone();
    tab.focus = content;
    scene_active_and_selected(book, &scene_key, &tab_key);
    Ok(())
}

fn scene_active_and_selected(book: &mut SceneBook, scene_id: &str, tab_id: &str) {
    if let Some(scene) = book.scenes.get_mut(scene_id) {
        scene.active = tab_id.to_string();
    }
    book.selected = scene_id.to_string();
}

fn rename_pane(
    book: &mut SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
    pane_id: &str,
    name: &str,
) -> Result<()> {
    check_label("pane", name, false)?;
    let (_, tab) = require_tab(book, scene_id, tab_id)?;
    let known = tab.grid.leaves().iter().any(|content| match content {
        PaneContent::Browser => pane_id == "browser",
        PaneContent::Terminal(id) => id == pane_id,
    }) || tab.closed.iter().any(|id| id == pane_id);
    if !known {
        return Err(Error::NotFound {
            entity: "pane",
            id: pane_id.to_string(),
        });
    }
    tab.pane_names
        .insert(pane_id.to_string(), name.trim().to_string());
    Ok(())
}

fn require_tab<'a>(
    book: &'a mut SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
) -> Result<(String, &'a mut TabView)> {
    let key = scene_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| book.selected.clone());
    let scene = book.scenes.get_mut(&key).ok_or(Error::NotFound {
        entity: "scene",
        id: key.clone(),
    })?;
    let tab_key = tab_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| scene.active.clone());
    if !scene.tabs.iter().any(|tab| tab.id == tab_key) {
        return Err(Error::NotFound {
            entity: "tab",
            id: tab_key,
        });
    }
    let tab = scene
        .tabs
        .iter_mut()
        .find(|tab| tab.id == tab_key)
        .expect("tab present");
    Ok((key, tab))
}

/// Remove the pane from the grid and remember a terminal id. The PTY is
/// not signalled.
fn close_pane(
    book: &mut SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
    pane_id: &str,
) -> Result<()> {
    let content = pane_content(pane_id)?;
    let (_, tab) = require_tab(book, scene_id, tab_id)?;
    if !tab.grid.leaves().contains(&content) {
        return Err(Error::NotFound {
            entity: "pane",
            id: pane_id.to_string(),
        });
    }
    if let PaneContent::Terminal(id) = &content {
        if !tab.closed.contains(id) {
            if tab.closed.len() >= 256 {
                return Err(Error::Validation(
                    "closed-pane reference limit reached".into(),
                ));
            }
            tab.closed.push(id.clone());
        }
    }
    if tab.grid == PaneNode::leaf(content.clone()) {
        if matches!(content, PaneContent::Browser) {
            return Err(Error::Validation(
                "the last browser pane cannot be closed".into(),
            ));
        }
        tab.grid = PaneNode::leaf(PaneContent::Browser);
    } else if tab.grid.close(&content).is_none() {
        return Err(Error::Validation(format!(
            "pane `{pane_id}` cannot be closed"
        )));
    }
    if !tab.grid.leaves().contains(&tab.focus) {
        tab.focus = tab.grid.leaves()[0].clone();
    }
    Ok(())
}

fn focus_pane(
    book: &mut SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
    pane_id: &str,
) -> Result<()> {
    let content = pane_content(pane_id)?;
    let (scene_key, tab) = require_tab(book, scene_id, tab_id)?;
    if !tab.grid.leaves().contains(&content) {
        return Err(Error::NotFound {
            entity: "pane",
            id: pane_id.to_string(),
        });
    }
    let tab_key = tab.id.clone();
    tab.focus = content;
    scene_active_and_selected(book, &scene_key, &tab_key);
    Ok(())
}

fn zoom(
    book: &mut SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
    on: Option<bool>,
) -> Result<()> {
    let (_, tab) = require_tab(book, scene_id, tab_id)?;
    tab.zoom = on.unwrap_or(!tab.zoom);
    Ok(())
}

fn resize(
    book: &mut SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
    path: &[bool],
    ratio: u16,
) -> Result<()> {
    if !(10..=90).contains(&ratio) {
        return Err(Error::Validation(
            "pane ratio must be 10..=90 (first-side percent)".into(),
        ));
    }
    let (_, tab) = require_tab(book, scene_id, tab_id)?;
    if !tab.grid.set_ratio_at_path(path, ratio) {
        return Err(Error::Validation(
            "no split at that path in this tab".into(),
        ));
    }
    Ok(())
}

/// Scene, tab, and focused pane after an open. A browser leaf's pane id
/// is `browser`. Terminal panes use the terminal id.
pub fn focused_pane(
    book: &SceneBook,
    scene_id: Option<&str>,
    tab_id: Option<&str>,
) -> Option<(String, String, String)> {
    let scene_key = scene_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(book.selected.as_str());
    let scene = book.scenes.get(scene_key)?;
    let tab_key = tab_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(scene.active.as_str());
    let tab = scene.tabs.iter().find(|tab| tab.id == tab_key)?;
    let pane_id = match &tab.focus {
        PaneContent::Browser => "browser".to_string(),
        PaneContent::Terminal(id) => id.clone(),
    };
    Some((scene_key.to_string(), tab.id.clone(), pane_id))
}

/// Whether the tab is zoomed after a tree edit. Used for plugin `pane.opened`
/// events when zoom is toggled with `on: None`.
pub fn tab_is_zoomed(book: &SceneBook, scene_id: Option<&str>, tab_id: Option<&str>) -> bool {
    let scene_key = scene_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(book.selected.as_str());
    let Some(scene) = book.scenes.get(scene_key) else {
        return false;
    };
    let tab_key = tab_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(scene.active.as_str());
    scene
        .tabs
        .iter()
        .find(|tab| tab.id == tab_key)
        .is_some_and(|tab| tab.zoom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::WorkbenchTreeOp;

    fn scene_with_two_tabs() -> (SceneBook, String, String) {
        let mut book = SceneBook::default();
        apply(
            &mut book,
            &WorkbenchTreeOp::CreateScene {
                id: Some("alpha".into()),
                name: "Alpha".into(),
            },
        )
        .unwrap();
        apply(
            &mut book,
            &WorkbenchTreeOp::CreateTab {
                scene_id: Some("alpha".into()),
                name: "one".into(),
            },
        )
        .unwrap();
        let first = book.scenes["alpha"].active.clone();
        apply(
            &mut book,
            &WorkbenchTreeOp::Split {
                scene_id: Some("alpha".into()),
                tab_id: Some(first.clone()),
                pane_id: None,
                direction: "right".into(),
                terminal_id: Some("t0".into()),
            },
        )
        .unwrap();
        apply(
            &mut book,
            &WorkbenchTreeOp::Split {
                scene_id: Some("alpha".into()),
                tab_id: Some(first.clone()),
                pane_id: Some("t0".into()),
                direction: "right".into(),
                terminal_id: Some("ta".into()),
            },
        )
        .unwrap();
        apply(
            &mut book,
            &WorkbenchTreeOp::CreateTab {
                scene_id: Some("alpha".into()),
                name: "two".into(),
            },
        )
        .unwrap();
        let second = book.scenes["alpha"].active.clone();
        apply(
            &mut book,
            &WorkbenchTreeOp::Split {
                scene_id: Some("alpha".into()),
                tab_id: Some(second.clone()),
                pane_id: None,
                direction: "down".into(),
                terminal_id: Some("t1".into()),
            },
        )
        .unwrap();
        apply(
            &mut book,
            &WorkbenchTreeOp::Split {
                scene_id: Some("alpha".into()),
                tab_id: Some(second.clone()),
                pane_id: Some("t1".into()),
                direction: "down".into(),
                terminal_id: Some("tb".into()),
            },
        )
        .unwrap();
        apply(
            &mut book,
            &WorkbenchTreeOp::Resize {
                scene_id: Some("alpha".into()),
                tab_id: Some(second.clone()),
                path: vec![],
                ratio: 70,
            },
        )
        .unwrap();
        (book, first, second)
    }

    fn ratio(book: &SceneBook, tab_id: &str) -> u16 {
        let tab = book.scenes["alpha"]
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .unwrap();
        match &tab.grid {
            PaneNode::Split { ratio, .. } => *ratio,
            PaneNode::Leaf(_) => panic!("expected a split"),
        }
    }

    #[test]
    fn merge_keeps_other_tabs_and_last_writer_wins_on_the_viewed_tab() {
        let (server, first, second) = scene_with_two_tabs();
        assert_eq!(ratio(&server, &second), 70);

        let mut client = server.clone();
        client.selected = "alpha".into();
        client.scenes.get_mut("alpha").unwrap().active = first.clone();
        apply(
            &mut client,
            &WorkbenchTreeOp::Resize {
                scene_id: Some("alpha".into()),
                tab_id: Some(first.clone()),
                path: vec![],
                ratio: 30,
            },
        )
        .unwrap();
        // A stale copy of the other tab must not clobber the server.
        apply(
            &mut client,
            &WorkbenchTreeOp::Resize {
                scene_id: Some("alpha".into()),
                tab_id: Some(second.clone()),
                path: vec![],
                ratio: 40,
            },
        )
        .unwrap();
        // The client is looking at `first`, so put the stale ratio back on
        // `second` only in the payload the merge will see.
        let stale = {
            let mut payload = client.clone();
            let tab = payload
                .scenes
                .get_mut("alpha")
                .unwrap()
                .tabs
                .iter_mut()
                .find(|tab| tab.id == second)
                .unwrap();
            tab.grid.set_ratio_at_path(&[], 15);
            payload.scenes.get_mut("alpha").unwrap().active = first.clone();
            payload
        };

        let mut merged = server.clone();
        merge_client_book(&mut merged, &stale).unwrap();
        assert_eq!(
            ratio(&merged, &first),
            30,
            "viewed tab takes the later size"
        );
        assert_eq!(ratio(&merged, &second), 70, "the other tab is untouched");

        let mut later = stale.clone();
        later
            .scenes
            .get_mut("alpha")
            .unwrap()
            .tabs
            .iter_mut()
            .find(|tab| tab.id == first)
            .unwrap()
            .grid
            .set_ratio_at_path(&[], 55);
        merge_client_book(&mut merged, &later).unwrap();
        assert_eq!(
            ratio(&merged, &first),
            55,
            "a later size on the same tab wins"
        );
        assert_eq!(ratio(&merged, &second), 70);
    }

    #[test]
    fn close_pane_keeps_the_terminal_id_and_does_not_touch_the_neighbor() {
        let (mut book, first, second) = scene_with_two_tabs();
        apply(
            &mut book,
            &WorkbenchTreeOp::ClosePane {
                scene_id: Some("alpha".into()),
                tab_id: Some(first.clone()),
                pane_id: "t0".into(),
            },
        )
        .unwrap();
        let tab = book.scenes["alpha"]
            .tabs
            .iter()
            .find(|tab| tab.id == first)
            .unwrap();
        assert!(tab.closed.iter().any(|id| id == "t0"));
        assert!(
            !tab.grid
                .leaves()
                .contains(&PaneContent::Terminal("t0".into()))
        );
        assert!(book.locate("t0").unwrap().hidden);
        assert_eq!(ratio(&book, &second), 70);
        assert!(book.locate("t1").is_some());
    }

    #[test]
    fn focus_does_not_resize_and_a_hidden_tab_does_not_change_the_other() {
        let (mut book, first, second) = scene_with_two_tabs();
        apply(
            &mut book,
            &WorkbenchTreeOp::FocusPane {
                scene_id: Some("alpha".into()),
                tab_id: Some(second.clone()),
                pane_id: "t1".into(),
            },
        )
        .unwrap();
        assert_eq!(ratio(&book, &second), 70);
        apply(
            &mut book,
            &WorkbenchTreeOp::CloseTab {
                scene_id: Some("alpha".into()),
                tab_id: first.clone(),
            },
        )
        .unwrap();
        assert!(
            book.scenes["alpha"]
                .tabs
                .iter()
                .find(|tab| tab.id == first)
                .unwrap()
                .hidden
        );
        assert_eq!(ratio(&book, &second), 70);
    }

    #[test]
    fn legacy_layout_json_imports_and_old_books_still_parse() {
        let legacy = serde_json::json!({
            "grid": {"Split": {"axis": "Horizontal", "ratio": 40, "first": {"Leaf": {"Terminal": "t0"}}, "second": {"Leaf": {"Terminal": "dead"}}}},
            "focus": {"Terminal": "t0"},
            "sidebar": 31
        });
        let book = import_stored(&legacy, |id| {
            if id == "t0" {
                "wt-a".into()
            } else {
                FLOATING.into()
            }
        })
        .unwrap()
        .unwrap();
        assert!(book.valid());
        assert_eq!(book.sidebar, 31);
        assert!(book.legacy.is_some());
        assert_eq!(book.locate("t0").unwrap().scene_id, "wt-a");
        assert_eq!(book.locate("dead").unwrap().scene_id, FLOATING);
        let old = r#"{"version":2,"selected":"floating","scenes":{},"sidebar":26}"#;
        let parsed: SceneBook = serde_json::from_str(old).unwrap();
        assert!(parsed.valid());
        assert!(
            import_stored(&serde_json::json!({}), |_| FLOATING.into())
                .unwrap()
                .is_none()
        );
    }
}
