//! Memory domain v4: curation candidates, knowledge kinds, lifecycle (#116).

use rusqlite::Transaction;

use crate::foundation::error::Result;

pub fn apply_memory_v4(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
ALTER TABLE memory_facts ADD COLUMN summary TEXT NOT NULL DEFAULT '';
ALTER TABLE memory_facts ADD COLUMN knowledge_kind TEXT NOT NULL DEFAULT 'personal_memory'
    CHECK (knowledge_kind IN ('personal_memory', 'project_knowledge', 'team_knowledge'));
ALTER TABLE memory_facts ADD COLUMN lifecycle_status TEXT NOT NULL DEFAULT 'active'
    CHECK (lifecycle_status IN ('active', 'superseded', 'archived', 'retracted'));
ALTER TABLE memory_facts ADD COLUMN importance_score REAL;
ALTER TABLE memory_facts ADD COLUMN importance_reason TEXT NOT NULL DEFAULT '';
ALTER TABLE memory_facts ADD COLUMN confidence REAL NOT NULL DEFAULT 1.0;
ALTER TABLE memory_facts ADD COLUMN provenance_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE memory_facts ADD COLUMN valid_until TEXT;
ALTER TABLE memory_facts ADD COLUMN superseded_by_fact_id TEXT;

ALTER TABLE memory_links ADD COLUMN link_scope TEXT NOT NULL DEFAULT 'personal'
    CHECK (link_scope IN ('personal', 'project', 'team'));

CREATE TABLE memory_candidates (
    candidate_id       TEXT PRIMARY KEY,
    member_id          TEXT NOT NULL,
    project_id         TEXT,
    knowledge_kind     TEXT NOT NULL
        CHECK (knowledge_kind IN ('personal_memory', 'project_knowledge', 'team_knowledge')),
    summary            TEXT NOT NULL,
    content            TEXT NOT NULL,
    source             TEXT NOT NULL CHECK (length(trim(source)) > 0),
    provenance_json    TEXT NOT NULL DEFAULT '{}',
    status             TEXT NOT NULL
        CHECK (status IN ('proposed', 'admitted', 'rejected', 'superseded', 'retracted')),
    admission_json     TEXT NOT NULL DEFAULT '{}',
    fact_id            TEXT REFERENCES memory_facts(fact_id),
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);

CREATE INDEX idx_memory_candidates_member ON memory_candidates(member_id, status);
CREATE INDEX idx_memory_candidates_fact ON memory_candidates(fact_id);

CREATE TABLE memory_execution_usages (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    fact_ref      TEXT NOT NULL,
    member_id     TEXT NOT NULL,
    execution_id  TEXT NOT NULL,
    note          TEXT NOT NULL DEFAULT '',
    used_at       TEXT NOT NULL
);
"#,
    )?;
    Ok(())
}
