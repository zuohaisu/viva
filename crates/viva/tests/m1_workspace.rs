use serde_json::json;
use viva::foundation::store::Store;
use viva::workspaces::navigation::{Navigation, parse_jsonc};

#[test]
fn jsonc_preserves_string_delimiters_and_rejects_broken_comments() {
    let v =
        parse_jsonc("{\n// comment\n\"url\":\"https://a/*b*/\",\"folders\":[{\"path\":\"a\",},],}")
            .unwrap();
    assert_eq!(v["url"], "https://a/*b*/");
    assert!(parse_jsonc("{/* no end").is_err());
}
#[test]
fn workspace_roundtrip_retains_unavailable_folders_and_other_fields() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory(viva::office::office_migrations()).unwrap();
    let nav = Navigation { store: &store };
    let input = dir.path().join("example.code-workspace");
    let original = "{ /*keep semantics*/ \"folders\":[{\"path\":\"missing\",\"name\":\"Absent\"},{\"uri\":\"vscode-remote://host/repo\"}],\"settings\":{\"url\":\"https://example.com\"},\"tasks\":{\"command\":\"never execute\"}}";
    std::fs::write(&input, original).unwrap();
    let id = nav.open(&input).unwrap();
    assert_eq!(nav.open(&input).unwrap(), id);
    let rows = nav.folders(&id).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.diagnostic.is_some()));
    let save = dir.path().join("saved.code-workspace");
    nav.save_as(&id, &save).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(save).unwrap()).unwrap();
    assert_eq!(v["settings"]["url"], "https://example.com");
    assert_eq!(v["folders"][0]["name"], "Absent");
    assert_eq!(std::fs::read_to_string(input).unwrap(), original);
}
#[test]
fn linked_worktree_and_compositions_share_project_without_task_or_grant() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=test",
            "-c",
            "user.email=t@t",
            "commit",
            "--allow-empty",
            "-m",
            "seed",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let linked = dir.path().join("linked");
    assert!(
        std::process::Command::new("git")
            .current_dir(&repo)
            .args(["worktree", "add", "-b", "test", linked.to_str().unwrap()])
            .output()
            .unwrap()
            .status
            .success()
    );
    let store = Store::open_in_memory(viva::office::office_migrations()).unwrap();
    let nav = Navigation { store: &store };
    let a = nav.open(&repo).unwrap();
    let b = nav.open(&linked).unwrap();
    assert_eq!(
        nav.folders(&a).unwrap()[0].project_id,
        nav.folders(&b).unwrap()[0].project_id
    );
    nav.add(&a, json!({"path":linked}), dir.path()).unwrap();
    assert_eq!(nav.folders(&a).unwrap().len(), 1);
    let pid = nav.folders(&a).unwrap()[0].project_id.clone().unwrap();
    nav.remove(&a, &pid).unwrap();
    assert!(nav.folders(&a).unwrap().is_empty());
    assert_eq!(
        viva::projects::ProjectRegistry::new(&store)
            .list(false)
            .unwrap()
            .len(),
        1
    );
    let count: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM task_worktrees", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    nav.select(&b).unwrap();
    nav.select(&a).unwrap();
    assert_eq!(nav.selected().unwrap(), Some(a));
}
