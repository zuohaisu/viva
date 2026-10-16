//! Issue #140: skill body versioning, rollback, and projection reads.

use std::path::Path;
use std::process::Command;
use std::str::FromStr as _;

use tempfile::TempDir;

use viva::foundation::ids::ExecutionId;
use viva::foundation::store::{
    DOMAIN_FOUNDATION, DOMAIN_SKILLS, FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::skills::{SkillBodySource, SkillBodyStore, SkillBodyWriteKind};

fn frozen() -> viva::foundation::store::FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(DOMAIN_SKILLS, 1, "skills v1", viva::skills::SKILLS_V1_SQL)
        .freeze()
        .expect("registry")
}

fn store() -> Store {
    let dir = tempfile::TempDir::new().expect("dir");
    let db = dir.path().join("viva.db");
    let store = Store::open(&db, &frozen()).expect("store");
    std::mem::forget(dir);
    store
}

fn write_body(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write body");
    path
}

/// Acceptance: bad write → rollback → current matches prior version; bad version still readable.
#[test]
fn rollback_restores_prior_body_and_keeps_history() {
    let store = store();
    let skills = SkillBodyStore::new(&store);
    let exec = ExecutionId::new();
    let created = skills
        .create(
            "deploy checklist",
            "# deploy\nstep one",
            SkillBodySource::Execution {
                execution_id: exec.clone(),
            },
        )
        .expect("create");
    let skill_id = created.skill.skill_id.clone();

    skills
        .write_body(
            &skill_id,
            "# deploy\nstep one\nWRONG SENTENCE",
            SkillBodyWriteKind::Modify,
            SkillBodySource::Execution {
                execution_id: exec.clone(),
            },
        )
        .expect("bad write");

    let rolled = skills.rollback(&skill_id, 1).expect("rollback");
    assert_eq!(rolled.version.body, "# deploy\nstep one");
    assert_eq!(rolled.skill.current_version, 3);

    let current = skills
        .current_body(&skill_id)
        .expect("current")
        .expect("some");
    assert_eq!(current.version.body, "# deploy\nstep one");

    let bad = skills.version_body(&skill_id, 2).expect("v2").expect("row");
    assert!(bad.body.contains("WRONG SENTENCE"));
}

/// Acceptance: projection file and `get` both read the post-rollback current body.
#[test]
fn projection_and_get_read_the_restored_current_version() {
    let store = store();
    let skills = SkillBodyStore::new(&store);
    let dir = tempfile::TempDir::new().expect("dir");
    let exec = ExecutionId::new();
    let created = skills
        .create(
            "lint skill",
            "good body",
            SkillBodySource::Execution {
                execution_id: exec.clone(),
            },
        )
        .expect("create");
    let skill_id = created.skill.skill_id;
    skills
        .write_body(
            &skill_id,
            "bad body",
            SkillBodyWriteKind::Modify,
            SkillBodySource::Execution { execution_id: exec },
        )
        .expect("bad");
    skills.rollback(&skill_id, 1).expect("rollback");

    let got = skills.current_body(&skill_id).expect("get").expect("some");
    assert_eq!(got.version.body, "good body");

    let dest = dir.path().join("SKILL.md");
    skills
        .write_current_projection_file(&skill_id, &dest)
        .expect("project");
    let on_disk = std::fs::read_to_string(&dest).expect("read projection");
    assert_eq!(on_disk, "good body");
}

/// Acceptance: manual edits are labeled manual; execution writes carry the execution id.
#[test]
fn manual_and_execution_sources_are_recorded_honestly() {
    let store = store();
    let skills = SkillBodyStore::new(&store);
    let created = skills
        .create("x", "v1", SkillBodySource::HaisuManual)
        .expect("create");
    let skill_id = created.skill.skill_id;

    assert!(ExecutionId::from_str("not-an-exec-id").is_err());

    let manual_row = skills
        .write_body(
            &skill_id,
            "manual v2",
            SkillBodyWriteKind::Modify,
            SkillBodySource::HaisuManual,
        )
        .expect("manual ok");
    assert_eq!(manual_row.version.operator, "haisu:manual");
    assert!(manual_row.version.execution_id.is_none());

    let dir = tempfile::TempDir::new().expect("dir");
    let body_path = write_body(dir.path(), "body.md", "from exec");
    let exec = ExecutionId::new();
    let exec_row = skills
        .write_body(
            &skill_id,
            std::fs::read_to_string(&body_path).expect("read"),
            SkillBodyWriteKind::Modify,
            SkillBodySource::Execution {
                execution_id: exec.clone(),
            },
        )
        .expect("exec write");
    assert_eq!(
        exec_row.version.execution_id.as_ref().map(|e| e.as_str()),
        Some(exec.as_str())
    );
    assert_ne!(exec_row.version.operator, "haisu:manual");
}

fn run_viva(home: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .output()
        .expect("spawn viva");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Real `viva` binary: rollback subcommand and rejection of writes without source.
#[test]
fn cli_skill_rollback_and_source_flags() {
    let home = TempDir::new().expect("home");
    let (ok, _, stderr) = run_viva(home.path(), &["init"]);
    assert!(ok, "init failed: {stderr}");

    let body_v1 = home.path().join("v1.md");
    std::fs::write(&body_v1, "version one").expect("v1");
    let exec = ExecutionId::new();
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "create",
            "--name",
            "cli skill",
            "--body-file",
            body_v1.to_str().expect("utf8"),
            "--execution",
            exec.as_str(),
        ],
    );
    assert!(ok, "create failed: {stderr}\n{stdout}");
    let created: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let skill_id = created["skill"]["skill_id"]
        .as_str()
        .expect("skill_id")
        .to_string();

    let (ok, _, stderr) = run_viva(
        home.path(),
        &["skill", "write", "--skill", &skill_id, "--body", "bad line"],
    );
    assert!(!ok, "write without source must fail");
    assert!(
        stderr.contains("execution") || stderr.contains("manual"),
        "{stderr}"
    );

    let body_v2 = home.path().join("v2.md");
    std::fs::write(&body_v2, "bad line").expect("v2");
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "skill",
            "write",
            "--skill",
            &skill_id,
            "--body-file",
            body_v2.to_str().expect("utf8"),
            "--execution",
            exec.as_str(),
        ],
    );
    assert!(ok, "write failed: {stderr}");

    let (ok, stdout, stderr) = run_viva(home.path(), &["skill", "rollback", &skill_id, "1"]);
    assert!(ok, "rollback failed: {stderr}");
    let rolled: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(rolled["version"]["body"].as_str(), Some("version one"));

    let (ok, stdout, stderr) = run_viva(home.path(), &["skill", "get", "--skill", &skill_id]);
    assert!(ok, "get failed: {stderr}");
    let current: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(current["version"]["body"].as_str(), Some("version one"));
}
