//! Pure conversion from a legacy Viva live-handoff manifest + scene book into
//! the portable plan the native host will hand to Herdr's adoption path.
//!
//! No process is touched here: building a plan reads the old host's manifest
//! and persisted layout only. The receiver still owns the PTY fds and must not
//! acknowledge the old host until every pane has been adopted.
use crate::foundation::error::{Error, Result};
use crate::server::HandoffManifest;
use crate::tui::layout::PaneContent;
use crate::tui::workbench::scenes::SceneBook;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedPane {
    pub terminal_id: String,
    pub pid: u32,
    pub cols: u16,
    pub rows: u16,
    /// Scrollback + the legacy formatted screen, both already ANSI.
    pub history_ansi: String,
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedTab {
    pub name: String,
    pub panes: Vec<ImportedPane>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedScene {
    pub name: String,
    pub tabs: Vec<ImportedTab>,
    pub active_tab: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyImportPlan {
    pub scenes: Vec<ImportedScene>,
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let raw = text.as_bytes();
    for pair in raw.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        bytes.push(((high << 4) | low) as u8);
    }
    Some(bytes)
}

fn history_ansi(entry: &crate::server::HandoffEntry) -> String {
    let mut text = String::new();
    if !entry.history.is_empty() {
        text.push_str(&entry.history.join("\r\n"));
        text.push_str("\r\n");
    }
    if let Some(bytes) = hex_decode(&entry.screen_hex) {
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    text
}

fn pane(entry: &crate::server::HandoffEntry) -> ImportedPane {
    ImportedPane {
        terminal_id: entry.terminal_id.clone(),
        pid: entry.pid,
        cols: entry.cols,
        rows: entry.rows,
        history_ansi: history_ansi(entry),
        purpose: entry.purpose.clone(),
    }
}

/// Convert a manifest plus the persisted legacy scene book. Panes the book
/// does not place are preserved in a trailing "restored" tab, so a stale or
/// partial layout can never drop a live terminal.
pub(crate) fn build_plan(
    manifest: &HandoffManifest,
    layout_json: Option<&str>,
) -> Result<LegacyImportPlan> {
    if manifest.entries.is_empty() {
        return Ok(LegacyImportPlan { scenes: Vec::new() });
    }
    let by_terminal: BTreeMap<&str, &crate::server::HandoffEntry> = manifest
        .entries
        .iter()
        .map(|entry| (entry.terminal_id.as_str(), entry))
        .collect();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut scenes = Vec::new();

    if let Some(raw) = layout_json {
        let book: SceneBook = serde_json::from_str(raw)
            .map_err(|err| Error::Validation(format!("legacy scene book is invalid: {err}")))?;
        for (scene_id, scene) in &book.scenes {
            let mut tabs = Vec::new();
            for tab in &scene.tabs {
                if tab.hidden {
                    continue;
                }
                let mut panes = Vec::new();
                for leaf in tab.grid.leaves() {
                    let PaneContent::Terminal(terminal_id) = leaf else {
                        continue;
                    };
                    let Some(entry) = by_terminal.get(terminal_id.as_str()) else {
                        continue;
                    };
                    used.insert(terminal_id.clone());
                    panes.push(pane(entry));
                }
                if !panes.is_empty() {
                    tabs.push(ImportedTab {
                        name: if tab.name.trim().is_empty() {
                            format!("tab {}", tabs.len() + 1)
                        } else {
                            tab.name.clone()
                        },
                        panes,
                    });
                }
            }
            if tabs.is_empty() {
                continue;
            }
            let active_tab = scene
                .tabs
                .iter()
                .position(|tab| tab.id == scene.active)
                .map(|index| index.min(tabs.len().saturating_sub(1)))
                .unwrap_or(0);
            scenes.push(ImportedScene {
                name: if scene.name.trim().is_empty() {
                    scene_id.clone()
                } else {
                    scene.name.clone()
                },
                tabs,
                active_tab,
            });
        }
    }

    let orphaned: Vec<_> = manifest
        .entries
        .iter()
        .filter(|entry| !used.contains(&entry.terminal_id))
        .map(pane)
        .collect();
    if !orphaned.is_empty() {
        let tab = ImportedTab {
            name: "restored".into(),
            panes: orphaned,
        };
        match scenes.last_mut() {
            Some(scene) => {
                // Keep the user's original active tab; the restored tab is
                // reachable but does not steal focus during a takeover.
                scene.tabs.push(tab);
            }
            None => scenes.push(ImportedScene {
                name: "restored".into(),
                tabs: vec![tab],
                active_tab: 0,
            }),
        }
    }
    Ok(LegacyImportPlan { scenes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{HandoffEntry, HandoffManifest};

    fn entry(id: &str, pid: u32, history: &[&str], screen: &str) -> HandoffEntry {
        HandoffEntry {
            terminal_id: id.into(),
            owner: "user_shell".into(),
            worktree_id: None,
            purpose: format!("shell {id}"),
            pid,
            pid_start_marker: "marker".into(),
            cols: 80,
            rows: 24,
            history: history.iter().map(|line| line.to_string()).collect(),
            screen_hex: screen.into(),
            pending_hex: String::new(),
        }
    }

    #[test]
    fn scene_book_places_live_terminals_and_orphans_are_preserved() {
        let manifest = HandoffManifest {
            protocol: 2,
            host_id: "old-host".into(),
            entries: vec![
                entry("term-a", 10, &["old line"], "4142"),
                entry("term-b", 11, &[], "43"),
                entry("term-c", 12, &[], ""),
            ],
        };
        let layout = serde_json::json!({
            "version": 2,
            "selected": "scene-1",
            "sidebar": 26,
            "scenes": {
                "scene-1": {
                    "name": "repo",
                    "active": "tab-2",
                    "tabs": [
                        {
                            "id": "tab-1",
                            "name": "one",
                            "grid": {"Leaf": {"Terminal": "term-a"}},
                            "focus": {"Terminal": "term-a"},
                            "zoom": false
                        },
                        {
                            "id": "tab-2",
                            "name": "two",
                            "grid": {
                                "Split": {
                                    "axis": "Horizontal",
                                    "first": {"Leaf": {"Terminal": "term-b"}},
                                    "second": {"Leaf": "Browser"},
                                    "ratio": 50
                                }
                            },
                            "focus": {"Terminal": "term-b"},
                            "zoom": false
                        }
                    ]
                }
            }
        })
        .to_string();

        let plan = build_plan(&manifest, Some(&layout)).unwrap();
        assert_eq!(plan.scenes.len(), 1);
        assert_eq!(plan.scenes[0].name, "repo");
        assert_eq!(plan.scenes[0].tabs.len(), 3, "two book tabs + restored");
        assert_eq!(plan.scenes[0].tabs[0].panes[0].terminal_id, "term-a");
        assert!(
            plan.scenes[0].tabs[0].panes[0]
                .history_ansi
                .contains("old line")
        );
        assert!(plan.scenes[0].tabs[0].panes[0].history_ansi.contains("AB"));
        assert_eq!(plan.scenes[0].tabs[1].panes[0].terminal_id, "term-b");
        assert_eq!(plan.scenes[0].tabs[2].name, "restored");
        assert_eq!(plan.scenes[0].tabs[2].panes[0].terminal_id, "term-c");
        assert_eq!(plan.scenes[0].active_tab, 1);
    }

    #[test]
    fn missing_or_invalid_layout_keeps_every_terminal() {
        for layout in [None, Some("{not-json")] {
            let manifest = HandoffManifest {
                protocol: 2,
                host_id: "old-host".into(),
                entries: vec![entry("term-a", 1, &[], ""), entry("term-b", 2, &[], "")],
            };
            // Invalid JSON is an honest conversion error, not a silent drop.
            if layout == Some("{not-json") {
                assert!(build_plan(&manifest, layout).is_err());
                continue;
            }
            let plan = build_plan(&manifest, layout).unwrap();
            assert_eq!(plan.scenes.len(), 1);
            assert_eq!(plan.scenes[0].tabs.len(), 1);
            assert_eq!(plan.scenes[0].tabs[0].panes.len(), 2);
        }
    }
}
