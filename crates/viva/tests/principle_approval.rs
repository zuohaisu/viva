//! Issue #120: owner approval, dedup, expiry, and rollback of member principles.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use viva::foundation::ids::{ExecutionId, MemberId};
use viva::foundation::store::{
    DOMAIN_FOUNDATION, DOMAIN_KNOWLEDGE, DOMAIN_PRINCIPLES, FOUNDATION_V1_SQL, MigrationRegistry,
    Store,
};
use viva::knowledge::KNOWLEDGE_V1_SQL;
use viva::principles::{
    ExperienceExcerpt, PRINCIPLES_V1_SQL, PrincipleActor, PrincipleStore, PrincipleWriteKind,
};

fn open_store() -> Store {
    let frozen = MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(DOMAIN_KNOWLEDGE, 1, "knowledge v1", KNOWLEDGE_V1_SQL)
        .register(DOMAIN_PRINCIPLES, 1, "principles v1", PRINCIPLES_V1_SQL)
        .freeze()
        .expect("registry");
    Store::open_in_memory(&frozen).expect("store")
}

fn excerpt(execution: &ExecutionId, statement: &str) -> ExperienceExcerpt {
    ExperienceExcerpt {
        execution_id: execution.clone(),
        statement: statement.to_string(),
    }
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

fn viva_ok(home: &Path, args: &[&str]) -> serde_json::Value {
    let (ok, stdout, stderr) = run_viva(home, args);
    assert!(ok, "viva {args:?} failed: {stderr}\n{stdout}");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|err| panic!("viva {args:?} did not return JSON ({err}): {stdout}"))
}

fn init_home() -> TempDir {
    let home = TempDir::new().expect("home");
    let (ok, stdout, stderr) = run_viva(home.path(), &["init"]);
    assert!(ok, "init failed: {stderr}\n{stdout}");
    home
}

#[test]
fn principles_stay_out_of_knowledge_and_self_model() {
    let store = open_store();
    let principles = PrincipleStore::new(&store);
    let member = MemberId::new();
    let exec_a = ExecutionId::new();
    let report = principles
        .digest(
            &member,
            &[excerpt(&exec_a, "Ask which repository before editing.")],
            "2026-01-01T00:00:00Z",
            &PrincipleActor::Owner,
        )
        .expect("digest");
    let candidate = &report.proposed[0];
    principles
        .approve(&candidate.candidate_id, None, &PrincipleActor::Owner)
        .expect("approve");

    let knowledge: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM knowledge_items", [], |row| row.get(0))
        .expect("count knowledge");
    let stored: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM principles", [], |row| row.get(0))
        .expect("count principles");
    let self_model: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE '%self_model%'",
            [],
            |row| row.get(0),
        )
        .expect("schema");
    assert_eq!(knowledge, 0, "approval must not write a knowledge item");
    assert_eq!(stored, 1);
    assert_eq!(
        self_model, 0,
        "principles are not self_model_candidate rows"
    );
}

#[test]
fn a_candidate_is_still_pending_after_twenty_seven_days() {
    let store = open_store();
    let principles = PrincipleStore::new(&store);
    let member = MemberId::new();
    let exec_a = ExecutionId::new();
    principles
        .digest(
            &member,
            &[excerpt(&exec_a, "Keep the diff limited to the task.")],
            "2026-01-01T00:00:00Z",
            &PrincipleActor::Owner,
        )
        .expect("digest");
    let later = principles
        .digest(&member, &[], "2026-01-28T00:00:00Z", &PrincipleActor::Owner)
        .expect("day 27");
    assert!(later.expired.is_empty());
    assert!(later.proposed.is_empty());
    let approved = principles.approved_current(&member).expect("approved");
    assert!(approved.is_empty());
}

#[test]
fn cli_projections_omit_unapproved_text_and_keep_repo_instruction_files() {
    let home = init_home();
    let member = MemberId::new();
    let exec_a = ExecutionId::new();
    let exec_b = ExecutionId::new();
    let statement = "Ask which repository before editing.";
    let digest = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--statement",
            statement,
            "--execution",
            exec_a.as_str(),
            "--execution",
            exec_b.as_str(),
        ],
    );
    let candidate = digest["proposed"][0]["candidate_id"]
        .as_str()
        .expect("candidate")
        .to_string();
    assert_eq!(digest["proposed"].as_array().expect("proposed").len(), 1);

    let out = home.path().join("projection");
    std::fs::create_dir_all(&out).expect("out");
    std::fs::write(out.join("AGENTS.md"), b"agents-sentinel").expect("agents");
    std::fs::write(out.join("CLAUDE.md"), b"claude-sentinel").expect("claude");
    viva_ok(
        home.path(),
        &[
            "principle",
            "project",
            "--member",
            member.as_str(),
            "--out",
            out.to_str().expect("utf8"),
        ],
    );
    let pi = std::fs::read_to_string(out.join("pi/principles.md")).expect("pi");
    let claude = std::fs::read_to_string(out.join("claude/principles.md")).expect("claude");
    assert!(
        !pi.contains(statement),
        "unapproved text leaked into Pi: {pi}"
    );
    assert!(
        !claude.contains(statement),
        "unapproved text leaked into Claude: {claude}"
    );
    assert_eq!(
        std::fs::read(out.join("AGENTS.md")).expect("agents"),
        b"agents-sentinel"
    );
    assert_eq!(
        std::fs::read(out.join("CLAUDE.md")).expect("claude"),
        b"claude-sentinel"
    );

    viva_ok(
        home.path(),
        &["principle", "approve", "--candidate", &candidate],
    );
    viva_ok(
        home.path(),
        &[
            "principle",
            "project",
            "--member",
            member.as_str(),
            "--out",
            out.to_str().expect("utf8"),
        ],
    );
    let pi = std::fs::read_to_string(out.join("pi/principles.md")).expect("pi");
    let claude = std::fs::read_to_string(out.join("claude/principles.md")).expect("claude");
    assert!(pi.contains(statement), "{pi}");
    assert!(claude.contains(statement), "{claude}");
    assert_eq!(
        pi.split_once('\n').expect("line").1,
        claude.split_once('\n').expect("line").1,
        "both harnesses read the same approved body"
    );
    assert!(pi.contains(exec_a.as_str()) && pi.contains(exec_b.as_str()));
    assert_eq!(
        std::fs::read(out.join("AGENTS.md")).expect("agents"),
        b"agents-sentinel"
    );
    assert_eq!(
        std::fs::read(out.join("CLAUDE.md")).expect("claude"),
        b"claude-sentinel"
    );
}

#[test]
fn cli_non_owner_review_is_refused_and_recorded() {
    let home = init_home();
    let member = MemberId::new();
    let intruder = MemberId::new();
    let exec_a = ExecutionId::new();
    let digest = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--statement",
            "State the assumption before changing it.",
            "--execution",
            exec_a.as_str(),
        ],
    );
    let candidate = digest["proposed"][0]["candidate_id"]
        .as_str()
        .expect("candidate")
        .to_string();
    for args in [
        vec!["principle", "approve", "--candidate", candidate.as_str()],
        vec![
            "principle",
            "reject",
            "--candidate",
            candidate.as_str(),
            "--reason",
            "not mine",
        ],
        vec![
            "principle",
            "edit",
            "--candidate",
            candidate.as_str(),
            "--body",
            "rewritten by a member",
        ],
    ] {
        let mut full = args;
        full.push("--as-member");
        full.push(intruder.as_str());
        let (ok, stdout, stderr) = run_viva(home.path(), &full);
        assert!(!ok, "non-owner command succeeded: {stdout} {stderr}");
        assert!(
            stderr.contains("only the owner"),
            "refusal should name the owner: {stderr}"
        );
    }

    let approved = viva_ok(
        home.path(),
        &["principle", "approve", "--candidate", &candidate],
    );
    let principle = approved["principle_id"]
        .as_str()
        .expect("principle")
        .to_string();
    for args in [
        vec![
            "principle",
            "edit",
            "--principle",
            principle.as_str(),
            "--body",
            "member edit",
        ],
        vec![
            "principle",
            "rollback",
            member.as_str(),
            principle.as_str(),
            "1",
        ],
    ] {
        let mut full = args;
        full.push("--as-member");
        full.push(intruder.as_str());
        let (ok, stdout, stderr) = run_viva(home.path(), &full);
        assert!(!ok, "non-owner command succeeded: {stdout} {stderr}");
        assert!(stderr.contains("only the owner"), "{stderr}");
    }

    let audit = viva_ok(
        home.path(),
        &["principle", "audit", "--member", member.as_str()],
    );
    let rows = audit.as_array().expect("audit");
    for detail in ["approve", "reject", "edit", "rollback"] {
        let denied = rows.iter().any(|row| {
            row["action"] == "denied"
                && row["detail"] == detail
                && row["actor"] == format!("member:{}", intruder.as_str())
                && row["source_execution_ids"]
                    .as_array()
                    .expect("sources")
                    .iter()
                    .any(|id| id == exec_a.as_str())
        });
        assert!(denied, "missing denied {detail} record: {audit}");
    }
}

#[test]
fn cli_rejection_suppresses_the_same_key_and_a_new_statement_names_it() {
    let home = init_home();
    let member = MemberId::new();
    let exec_a = ExecutionId::new();
    let exec_b = ExecutionId::new();
    let digest = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--statement",
            "Ship without a recorded execution.",
            "--execution",
            exec_a.as_str(),
            "--execution",
            exec_b.as_str(),
        ],
    );
    let candidate = digest["proposed"][0]["candidate_id"]
        .as_str()
        .expect("candidate")
        .to_string();
    let rejected = viva_ok(
        home.path(),
        &[
            "principle",
            "reject",
            "--candidate",
            &candidate,
            "--reason",
            "the executions do not support that rule",
        ],
    );
    assert_eq!(rejected["status"], "rejected");
    assert_eq!(
        rejected["rejection_reason"],
        "the executions do not support that rule"
    );

    let again = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--statement",
            "  ship   without a recorded execution. ",
            "--execution",
            exec_b.as_str(),
            "--execution",
            exec_a.as_str(),
        ],
    );
    assert!(again["proposed"].as_array().expect("proposed").is_empty());
    assert_eq!(again["skipped"][0]["candidate_id"], candidate);
    assert_eq!(again["skipped"][0]["status"], "rejected");

    let replacement = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--statement",
            "Prefer the smallest change that holds the tests.",
            "--execution",
            exec_a.as_str(),
            "--execution",
            exec_b.as_str(),
        ],
    );
    assert_eq!(
        replacement["proposed"][0]["replaces_candidate_id"], candidate,
        "{replacement}"
    );
    let projected = viva_ok(
        home.path(),
        &[
            "principle",
            "project",
            "--member",
            member.as_str(),
            "--out",
            home.path().join("out").to_str().expect("utf8"),
        ],
    );
    let pi = std::fs::read_to_string(home.path().join("out/pi/principles.md")).expect("pi");
    assert!(!pi.contains("Prefer the smallest change"));
    assert!(projected["principles"].as_array().expect("rows").is_empty());

    let audit = viva_ok(
        home.path(),
        &["principle", "audit", "--member", member.as_str()],
    );
    let reject = audit
        .as_array()
        .expect("audit")
        .iter()
        .find(|row| row["action"] == "reject")
        .expect("reject row");
    let sources = reject["source_execution_ids"].as_array().expect("sources");
    assert!(sources.iter().any(|id| id == exec_a.as_str()));
    assert!(sources.iter().any(|id| id == exec_b.as_str()));
}

#[test]
fn cli_digest_expires_a_candidate_left_unreviewed_through_downtime() {
    let home = init_home();
    let member = MemberId::new();
    let exec_a = ExecutionId::new();
    let statement = "Leave a candidate sitting for weeks.";
    let digest = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--statement",
            statement,
            "--execution",
            exec_a.as_str(),
            "--now",
            "2026-01-01T00:00:00Z",
        ],
    );
    let candidate = digest["proposed"][0]["candidate_id"]
        .as_str()
        .expect("candidate")
        .to_string();

    let expired = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--now",
            "2026-02-15T00:00:00Z",
        ],
    );
    assert_eq!(expired["expired"][0]["candidate_id"], candidate);
    assert_eq!(expired["expired"][0]["status"], "expired");
    let (ok, _stdout, stderr) = run_viva(
        home.path(),
        &["principle", "approve", "--candidate", &candidate],
    );
    assert!(!ok, "an expired candidate must not approve");
    assert!(stderr.contains("expired"), "{stderr}");

    let out = home.path().join("out");
    viva_ok(
        home.path(),
        &[
            "principle",
            "project",
            "--member",
            member.as_str(),
            "--out",
            out.to_str().expect("utf8"),
        ],
    );
    let pi = std::fs::read_to_string(out.join("pi/principles.md")).expect("pi");
    let claude = std::fs::read_to_string(out.join("claude/principles.md")).expect("claude");
    assert!(!pi.contains(statement) && !claude.contains(statement));

    let again = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--statement",
            statement,
            "--execution",
            exec_a.as_str(),
            "--now",
            "2026-03-01T00:00:00Z",
        ],
    );
    assert!(again["proposed"].as_array().expect("proposed").is_empty());
    assert_eq!(again["skipped"][0]["status"], "expired");

    let audit = viva_ok(
        home.path(),
        &["principle", "audit", "--member", member.as_str()],
    );
    let expire = audit
        .as_array()
        .expect("audit")
        .iter()
        .find(|row| row["action"] == "expire")
        .expect("expire");
    assert_eq!(expire["source_execution_ids"][0], exec_a.as_str());
}

#[test]
fn cli_rollback_restores_the_projection_and_keeps_the_bad_version() {
    let home = init_home();
    let member = MemberId::new();
    let exec_a = ExecutionId::new();
    let good = "Ask which repository before editing.";
    let bad = "WRONG SENTENCE push past the failing check.";
    let digest = viva_ok(
        home.path(),
        &[
            "principle",
            "digest",
            "--member",
            member.as_str(),
            "--statement",
            good,
            "--execution",
            exec_a.as_str(),
        ],
    );
    let candidate = digest["proposed"][0]["candidate_id"]
        .as_str()
        .expect("candidate")
        .to_string();
    let approved = viva_ok(
        home.path(),
        &["principle", "approve", "--candidate", &candidate],
    );
    let principle = approved["principle_id"]
        .as_str()
        .expect("principle")
        .to_string();
    viva_ok(
        home.path(),
        &[
            "principle",
            "edit",
            "--principle",
            &principle,
            "--body",
            bad,
        ],
    );
    let rolled = viva_ok(
        home.path(),
        &["principle", "rollback", member.as_str(), &principle, "1"],
    );
    assert_eq!(rolled["current_version"], 3);
    assert_eq!(rolled["version"]["body"], good);
    assert_eq!(rolled["version"]["write_kind"], "rollback");
    assert_eq!(rolled["version"]["rollback_from_version"], 1);

    let bad_row = viva_ok(
        home.path(),
        &[
            "principle",
            "version-get",
            "--principle",
            &principle,
            "--version",
            "2",
        ],
    );
    assert_eq!(bad_row["body"], bad);
    assert_eq!(bad_row["write_kind"], "edit");

    let out = home.path().join("out");
    viva_ok(
        home.path(),
        &[
            "principle",
            "project",
            "--member",
            member.as_str(),
            "--out",
            out.to_str().expect("utf8"),
        ],
    );
    for harness in ["pi", "claude"] {
        let text = std::fs::read_to_string(out.join(format!("{harness}/principles.md")))
            .expect("projection");
        assert!(text.contains(good), "{harness}: {text}");
        assert!(!text.contains("WRONG SENTENCE"), "{harness}: {text}");
    }

    let audit = viva_ok(
        home.path(),
        &["principle", "audit", "--member", member.as_str()],
    );
    for action in ["approve", "rollback"] {
        let row = audit
            .as_array()
            .expect("audit")
            .iter()
            .find(|row| row["action"] == action)
            .unwrap_or_else(|| panic!("missing {action}: {audit}"));
        assert_eq!(row["source_execution_ids"][0], exec_a.as_str());
    }
}

#[test]
fn approved_replacement_is_cited_and_does_not_drop_the_current_body() {
    let store = open_store();
    let principles = PrincipleStore::new(&store);
    let member = MemberId::new();
    let exec_a = ExecutionId::new();
    let first = principles
        .digest(
            &member,
            &[excerpt(&exec_a, "Keep notes next to the decision.")],
            "2026-03-01T00:00:00Z",
            &PrincipleActor::Owner,
        )
        .expect("digest");
    let approved = principles
        .approve(
            &first.proposed[0].candidate_id,
            None,
            &PrincipleActor::Owner,
        )
        .expect("approve");
    let second = principles
        .digest(
            &member,
            &[excerpt(
                &exec_a,
                "Keep the decision in the issue, not in chat.",
            )],
            "2026-03-02T00:00:00Z",
            &PrincipleActor::Owner,
        )
        .expect("second");
    assert_eq!(
        second.proposed[0].replaces_principle_id.as_deref(),
        Some(approved.principle_id.as_str())
    );
    let current = principles.approved_current(&member).expect("current");
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].body, "Keep notes next to the decision.");
    assert_eq!(current[0].version, 1);
    assert_eq!(approved.version.write_kind, PrincipleWriteKind::Approve);
}
