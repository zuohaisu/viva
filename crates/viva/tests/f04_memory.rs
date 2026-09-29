//! F04 acceptance tests (issue #26): Viva's namespace/audit/exit layer
//! over the REAL bundled Holographic memory.
//!
//! Tests touching the real provider are `#[ignore]`d: on CI (no Hermes
//! checkout) they would otherwise skip silently and be counted as pass —
//! real-evidence criteria must be visible as ignored, never as green.
//! Run them on a machine with the checkout:
//!   cargo test --test f04_memory -- --ignored
//! They ALWAYS use a temp store; the user's real `memory_store.db` is
//! never touched.

use std::path::PathBuf;

use viva::authority::{Actor, AuthorityEngine, GrantMode};
use viva::foundation::ids::{GrantId, MemberId, ProjectId};
use viva::foundation::store::{
    DOMAIN_AUTHORITY, DOMAIN_FOUNDATION, DOMAIN_MEMORY, FOUNDATION_V1_SQL, MigrationRegistry, Store,
};
use viva::memory::{AdapterConfig, LinkStatus, MemorySearch, MemoryService};

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
        .register(
            DOMAIN_AUTHORITY,
            1,
            "authority v1",
            viva::authority::AUTHORITY_V1_SQL,
        )
        .freeze()
        .expect("registry")
}

fn store() -> Store {
    Store::open_in_memory(&frozen()).expect("store")
}

/// Locate the REAL implementation on this machine, with a TEMP db. Returns
/// None when the checkout/venv/adapter is not present.
fn real_adapter() -> Option<AdapterConfig> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = manifest
        .join("../../extensions/pi/memory/memory_adapter.py")
        .canonicalize()
        .ok()?;
    if !script.is_file() {
        return None;
    }
    let agent_dir = std::env::var("VIVA_HERMES_AGENT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".hermes").join("hermes-agent")
        });
    if !agent_dir
        .join("plugins")
        .join("memory")
        .join("holographic")
        .is_dir()
    {
        return None;
    }
    let python = std::env::var("VIVA_MEMORY_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|_| agent_dir.join("venv").join("bin").join("python"));
    if !python.is_file() {
        return None;
    }
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db_path = dir.path().join("memory.db");
    std::mem::forget(dir); // rows live in the file; keep it for the test
    Some(AdapterConfig {
        python,
        adapter_script: script,
        agent_dir,
        db_path,
    })
}

macro_rules! real_service {
    ($store:ident, $service:ident) => {
        let Some(adapter) = real_adapter() else {
            // Under `cargo test -- --ignored` a silent early-return would
            // look like green tests with no real evidence (QA finding N6):
            // an ignored test that cannot find its provider must be a
            // VISIBLE failure, not a pass.
            panic!(
                "real Holographic checkout not found — these tests produce real evidence \
                 only on a machine with the checkout; set VIVA_HERMES_AGENT and \
                 VIVA_MEMORY_PYTHON, or run without --ignored"
            );
        };
        let $store = store();
        let $service = MemoryService::with_adapter(&$store, adapter);
    };
}

/// A granted exit actor: memory exits are office-state mutations and need
/// a live `maintain_knowledge` grant (same action as knowledge curation).
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
        .expect("exit grant");
    Actor::Member {
        member: member.clone(),
        grant: Some(grant.grant_id),
    }
}

const EN_FACT: &str = "Viva F04: the office release train deploys every Tuesday at noon";
const CN_FACT: &str = "Viva F04 测试：演示环境每周五下午重建";

/// Acceptance: "真实 Holographic 写入/检索有证据" — a real write through
/// the real provider comes back searchable, and every recall leaves usage
/// evidence. The documented retrieval boundary is asserted honestly:
/// exact-term recall (EN and CJK full runs) works; paraphrase and partial
/// CJK recall do not (FTS candidates gate everything) — reported, never
/// papered over.
#[test]
#[ignore = "needs the real Holographic checkout (VIVA_HERMES_AGENT); a silent skip must not count as evidence"]
fn real_write_search_roundtrip_with_usage_evidence() {
    real_service!(store, service);
    let member = MemberId::new();

    let link = service
        .remember(
            &member,
            None,
            EN_FACT,
            "F04 integration test write",
            "general",
            "",
        )
        .expect("real write");
    assert!(link.fact_id > 0, "the external store returned a fact id");
    service
        .remember(
            &member,
            None,
            CN_FACT,
            "F04 integration test write",
            "general",
            "",
        )
        .expect("real cn write");

    // Exact-term recall, EN.
    let result = service
        .search(&member, None, "release train", 16 * 1024)
        .expect("search runs");
    let MemorySearch::Fetched {
        facts,
        hidden_unclaimable,
    } = result
    else {
        panic!("the store is configured and must be reachable");
    };
    assert_eq!(facts.len(), 1, "the EN fact is recalled");
    assert_eq!(facts[0].content, EN_FACT);
    assert_eq!(facts[0].source, "F04 integration test write");
    assert_eq!(hidden_unclaimable, 0);
    let usage = service.usage_of(facts[0].fact_id).expect("usage");
    assert_eq!(usage.len(), 1, "the recall left usage evidence");
    assert_eq!(usage[0].query, "release train");

    // CJK recall boundary, verified against the provider: the FTS5
    // unicode61 tokenizer does not segment CJK, so the full run matches
    // while a substring of the run does not. Asserted as the honest
    // behavior, not hidden.
    let result = service
        .search(&member, None, "演示环境每周五下午重建", 16 * 1024)
        .expect("cn search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("reachable");
    };
    assert_eq!(facts.len(), 1, "the full CJK run matches itself");
    assert_eq!(facts[0].content, CN_FACT);
    let result = service
        .search(&member, None, "演示环境", 16 * 1024)
        .expect("cn partial search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("reachable");
    };
    assert!(
        facts.is_empty(),
        "partial CJK runs are honestly not recalled by this provider"
    );

    // Paraphrase recall is NOT achieved (FTS candidate gate) — asserted,
    // and the product choice is documented in extensions/pi/memory/README.md.
    let result = service
        .search(&member, None, "weekly shipping cadence", 16 * 1024)
        .expect("paraphrase search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("reachable");
    };
    assert!(
        facts.iter().all(|f| f.content != EN_FACT),
        "paraphrase recall is honestly empty for this provider"
    );

    // Entity probe through the provider's real interface.
    let result = service
        .probe(&member, None, "Viva", 16 * 1024)
        .expect("probe");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("reachable");
    };
    assert!(
        facts.iter().any(|f| f.content == EN_FACT),
        "entity probe recalls the linked fact (when the extractor keys on it)"
    );
}

/// Acceptance: "两个成员/两个项目的隔离经过真实验证" — read AND write
/// sides. Writing the same content as another member yields the writer's
/// OWN link (the provider dedupes the fact row; claims stay separate);
/// exits are owner-scoped; a member cannot archive another's claim.
#[test]
#[ignore = "needs the real Holographic checkout (VIVA_HERMES_AGENT); a silent skip must not count as evidence"]
fn member_and_project_isolation_on_the_real_store() {
    real_service!(store, service);
    let alice = MemberId::new();
    let bob = MemberId::new();
    let project_a = ProjectId::new();
    let project_b = ProjectId::new();

    let alice_link = service
        .remember(
            &alice,
            Some(&project_a),
            EN_FACT,
            "alice wrote this",
            "general",
            "",
        )
        .expect("write");

    // Same member, right project: visible.
    let result = service
        .search(&alice, Some(&project_a), "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched {
        facts,
        hidden_unclaimable,
    } = result
    else {
        panic!("reachable");
    };
    assert_eq!(facts.len(), 1);
    assert_eq!(hidden_unclaimable, 0);

    // Another member: hidden, and the boundary is visible as a count.
    let result = service
        .search(&bob, Some(&project_a), "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched {
        facts,
        hidden_unclaimable,
    } = result
    else {
        panic!("reachable");
    };
    assert!(facts.is_empty(), "bob never sees alice's fact");
    assert_eq!(hidden_unclaimable, 1, "the hidden hit is counted");

    // Write side: bob writing the SAME content gets his OWN link — not
    // alice's — and can then recall it under his own claim.
    let bob_link = service
        .remember(
            &bob,
            Some(&project_a),
            EN_FACT,
            "bob wrote this too",
            "general",
            "",
        )
        .expect("bob write");
    assert_eq!(
        bob_link.fact_id, alice_link.fact_id,
        "provider dedupes content"
    );
    assert_ne!(bob_link.link_id, alice_link.link_id, "claims stay separate");
    assert_eq!(bob_link.member_id, bob, "bob's claim names bob");
    assert_eq!(
        bob_link.source, "bob wrote this too",
        "bob's provenance kept"
    );
    let result = service
        .search(&bob, Some(&project_a), "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("reachable");
    };
    assert_eq!(facts.len(), 1, "bob recalls through his own link");
    assert_eq!(facts[0].source, "bob wrote this too");

    // Same member, another project: hidden too.
    let result = service
        .search(&alice, Some(&project_b), "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched {
        facts,
        hidden_unclaimable,
    } = result
    else {
        panic!("reachable");
    };
    assert!(facts.is_empty());
    assert_eq!(hidden_unclaimable, 1);

    // Exit is owner-scoped: bob cannot exit a fact he has no claim on
    // (alice's CN fact), even with a valid grant.
    let authority = AuthorityEngine::new(&store);
    let bob_actor = exit_actor(&store, &bob);
    let alice_cn = service
        .remember(
            &alice,
            Some(&project_a),
            CN_FACT,
            "alice only",
            "general",
            "",
        )
        .expect("alice cn write");
    let err = service
        .archive(alice_cn.fact_id, "mallory exit", &bob_actor, &authority)
        .expect_err("bob cannot exit a fact he never claimed");
    assert!(err.to_string().contains("own memory link"), "got: {err}");
    let alice_after = service
        .link_for(alice_cn.fact_id, &alice)
        .expect("link")
        .expect("exists");
    assert_eq!(alice_after.status, LinkStatus::Active, "untouched");

    // And bob exiting his own claim works — alice's stays untouched.
    let bob_actor_own = exit_actor(&store, &bob);
    let bob_archived = service
        .archive(
            bob_link.fact_id,
            "bob reconsiders",
            &bob_actor_own,
            &authority,
        )
        .expect("bob exits his own claim");
    assert_eq!(bob_archived.status, LinkStatus::Archived);
    let alice_still = service
        .link_for(alice_link.fact_id, &alice)
        .expect("link")
        .expect("exists");
    assert_eq!(alice_still.status, LinkStatus::Active, "alice untouched");
}

/// Acceptance: "失效事实过滤/可恢复归档；remove 的物理删除不得冒充归档"
/// — archive hides the fact from selection while the row stays on disk in
/// the real store; restore brings it back. The service exposes no delete.
#[test]
#[ignore = "needs the real Holographic checkout (VIVA_HERMES_AGENT); a silent skip must not count as evidence"]
fn archive_is_recoverable_and_delete_does_not_exist() {
    real_service!(store, service);
    let adapter_db = service.adapter_config().db_path.clone();
    let member = MemberId::new();
    let actor = exit_actor(&store, &member);
    let authority = AuthorityEngine::new(&store);
    let link = service
        .remember(&member, None, EN_FACT, "archivable fact", "general", "")
        .expect("write");

    let archived = service
        .archive(
            link.fact_id,
            "superseded by the new runbook",
            &actor,
            &authority,
        )
        .expect("archive");
    assert_eq!(archived.status, LinkStatus::Archived);
    assert!(
        archived
            .archived_reason
            .as_deref()
            .unwrap_or("")
            .contains("superseded"),
        "the exit reason is kept: {:?}",
        archived.archived_reason
    );

    let result = service
        .search(&member, None, "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched {
        facts,
        hidden_unclaimable,
    } = result
    else {
        panic!("reachable");
    };
    assert!(facts.is_empty(), "archived facts leave selection");
    assert_eq!(hidden_unclaimable, 1);

    // The fact still exists in the real store on disk (the temp db).
    let conn = rusqlite::Connection::open_with_flags(
        &adapter_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("temp db readable");
    let still_there: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM facts WHERE fact_id = ?1",
            [link.fact_id],
            |row| row.get(0),
        )
        .expect("fact row");
    assert_eq!(still_there, 1, "archive never deletes from the store");

    let restored = service
        .restore(
            link.fact_id,
            "owner asked to bring it back",
            &actor,
            &authority,
        )
        .expect("restore");
    assert_eq!(restored.status, LinkStatus::Active);
    let result = service
        .search(&member, None, "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("reachable");
    };
    assert_eq!(facts.len(), 1, "restored facts are selectable again");
}

/// Acceptance: "服务不可用不假称记得" — a broken checkout path answers
/// `unavailable` with a reason, never a fake empty success. Runs
/// everywhere (no checkout needed).
#[test]
fn unavailable_store_says_so() {
    let store = store();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let script = manifest
        .join("../../extensions/pi/memory/memory_adapter.py")
        .canonicalize()
        .expect("adapter script exists in the repo");
    let dir = tempfile::TempDir::new().expect("dir");
    let config = AdapterConfig {
        python: PathBuf::from("python3"),
        adapter_script: script,
        agent_dir: PathBuf::from("/nonexistent/hermes-agent"),
        db_path: dir.path().join("memory.db"),
    };
    let service = MemoryService::with_adapter(&store, config);
    let member = MemberId::new();
    let result = service
        .search(&member, None, "anything", 1024)
        .expect("the search call itself succeeds");
    let MemorySearch::Unavailable { reason } = result else {
        panic!("a broken checkout must be unavailable");
    };
    assert!(
        reason.contains("no bundled") || reason.contains("ModuleNotFound"),
        "got: {reason}"
    );

    // Writes fail closed as well — and refuse without provenance first.
    assert!(
        service
            .remember(&member, None, "x", "", "general", "")
            .is_err()
    );
}

/// Re-linking one's own fact is idempotent; the first provenance stays.
#[test]
#[ignore = "needs the real Holographic checkout (VIVA_HERMES_AGENT); a silent skip must not count as evidence"]
fn duplicate_content_links_once_with_first_source() {
    real_service!(store, service);
    let member = MemberId::new();
    let first = service
        .remember(&member, None, EN_FACT, "first source", "general", "")
        .expect("write");
    let second = service
        .remember(&member, None, EN_FACT, "second source", "general", "")
        .expect("dedup write");
    assert_eq!(first.fact_id, second.fact_id, "the store dedupes content");
    assert_eq!(first.link_id, second.link_id, "one claim per member");
    let link = service
        .link_for(first.fact_id, &member)
        .expect("link")
        .expect("exists");
    assert_eq!(link.source, "first source", "the first provenance wins");
}

/// The harness-switch path: `link` adopts an existing external fact id by
/// asset reference and the adopted fact is then recallable. Runs on the
/// link layer only — still needs the provider to resolve the id.
#[test]
#[ignore = "needs the real Holographic checkout (VIVA_HERMES_AGENT); a silent skip must not count as evidence"]
fn link_adopts_an_external_fact_by_asset_reference() {
    real_service!(store, service);
    let writer = MemberId::new();
    let adopter = MemberId::new();
    // The "previous harness" wrote this fact; only its id is known.
    let original = service
        .remember(
            &writer,
            None,
            EN_FACT,
            "written in the old harness",
            "general",
            "",
        )
        .expect("write");

    // The new harness re-adopts the SAME asset reference (fact id).
    let adopted = service
        .link_fact(
            original.fact_id,
            &adopter,
            None,
            "adopted from harness switch, asset reference fact-id",
        )
        .expect("adopt");
    assert_eq!(adopted.fact_id, original.fact_id);

    let result = service
        .search(&adopter, None, "release train", 16 * 1024)
        .expect("search");
    let MemorySearch::Fetched { facts, .. } = result else {
        panic!("reachable");
    };
    assert_eq!(facts.len(), 1, "the adopted fact is recallable");
    assert!(facts[0].source.contains("harness switch"));
    let _ = GrantId::new(); // keep the import honest for future exit flows
}
