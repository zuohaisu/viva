//! F04 / issue #130 acceptance: native memory (FTS trigram, minimal HRR, link)
//! through the real `viva` binary and an isolated `VIVA_HOME` — no Hermes, no Python.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use viva::authority::{Actor, AuthorityEngine, GrantMode};
use viva::foundation::ids::{MemberId, ProjectId};
use viva::foundation::store::{
    DOMAIN_AUTHORITY, DOMAIN_FOUNDATION, DOMAIN_MEMORY, FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::memory::{HRR_DIM, LinkStatus, MemorySearch, MemoryService, encode_fact_hrr};

fn frozen() -> viva::foundation::store::FrozenMigrations {
    MigrationRegistry::new()
        .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
        .register(DOMAIN_MEMORY, 1, "memory v1", viva::memory::MEMORY_V1_SQL)
        .register(
            DOMAIN_MEMORY,
            2,
            "memory v2 per-member links",
            viva::memory::MEMORY_V2_SQL,
        )
        .register_fn(
            DOMAIN_MEMORY,
            3,
            "memory v3 native facts fts hrr",
            viva::memory::migrate::apply_memory_v3,
        )
        .register(
            DOMAIN_AUTHORITY,
            1,
            "authority v1",
            viva::authority::AUTHORITY_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

fn run_viva(home: &Path, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .env_remove("VIVA_MEMORY_DB")
        .env_remove("VIVA_HERMES_AGENT")
        .output()
        .expect("spawn viva");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn init_home(home: &Path) {
    let (ok, _, stderr) = run_viva(home, &["init"]);
    assert!(ok, "init: {stderr}");
}

const EN_FACT: &str = "Viva F04: the team release train deploys every Tuesday at noon";
const CN_FACT: &str = "Viva F04 测试：演示环境每周五下午重建";

#[test]
fn cli_memory_works_without_hermes_or_python() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let member = MemberId::new();
    let mid = member.as_str();
    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "remember",
            "--member",
            mid,
            "--content",
            EN_FACT,
            "--source",
            "f04 native acceptance",
        ],
    );
    assert!(ok, "remember: {stderr}\n{stdout}");
    let link: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        link["fact_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("nf-"))
    );

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "search",
            "--member",
            mid,
            "--query",
            "release train",
        ],
    );
    assert!(ok, "search: {stderr}");
    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(body["state"], "fetched");
    assert_eq!(body["facts"].as_array().unwrap().len(), 1);
}

#[test]
fn cli_memory_status_probe_archive_restore_without_hermes() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let member = MemberId::new();
    let mid = member.as_str();

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "remember",
            "--member",
            mid,
            "--content",
            EN_FACT,
            "--source",
            "cli five-command acceptance",
        ],
    );
    assert!(ok, "remember: {stderr}\n{stdout}");
    let link: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let fact_id = link["fact_id"].as_str().unwrap();

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &["memory", "probe", "--member", mid, "--entity", "release"],
    );
    assert!(ok, "probe: {stderr}");
    let probe: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(probe["state"], "fetched");

    let (ok, stdout, stderr) = run_viva(home.path(), &["memory", "status"]);
    assert!(ok, "status: {stderr}");
    let status: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(status["storage"], "native");
    assert_eq!(status["probe"]["state"], "ready");
    assert_eq!(status["probe"]["facts_total"], 1);
    assert_eq!(status["probe"]["legacy_hermes_links"], 0);

    let (ok, stdout, stderr) = run_viva(
        home.path(),
        &[
            "grant",
            "--member",
            mid,
            "--action",
            "maintain_knowledge",
            "--mode",
            "ACT_WITH_APPROVAL",
        ],
    );
    assert!(ok, "grant: {stderr}");
    let grant: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let grant_id = grant["grant_id"].as_str().unwrap();

    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "archive",
            "--fact",
            fact_id,
            "--member",
            mid,
            "--grant",
            grant_id,
            "--reason",
            "acceptance archive",
        ],
    );
    assert!(ok, "archive: {stderr}");

    let (ok, stdout, _) = run_viva(
        home.path(),
        &[
            "memory",
            "search",
            "--member",
            mid,
            "--query",
            "release train",
        ],
    );
    assert!(ok);
    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(body["facts"].as_array().unwrap().is_empty());

    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "restore",
            "--fact",
            fact_id,
            "--member",
            mid,
            "--grant",
            grant_id,
            "--reason",
            "acceptance restore",
        ],
    );
    assert!(ok, "restore: {stderr}");
}

#[test]
fn chinese_trigram_substring_and_two_char_fallback() {
    let home = TempDir::new().unwrap();
    init_home(home.path());
    let member = MemberId::new();
    let mid = member.as_str();
    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "remember",
            "--member",
            mid,
            "--content",
            CN_FACT,
            "--source",
            "cn write",
        ],
    );
    assert!(ok, "remember cn: {stderr}");

    let (ok, stdout, _) = run_viva(
        home.path(),
        &["memory", "search", "--member", mid, "--query", "演示环境"],
    );
    assert!(ok);
    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        body["facts"].as_array().unwrap().len(),
        1,
        "3+ char CJK substring"
    );

    let (ok, stdout, _) = run_viva(
        home.path(),
        &["memory", "search", "--member", mid, "--query", "记忆"],
    );
    assert!(ok);
    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        body["facts"].as_array().unwrap().is_empty(),
        "two-char query uses fallback; this content has no 记忆"
    );

    let (ok, _, stderr) = run_viva(
        home.path(),
        &[
            "memory",
            "remember",
            "--member",
            mid,
            "--content",
            "团队记忆：每周五复盘",
            "--source",
            "two-char",
        ],
    );
    assert!(ok, "remember two-char fixture: {stderr}");
    let (ok, stdout, _) = run_viva(
        home.path(),
        &["memory", "search", "--member", mid, "--query", "记忆"],
    );
    assert!(ok);
    let body: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        body["facts"].as_array().unwrap().len(),
        1,
        "two-char fallback finds substring in scoped facts"
    );
}

#[test]
fn member_and_project_isolation_native() {
    let store = Store::open_in_memory(&frozen()).expect("store");
    let service = MemoryService::new(&store);
    let alice = MemberId::new();
    let bob = MemberId::new();
    let project_a = ProjectId::new();
    let project_b = ProjectId::new();

    let alice_link = service
        .remember(&alice, Some(&project_a), EN_FACT, "alice", "general", "")
        .expect("write");

    let result = service
        .search(&bob, Some(&project_a), "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("fetched");
    };
    assert!(facts.is_empty(), "bob does not see alice's scoped facts");

    let bob_link = service
        .remember(&bob, Some(&project_a), EN_FACT, "bob", "general", "")
        .expect("bob write");
    assert_eq!(bob_link.fact_id, alice_link.fact_id, "content dedupe");
    assert_ne!(bob_link.link_id, alice_link.link_id);

    let result = service
        .search(&alice, Some(&project_b), "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("fetched");
    };
    assert!(facts.is_empty());
}

fn exit_actor(store: &Store, member: &MemberId) -> Actor {
    let authority = AuthorityEngine::new(store);
    let grant = authority
        .issue_root_grant(
            Some(member.clone()),
            None,
            vec!["maintain_knowledge".into()],
            GrantMode::ActWithApproval,
            None,
        )
        .expect("grant");
    Actor::Member {
        member: member.clone(),
        grant: Some(grant.grant_id),
    }
}

#[test]
fn archive_restore_native() {
    let store = Store::open_in_memory(&frozen()).expect("store");
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let actor = exit_actor(&store, &member);
    let authority = AuthorityEngine::new(&store);
    let link = service
        .remember(&member, None, EN_FACT, "src", "general", "")
        .expect("write");

    service
        .archive(&link.fact_id, "superseded", &actor, &authority)
        .expect("archive");
    let result = service
        .search(&member, None, "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("fetched");
    };
    assert!(facts.is_empty());

    service
        .restore(&link.fact_id, "bring back", &actor, &authority)
        .expect("restore");
    let result = service
        .search(&member, None, "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("fetched");
    };
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].content, EN_FACT);
}

#[test]
fn duplicate_content_and_link_semantics() {
    let store = Store::open_in_memory(&frozen()).expect("store");
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let first = service
        .remember(&member, None, EN_FACT, "first source", "general", "")
        .expect("write");
    let second = service
        .remember(&member, None, EN_FACT, "second source", "general", "")
        .expect("dedupe");
    assert_eq!(first.fact_id, second.fact_id);
    assert_eq!(first.link_id, second.link_id);
    let link = service
        .link_for(&first.fact_id, &member)
        .expect("link")
        .expect("exists");
    assert_eq!(link.source, "first source");

    let archived = service
        .archive(
            &first.fact_id,
            "hold",
            &exit_actor(&store, &member),
            &AuthorityEngine::new(&store),
        )
        .expect("archive");
    assert_eq!(archived.status, LinkStatus::Archived);
    let relink = service
        .link_fact(&first.fact_id, &member, None, "new source")
        .expect("relink");
    assert_eq!(relink.status, LinkStatus::Archived);
    assert_eq!(relink.source, "first source");
}

#[test]
fn link_rejects_legacy_hermes_and_missing_facts() {
    let store = Store::open_in_memory(&frozen()).expect("store");
    store
        .connection()
        .execute(
            "INSERT INTO memory_links(link_id, fact_ref, storage_origin, member_id, source, status, created_at)
             VALUES ('legacy', '42', 'legacy_hermes', 'm', 's', 'active', 't')",
            [],
        )
        .unwrap();
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let err = service
        .link_fact("42", &member, None, "src")
        .expect_err("legacy");
    assert!(err.to_string().contains("legacy Hermes"));
    let err = service
        .link_fact("nf-does-not-exist", &member, None, "src")
        .expect_err("missing");
    assert!(err.to_string().contains("native memory fact"));
}

#[test]
fn link_adopts_existing_native_fact_for_harness_switch() {
    let store = Store::open_in_memory(&frozen()).expect("store");
    let service = MemoryService::new(&store);
    let writer = MemberId::new();
    let adopter = MemberId::new();
    let original = service
        .remember(&writer, None, EN_FACT, "old harness", "general", "")
        .expect("write");
    let adopted = service
        .link_fact(&original.fact_id, &adopter, None, "harness switch")
        .expect("adopt");
    assert_eq!(adopted.fact_id, original.fact_id);
    let result = service
        .search(&adopter, None, "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("fetched");
    };
    assert_eq!(facts.len(), 1);
}

#[test]
fn hrr_frozen_encoding_snapshot() {
    let vec = encode_fact_hrr("hrr acceptance anchor phrase", &[], HRR_DIM);
    assert_eq!(vec.len(), HRR_DIM);
    let blob = viva::memory::hrr::phases_to_bytes(&vec, HRR_DIM);
    assert_eq!(blob.len(), 4 + HRR_DIM * 4);
    let again = viva::memory::hrr::bytes_to_phases(&blob, HRR_DIM).expect("parse");
    for (a, b) in vec.iter().zip(again.iter()) {
        assert!((a - b).abs() < 1e-5);
    }
    // Spot-check against upstream encode_atom("__hrr_empty__") first phase.
    let empty = viva::memory::hrr::encode_atom("__hrr_empty__", HRR_DIM);
    assert!((empty[0] - 3.990651).abs() < 1e-4);
}

#[test]
fn legacy_hermes_link_rows_survive_migration() {
    let store = Store::open_in_memory(&frozen()).expect("store");
    store
        .connection()
        .execute(
            "INSERT INTO memory_links(link_id, fact_ref, storage_origin, member_id, source, status, created_at)
             VALUES ('l1', '7', 'legacy_hermes', 'm1', 's', 'active', 't')",
            [],
        )
        .unwrap();
    let n: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM memory_links WHERE storage_origin = 'legacy_hermes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    let service = MemoryService::new(&store);
    let member = MemberId::new();
    let result = service
        .search(&member, None, "anything", 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("fetched");
    };
    assert!(
        facts.is_empty(),
        "legacy rows do not participate in retrieval"
    );
}
