//! Memory domain v3: native facts, FTS trigram, link storage origins.

use rusqlite::Transaction;

use crate::foundation::error::Result;

pub fn apply_memory_v3(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
CREATE TABLE memory_facts (
    fact_id       TEXT PRIMARY KEY,
    content       TEXT NOT NULL,
    content_hash  TEXT NOT NULL UNIQUE,
    category      TEXT NOT NULL DEFAULT 'general',
    tags          TEXT NOT NULL DEFAULT '',
    trust_score   REAL NOT NULL DEFAULT 1.0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    hrr_vector    BLOB
);

CREATE INDEX idx_memory_facts_hash ON memory_facts(content_hash);

CREATE TABLE memory_entities (
    entity_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL,
    entity_type TEXT NOT NULL DEFAULT 'unknown',
    aliases     TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL
);

CREATE UNIQUE INDEX idx_memory_entities_name ON memory_entities(name);

CREATE TABLE memory_fact_entities (
    fact_id   TEXT NOT NULL REFERENCES memory_facts(fact_id),
    entity_id INTEGER NOT NULL REFERENCES memory_entities(entity_id),
    PRIMARY KEY (fact_id, entity_id)
);

CREATE VIRTUAL TABLE memory_facts_fts USING fts5(
    content,
    category,
    tags,
    content='memory_facts',
    content_rowid='rowid',
    tokenize='trigram'
);

CREATE TRIGGER memory_facts_ai AFTER INSERT ON memory_facts BEGIN
    INSERT INTO memory_facts_fts(rowid, content, category, tags)
        VALUES (new.rowid, new.content, new.category, new.tags);
END;

CREATE TRIGGER memory_facts_ad AFTER DELETE ON memory_facts BEGIN
    INSERT INTO memory_facts_fts(memory_facts_fts, rowid, content, category, tags)
        VALUES ('delete', old.rowid, old.content, old.category, old.tags);
END;

CREATE TRIGGER memory_facts_au AFTER UPDATE ON memory_facts BEGIN
    INSERT INTO memory_facts_fts(memory_facts_fts, rowid, content, category, tags)
        VALUES ('delete', old.rowid, old.content, old.category, old.tags);
    INSERT INTO memory_facts_fts(rowid, content, category, tags)
        VALUES (new.rowid, new.content, new.category, new.tags);
END;

CREATE TABLE memory_links_v3 (
    link_id         TEXT PRIMARY KEY,
    fact_ref        TEXT NOT NULL,
    storage_origin  TEXT NOT NULL CHECK (storage_origin IN ('native', 'legacy_hermes')),
    member_id       TEXT NOT NULL,
    project_id      TEXT,
    source          TEXT NOT NULL CHECK (length(trim(source)) > 0),
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'archived')),
    created_at      TEXT NOT NULL,
    archived_at     TEXT,
    archived_reason TEXT,
    UNIQUE (fact_ref, member_id, storage_origin)
);

INSERT INTO memory_links_v3
    (link_id, fact_ref, storage_origin, member_id, project_id, source, status,
     created_at, archived_at, archived_reason)
SELECT link_id, CAST(fact_id AS TEXT), 'legacy_hermes', member_id, project_id, source, status,
       created_at, archived_at, archived_reason
FROM memory_links;

DROP TABLE memory_links;
ALTER TABLE memory_links_v3 RENAME TO memory_links;

CREATE TABLE memory_usages_v2 (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    fact_ref   TEXT NOT NULL,
    member_id  TEXT NOT NULL,
    query      TEXT NOT NULL,
    used_at    TEXT NOT NULL
);

INSERT INTO memory_usages_v2 (seq, fact_ref, member_id, query, used_at)
SELECT seq, CAST(fact_id AS TEXT), member_id, query, used_at FROM memory_usages;

DROP TABLE memory_usages;
ALTER TABLE memory_usages_v2 RENAME TO memory_usages;
"#,
    )?;
    Ok(())
}
