//! Persistent views over existing server terminals; never a process owner.
use crate::tui::layout::{MAX_PANES, PaneContent, PaneNode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FLOATING: &str = "floating";
pub const MAX_TABS: usize = 32;
pub const MAX_SCENES: usize = 128;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabView {
    pub id: String,
    pub name: String,
    pub grid: PaneNode,
    pub focus: PaneContent,
    pub zoom: bool,
    #[serde(default)]
    pub hidden: bool,
    /// Closed leaves remain navigable. They do not render or stop their sessions.
    #[serde(default)]
    pub closed: Vec<String>,
}
impl TabView {
    pub fn new(name: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            grid: PaneNode::leaf(PaneContent::Browser),
            focus: PaneContent::Browser,
            zoom: false,
            hidden: false,
            closed: vec![],
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Scene {
    pub active: String,
    pub tabs: Vec<TabView>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneBook {
    pub version: u32,
    pub selected: String,
    pub scenes: BTreeMap<String, Scene>,
    pub sidebar: u16,
    /// Unchanged legacy payload for provenance and downgrade/manual rollback.
    #[serde(default)]
    pub legacy: Option<serde_json::Value>,
}
impl Default for SceneBook {
    fn default() -> Self {
        Self {
            version: 2,
            selected: FLOATING.into(),
            scenes: BTreeMap::new(),
            sidebar: 26,
            legacy: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalLocation {
    pub scene_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub hidden: bool,
}
impl SceneBook {
    pub fn locate(&self, id: &str) -> Option<TerminalLocation> {
        for (scene_id, s) in &self.scenes {
            for tab in &s.tabs {
                let placed = tab
                    .grid
                    .leaves()
                    .contains(&PaneContent::Terminal(id.into()));
                if placed || tab.closed.iter().any(|c| c == id) {
                    return Some(TerminalLocation {
                        scene_id: scene_id.clone(),
                        tab_id: tab.id.clone(),
                        pane_id: id.into(),
                        hidden: tab.hidden || !placed,
                    });
                }
            }
        }
        None
    }
    pub fn valid(&self) -> bool {
        fn node(n: &PaneNode, depth: usize) -> bool {
            if depth > MAX_PANES {
                return false;
            }
            match n {
                PaneNode::Leaf(_) => true,
                PaneNode::Split {
                    first,
                    second,
                    ratio,
                    ..
                } => (10..=90).contains(ratio) && node(first, depth + 1) && node(second, depth + 1),
            }
        }
        let mut ids = std::collections::HashSet::new();
        let mut terminals = std::collections::HashSet::new();
        self.version == 2
            && self.scenes.len() <= MAX_SCENES
            && self.scenes.values().all(|s| {
                s.tabs.len() <= MAX_TABS
                    && s.tabs.iter().all(|t| {
                        ids.insert(t.id.clone())
                            && t.name.len() <= 256
                            && node(&t.grid, 0)
                            && t.grid.leaves().len() <= MAX_PANES
                            && t.closed.len() <= 256
                            && t.grid.leaves().iter().all(|c| match c {
                                PaneContent::Terminal(id) => terminals.insert(id.clone()),
                                _ => true,
                            })
                    })
            })
    }
}
