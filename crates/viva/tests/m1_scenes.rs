use viva::tui::layout::{PaneContent, SplitAxis};
use viva::tui::workbench::{TerminalRow, WorkbenchApp, WorkbenchModel};
fn app() -> WorkbenchApp {
    let mut a = WorkbenchApp::new();
    a.set_model(WorkbenchModel {
        terminals: (0..4)
            .map(|i| TerminalRow {
                terminal_id: format!("t{i}"),
                worktree_id: Some(if i < 2 { "a" } else { "b" }.into()),
                purpose: "shell".into(),
                owner_label: "user_shell".into(),
                live: Some(true),
                agent_status: vec![],
            })
            .collect(),
        ..Default::default()
    });
    a
}
#[test]
fn scenes_restore_tabs_focus_zoom_and_repeated_selection_never_duplicates() {
    let mut a = app();
    a.select_worktree("a".into());
    a.new_tab("build".into());
    a.attach_terminal("t0".into());
    a.split_pane(SplitAxis::Vertical, "t1".into());
    a.toggle_zoom();
    let loc = a.terminal_location("t1").unwrap();
    a.new_tab("tests".into());
    a.select_worktree("b".into());
    a.new_tab("agent".into());
    a.attach_terminal("t2".into());
    a.new_tab("shell".into());
    a.attach_terminal("t3".into());
    a.attach_terminal("t1".into());
    assert_eq!(a.selected_scene(), "a");
    assert_eq!(a.terminal_location("t1").unwrap(), loc);
    assert!(a.zoomed());
    assert_eq!(a.terminal_leaves(), vec!["t0", "t1"]);
    let before = a.serialize_layout();
    a.attach_terminal("t1".into());
    assert_eq!(a.serialize_layout(), before);
    let mut restored = app();
    restored.restore_layout(&before, &Default::default());
    assert_eq!(restored.terminal_location("t1").unwrap(), loc);
    assert_eq!(restored.pane_focus(), &PaneContent::Terminal("t1".into()));
    assert!(restored.zoomed());
}
#[test]
fn close_view_and_relocate_preserves_neighbors_and_ids() {
    let mut a = app();
    a.attach_terminal("t0".into());
    a.split_pane(SplitAxis::Horizontal, "t1".into());
    let old = a.terminal_location("t1").unwrap();
    a.close_pane();
    assert_eq!(a.terminal_leaves(), vec!["t0"]);
    assert!(a.terminal_location("t1").unwrap().hidden);
    a.attach_terminal("t1".into());
    assert_eq!(a.terminal_location("t1").unwrap().tab_id, old.tab_id);
    assert_eq!(a.terminal_leaves(), vec!["t0", "t1"]);
    a.close_tab();
    assert!(a.terminal_location("t0").unwrap().hidden);
    a.attach_terminal("t0".into());
    assert_eq!(a.terminal_leaves(), vec!["t0", "t1"]);
}
#[test]
fn legacy_and_dead_references_are_retained_without_replay() {
    let mut a = app();
    let old=serde_json::json!({"grid":{"Split":{"axis":"Horizontal","first":{"Leaf":{"Terminal":"t0"}},"second":{"Leaf":{"Terminal":"dead"}},"ratio":50}},"focus":{"Terminal":"t0"}}).to_string();
    a.restore_layout(&old, &Default::default());
    assert!(a.terminal_location("t0").is_some());
    assert!(a.terminal_location("dead").is_some());
    let saved: serde_json::Value = serde_json::from_str(&a.serialize_layout()).unwrap();
    assert_eq!(
        saved["legacy"],
        serde_json::from_str::<serde_json::Value>(&old).unwrap()
    );
    let before = a.serialize_layout();
    a.restore_layout("garbage", &Default::default());
    assert_eq!(a.serialize_layout(), before);
}
