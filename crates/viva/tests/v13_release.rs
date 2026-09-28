//! V13 acceptance tests (issue #22): the shipped binary as the product —
//! clean-environment startup, read-only data export for asset preservation,
//! and honest overwrite refusal. Install/release packaging is exercised in
//! CI (`release.yml`); platform-specific PASS claims stay there.

use std::process::Command;
use tempfile::TempDir;

fn viva(home: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .output()
        .expect("viva runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn clean_home_boot_and_data_export_roundtrip() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().join("home");

    // A clean environment boots: init applies the full office schema,
    // doctor reports state, and the process exits 0.
    let (code, _, err) = viva(&home, &["init"]);
    assert_eq!(code, 0, "init failed: {err}");
    let (code, out, err) = viva(&home, &["doctor"]);
    assert_eq!(code, 0, "doctor failed: {err}");
    assert!(out.contains("state:     openable"), "doctor output: {out}");

    // Produce one fact, then export everything read-only.
    let (code, _, err) = viva(
        &home,
        &[
            "event",
            "add",
            "foundation",
            "v13_smoke",
            "tool",
            "viva",
            "{\"s\":1}",
        ],
    );
    assert_eq!(code, 0, "event add failed: {err}");

    let export = dir.path().join("export");
    let (code, _out, err) = viva(
        &home,
        &["data", "export", "--out", export.to_str().unwrap()],
    );
    assert_eq!(code, 0, "export failed: {err}");

    // The manifest inventories the tables; the fact we wrote is in the file.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(export.join("manifest.json")).expect("manifest"),
    )
    .expect("manifest json");
    assert_eq!(manifest["read_only"], serde_json::Value::Bool(true));
    let tables = manifest["tables"].as_object().expect("table map");
    assert!(
        tables.contains_key("office_events") && tables.contains_key("members"),
        "core tables inventoried: {:?}",
        tables.keys().collect::<Vec<_>>()
    );
    let events: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(export.join("office_events.json")).expect("events file"),
    )
    .expect("events json");
    assert_eq!(
        events.as_array().expect("array").len(),
        1,
        "the exported fact is present"
    );

    // The export must not have mutated the store: doctor still opens it and
    // the event is still exactly once.
    let (code, out, _) = viva(&home, &["event", "list", "10"]);
    assert_eq!(code, 0);
    assert_eq!(out.matches("v13_smoke").count(), 1);
}

#[test]
fn export_refuses_overwrite_and_never_writes_without_a_store() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().join("home");
    viva(&home, &["init"]);

    let export = dir.path().join("export");
    viva(
        &home,
        &["data", "export", "--out", export.to_str().unwrap()],
    );
    let (code, _, err) = viva(
        &home,
        &["data", "export", "--out", export.to_str().unwrap()],
    );
    assert_ne!(code, 0, "second export must refuse");
    assert!(err.contains("refusing to overwrite"), "got: {err}");

    // A missing home is an honest failure, not a fresh boot.
    let lonely = dir.path().join("lonely-home");
    let (code, _stdout, _err) = viva(
        &lonely,
        &[
            "data",
            "export",
            "--out",
            dir.path().join("out2").to_str().unwrap(),
        ],
    );
    assert_ne!(code, 0, "export without a store must fail");
    assert!(
        !lonely.exists()
            || lonely
                .read_dir()
                .map(|mut d| d.next().is_none())
                .unwrap_or(true)
    );
}
