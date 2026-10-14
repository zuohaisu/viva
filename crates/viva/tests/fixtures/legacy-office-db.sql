-- A database written by the last release that still used the old names
-- ("office" tables, triggers, registry domain and event values): viva 0.3.6,
-- built from main at 1cdb00f. `tests/legacy_migration.rs` opens it with the
-- current binary to prove the one-time rename (issue #97).
--
-- Produced by running that real binary, not written by hand, with
-- `make_legacy_fixture.py` (same directory):
--   1. the old CLI creates a project and a task, starts the server through
--      `terminal create`, pauses/resumes, toggles auto-pull, syncs, and shuts
--      down (this registers every domain and writes the host row, the
--      pause/resume/sync events and a recovery event);
--   2. rows are added to every renamed table and to the tables that
--      reference them (sessions, executions, launch intents, conversation
--      nodes, settings, recovery events, control requests, gh evidence),
--      including words typed by a person ("Haisu's office", a task goal, an
--      event appended with origin `cli`) that the migration must leave alone;
--   3. the database is dumped with `iterdump()`, the checkout path replaced
--      by /fixture/repo.
-- Replaying this file creates the schema exactly as the old binary left it,
-- triggers included. Do not edit it; regenerate it from the old binary.
-- (Foreign keys are off while replaying: the dump lists tables
-- alphabetically, so a child table is filled before its parent exists.)
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE computer_actions (
    action_id       TEXT PRIMARY KEY,
    task_id         TEXT,
    actor_member_id TEXT NOT NULL,
    grant_id        TEXT,
    tool            TEXT NOT NULL,
    target          TEXT NOT NULL,
    argv_json       TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('executed', 'refused', 'failed')),
    reason          TEXT,
    pre_state       TEXT,
    action_output   TEXT,
    post_state      TEXT,
    foreground      INTEGER NOT NULL CHECK (foreground IN (0, 1)),
    recorded_at     TEXT NOT NULL
);
CREATE TABLE computer_capabilities (
    tool             TEXT PRIMARY KEY,
    present          INTEGER NOT NULL CHECK (present IN (0, 1)),
    detail           TEXT NOT NULL,
    permission_state TEXT NOT NULL
                     CHECK (permission_state IN ('available', 'unavailable', 'unknown')),
    checked_at       TEXT NOT NULL
);
CREATE TABLE control_requests (
    request_id      TEXT PRIMARY KEY,
    idempotency_key TEXT UNIQUE,
    caller_channel  TEXT NOT NULL,
    caller_role     TEXT NOT NULL,
    kind            TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('accepted', 'rejected', 'replayed')),
    rejection       TEXT,
    result_json     TEXT,
    received_at     TEXT NOT NULL
);
INSERT INTO "control_requests" VALUES('req-fixture',NULL,'chan-x','worker','ping','rejected','untrusted caller: channel `chan-x` was never issued by this office',NULL,'2026-10-13T00:00:00Z');
CREATE TABLE conversation_handoffs (
    handoff_id         TEXT PRIMARY KEY,
    from_node_id       TEXT NOT NULL REFERENCES conversation_nodes(node_id),
    to_harness         TEXT NOT NULL,
    to_capability      TEXT NOT NULL CHECK (to_capability IN ('native_fork', 'handoff_only')),
    native_session_id  TEXT,
    identity_member_id TEXT,
    task_id            TEXT,
    brief_snapshot     TEXT,
    history_ref        TEXT,
    created_at         TEXT NOT NULL
);
CREATE TABLE conversation_nodes (
    node_id            TEXT PRIMARY KEY,
    session_id         TEXT NOT NULL REFERENCES office_sessions(session_id),
    parent_node_id     TEXT REFERENCES conversation_nodes(node_id),
    display_name       TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    harness            TEXT NOT NULL,
    native_session_id  TEXT,
    native_node_id     TEXT,
    task_id            TEXT,
    created_at         TEXT NOT NULL,
    archived_at        TEXT
);
INSERT INTO "conversation_nodes" VALUES('node-fixture','sess-fixture-chat',NULL,'main','pi',NULL,NULL,NULL,'2026-10-13T00:00:00Z',NULL);
CREATE TABLE foreground_leases (
    task_id          TEXT PRIMARY KEY,
    host_pid         INTEGER,
    pid_start_marker TEXT,
    acquired_at      TEXT NOT NULL
);
CREATE TABLE github_evidence (
    evidence_id TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL,
    worktree_id TEXT,
    kind        TEXT NOT NULL CHECK (kind IN ('pr', 'issue', 'checks')),
    subject     TEXT NOT NULL,
    head_sha    TEXT,
    state_json  TEXT NOT NULL,
    fetched_at  TEXT NOT NULL,
    stale       INTEGER NOT NULL DEFAULT 0 CHECK (stale IN (0, 1))
);
INSERT INTO "github_evidence" VALUES('ev-fixture','task-819af55124ab45ee93f777633d08a726',NULL,'pr','pr 1',NULL,'{"state":"unavailable","reason":"write rejected: `gh pr merge` is outside the office''s read-only surface"}','2026-10-13T00:00:00Z',0);
CREATE TABLE grant_denials (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    subject      TEXT NOT NULL,
    action       TEXT NOT NULL,
    reason       TEXT NOT NULL,
    occurred_at  TEXT NOT NULL
);
CREATE TABLE grants (
    grant_id            TEXT PRIMARY KEY,
    parent_grant_id     TEXT REFERENCES grants(grant_id),
    principal_member_id TEXT,
    issued_by           TEXT NOT NULL CHECK (issued_by IN ('user', 'delegation')),
    task_id             TEXT,
    actions_json        TEXT NOT NULL,
    mode                TEXT NOT NULL
                        CHECK (mode IN ('READ', 'PROPOSE', 'ACT_WITH_APPROVAL', 'ACT_AUTONOMOUSLY')),
    status              TEXT NOT NULL CHECK (status IN ('live', 'revoked')),
    expires_at          TEXT,
    created_at          TEXT NOT NULL,
    revoked_at          TEXT,
    revoke_reason       TEXT
);
CREATE TABLE knowledge_items (
    item_id        TEXT PRIMARY KEY,
    title          TEXT NOT NULL CHECK (length(trim(title)) > 0),
    scope          TEXT NOT NULL CHECK (scope IN ('personal', 'project', 'team', 'skill')),
    owner_member_id TEXT,
    project_id     TEXT,
    tags_json      TEXT NOT NULL DEFAULT '[]',
    body_path      TEXT NOT NULL,
    provenance     TEXT NOT NULL CHECK (length(trim(provenance)) > 0),
    status         TEXT NOT NULL DEFAULT 'active'
                   CHECK (status IN ('active', 'archived', 'withdrawn', 'superseded')),
    superseded_by  TEXT REFERENCES knowledge_items(item_id),
    external_ref   TEXT,
    external_state TEXT NOT NULL DEFAULT 'none'
                   CHECK (external_state IN ('none', 'unavailable', 'linked', 'unresolvable')),
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    CHECK (scope <> 'personal' OR owner_member_id IS NOT NULL),
    CHECK ((status = 'superseded') = (superseded_by IS NOT NULL))
);
CREATE TABLE knowledge_lifecycle (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id     TEXT NOT NULL REFERENCES knowledge_items(item_id),
    action      TEXT NOT NULL CHECK (action IN ('archived', 'withdrawn', 'restored', 'corrected', 'superseded')),
    reason      TEXT NOT NULL,
    actor       TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
CREATE TABLE knowledge_usage (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    item_id      TEXT NOT NULL REFERENCES knowledge_items(item_id),
    execution_id TEXT NOT NULL,
    used_at      TEXT NOT NULL,
    note         TEXT NOT NULL DEFAULT ''
);
CREATE TABLE launch_intents (
    intent_id       TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL REFERENCES tasks(task_id),
    request_key     TEXT NOT NULL UNIQUE,
    member_id       TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('intended', 'confirmed', 'exited', 'unresolved')),
    execution_id    TEXT REFERENCES office_executions(execution_id),
    pid             INTEGER,
    pid_start_marker TEXT,
    intended_at     TEXT NOT NULL,
    confirmed_at    TEXT,
    exited_at       TEXT,
    exit_code       INTEGER
);
INSERT INTO "launch_intents" VALUES('intent-fixture','task-819af55124ab45ee93f777633d08a726','key-fixture','mem-fixture','confirmed','exec-fixture-running',NULL,NULL,'2026-10-13T00:00:00Z',NULL,NULL,NULL);
CREATE TABLE launch_specs (
    spec_id        TEXT PRIMARY KEY,
    execution_id   TEXT,
    terminal_id    TEXT,
    argv           TEXT NOT NULL,
    cwd            TEXT NOT NULL,
    member_id      TEXT,
    model_binding  TEXT,
    tool_binding   TEXT,
    worktree_id    TEXT,
    request_origin TEXT NOT NULL,
    budget_json    TEXT,
    created_at     TEXT NOT NULL
);
CREATE TABLE maintenance_proposals (
    proposal_id      TEXT PRIMARY KEY,
    session_id       TEXT NOT NULL REFERENCES maintenance_sessions(session_id),
    scan_id          TEXT NOT NULL REFERENCES maintenance_scans(scan_id),
    dedup_key        TEXT NOT NULL UNIQUE,
    kind             TEXT NOT NULL CHECK (kind IN
                         ('stale_knowledge', 'idle_skill', 'worktree_review', 'repo_cleanliness')),
    subject          TEXT NOT NULL,
    evidence         TEXT NOT NULL CHECK (length(trim(evidence)) > 0),
    suggested_action TEXT NOT NULL CHECK (suggested_action IN
                         ('archive_knowledge', 'disable_skill', 'human_review')),
    status           TEXT NOT NULL DEFAULT 'open'
                     CHECK (status IN ('open', 'executed', 'dismissed')),
    created_at       TEXT NOT NULL,
    resolved_at      TEXT,
    resolved_by      TEXT,
    resolution_note  TEXT
);
CREATE TABLE maintenance_scans (
    scan_id     TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES maintenance_sessions(session_id),
    kind        TEXT NOT NULL,
    started_at  TEXT NOT NULL,
    finished_at TEXT,
    proposed    INTEGER,
    duplicates  INTEGER
);
CREATE TABLE maintenance_sessions (
    session_id  TEXT PRIMARY KEY,
    host_pid    INTEGER NOT NULL,
    started_at  TEXT NOT NULL,
    ended_at    TEXT
);
CREATE TABLE member_bindings (
    member_id     TEXT PRIMARY KEY REFERENCES members(member_id),
    role          TEXT NOT NULL,
    model_binding TEXT NOT NULL,
    tools_json    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE TABLE members (
    member_id    TEXT PRIMARY KEY,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    created_at   TEXT NOT NULL
);
CREATE TABLE "memory_links" (
    link_id         TEXT PRIMARY KEY,
    fact_id         INTEGER NOT NULL,
    member_id       TEXT NOT NULL,
    project_id      TEXT,
    source          TEXT NOT NULL CHECK (length(trim(source)) > 0),
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'archived')),
    created_at      TEXT NOT NULL,
    archived_at     TEXT,
    archived_reason TEXT,
    UNIQUE (fact_id, member_id)
);
CREATE TABLE memory_usages (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    fact_id    INTEGER NOT NULL,
    member_id  TEXT NOT NULL,
    query      TEXT NOT NULL,
    used_at    TEXT NOT NULL
);
CREATE TABLE navigation_checkouts (
 worktree_id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(project_id),
 path TEXT NOT NULL UNIQUE, branch TEXT NOT NULL
);
CREATE TABLE navigation_selection (singleton INTEGER PRIMARY KEY CHECK(singleton=1), workspace_id TEXT REFERENCES workspaces(workspace_id));
CREATE TABLE office_events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id     TEXT NOT NULL UNIQUE,
    occurred_at  TEXT NOT NULL,
    domain       TEXT NOT NULL,
    kind         TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    origin       TEXT NOT NULL,
    payload      TEXT NOT NULL,
    CHECK (length(CAST(payload AS BLOB)) <= 65536)
);
INSERT INTO "office_events" VALUES(1,'evt-aa1bd460b95e4b728929f0521ad75a6d','2026-10-13T16:20:57.75754487Z','office_host','office_paused','office','host-5821f4c8195c4d30b062ecc356724214','office_host','{"note":"no new dispatch, no maintenance cycles; running executions untouched"}');
INSERT INTO "office_events" VALUES(2,'evt-da9b9dd2e6434abcb60683990e1bb615','2026-10-13T16:20:57.782423446Z','office_host','office_resumed','office','host-5821f4c8195c4d30b062ecc356724214','office_host','{"note":"dispatch and maintenance resumed"}');
INSERT INTO "office_events" VALUES(3,'evt-d1359465ecae4ae892ce180a4d0cc9bc','2026-10-13T16:20:57.811501649Z','office_host','auto_pull_toggled','office','host-5821f4c8195c4d30b062ecc356724214','office_host','{"on":true}');
INSERT INTO "office_events" VALUES(4,'evt-01e9fda179084815b81514ccd415a076','2026-10-13T16:20:57.876045159Z','office_host','git_auto_sync','project','proj-c8681673c1eb49e2b27437bca44b6f1e','office_host','{"kind":"main_checkout","outcome":"failed: validation: git pull --ff-only failed (the tree was left exactly as it was): validation: `git pull --ff-only` failed (1): There is no tracking information for the current branch.\nPlease specify which branch you want to merge with.\nSee git-pull(1) for details.\n\n    git pull <remote> <branch>\n\nIf you wish to set tracking information for this branch you can do so with:\n\n    git branch --set-upstream-to=<remote>/<branch> main\n\n","path":"/fixture/repo","project":"proj-c8681673c1eb49e2b27437bca44b6f1e"}');
INSERT INTO "office_events" VALUES(5,'evt-fixture-denied','2026-10-13T00:00:00Z','office_host','socket_grant_denied','socket_request','req-1','office_host','{"reason":"grant expired"}');
INSERT INTO "office_events" VALUES(6,'evt-fixture-user','2026-10-13T00:00:00Z','tasks_executions','office_paused','office','user-note','cli','{"note":"typed by a person, not by Viva"}');
CREATE TABLE office_executions (
    execution_id        TEXT PRIMARY KEY,
    task_id             TEXT NOT NULL,
    session_id          TEXT REFERENCES office_sessions(session_id),
    member_id           TEXT NOT NULL,
    attribution         TEXT NOT NULL,
    status              TEXT NOT NULL CHECK (status IN ('requested', 'running', 'stopped', 'failed', 'completed')),
    process_exit        INTEGER,
    completion_evidence TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    CHECK (status <> 'completed' OR completion_evidence IS NOT NULL)
);
INSERT INTO "office_executions" VALUES('exec-fixture-running','task-819af55124ab45ee93f777633d08a726','sess-fixture-run','mem-fixture','{}','running',NULL,NULL,'2026-10-13T00:00:00Z','2026-10-13T00:00:00Z');
INSERT INTO "office_executions" VALUES('exec-fixture-done','task-819af55124ab45ee93f777633d08a726',NULL,'mem-fixture','{}','completed',0,'evidence: tests green','2026-10-13T00:00:00Z','2026-10-13T00:00:00Z');
CREATE TABLE office_hosts (
    host_id    TEXT PRIMARY KEY,
    pid        INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    exited_at  TEXT,
    exit_kind  TEXT CHECK (exit_kind IN ('graceful', 'crashed') OR exit_kind IS NULL)
);
INSERT INTO "office_hosts" VALUES('host-5821f4c8195c4d30b062ecc356724214',557046,'2026-10-13T16:20:57.624007058Z','2026-10-13T16:20:57.931981249Z','graceful');
CREATE TABLE office_recovery_events (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id     TEXT NOT NULL,
    kind        TEXT NOT NULL,
    detail      TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
INSERT INTO "office_recovery_events" VALUES(1,'host-5821f4c8195c4d30b062ecc356724214','graceful_handoff','host stopped owned terminal(s) and recorded their exits; state persisted for the next host (1 terminal(s) were hosted here)','2026-10-13T16:20:57.932077391Z');
INSERT INTO "office_recovery_events" VALUES(2,'host-fixture','previous_host_crashed','host `host-old` did not record its exit','2026-10-13T00:00:00Z');
INSERT INTO "office_recovery_events" VALUES(3,'host-fixture','live_handoff_out','1 live terminal(s) transferred','2026-10-13T00:00:00Z');
CREATE TABLE office_sessions (
    session_id               TEXT PRIMARY KEY,
    kind                     TEXT NOT NULL CHECK (kind IN ('conversation', 'task_execution')),
    title                    TEXT NOT NULL,
    task_id                  TEXT,
    member_id                TEXT,
    harness                  TEXT,
    harness_native_session_id TEXT,
    created_at               TEXT NOT NULL,
    CHECK (kind <> 'task_execution' OR task_id IS NOT NULL)
);
INSERT INTO "office_sessions" VALUES('sess-fixture-chat','conversation','Haisu''s office chat',NULL,'mem-fixture','pi','native-1','2026-10-13T00:00:00Z');
INSERT INTO "office_sessions" VALUES('sess-fixture-run','task_execution','run','task-819af55124ab45ee93f777633d08a726','mem-fixture','pi',NULL,'2026-10-13T00:00:00Z');
CREATE TABLE office_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO "office_settings" VALUES('paused','false');
INSERT INTO "office_settings" VALUES('auto_pull','true');
INSERT INTO "office_settings" VALUES('last_sync_note','1 sync result(s) at 2026-10-13T16:20:57.87628306Z');
INSERT INTO "office_settings" VALUES('workbench_layout','{"version":1}');
CREATE TABLE project_references (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id      TEXT NOT NULL REFERENCES projects(project_id),
    reference_path  TEXT NOT NULL CHECK (reference_path LIKE '/%'),
    kind            TEXT NOT NULL CHECK (kind IN ('reference', 'dependency')),
    note            TEXT NOT NULL DEFAULT '',
    added_at        TEXT NOT NULL,
    removed_at      TEXT
);
CREATE TABLE projects (
    project_id      TEXT PRIMARY KEY,
    workspace_id    TEXT REFERENCES workspaces(workspace_id),
    display_name    TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    repo_path       TEXT NOT NULL UNIQUE CHECK (repo_path LIKE '/%'),
    active          INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at      TEXT NOT NULL,
    deactivated_at  TEXT,
    CHECK (active = 1 OR deactivated_at IS NOT NULL)
);
INSERT INTO "projects" VALUES('proj-c8681673c1eb49e2b27437bca44b6f1e',NULL,'Haisu''s office','/fixture/repo',1,'2026-10-13T16:20:57.570966899Z',NULL);
CREATE TABLE recovery_plans (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    terminal_id TEXT NOT NULL,
    task_id     TEXT,
    reset_at    TEXT NOT NULL,
    action      TEXT NOT NULL DEFAULT 'resume_prompt',
    state       TEXT NOT NULL DEFAULT 'scheduled'
                CHECK (state IN ('scheduled','fired','cancelled','expired')),
    created_at  TEXT NOT NULL,
    fired_at    TEXT
);
CREATE TABLE schema_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
INSERT INTO "schema_meta" VALUES('layout_version','1');
CREATE TABLE schema_migrations (
                domain     TEXT NOT NULL,
                version    INTEGER NOT NULL,
                name       TEXT NOT NULL,
                applied_at TEXT NOT NULL,
                PRIMARY KEY (domain, version)
            );
INSERT INTO "schema_migrations" VALUES('authority',1,'authority v1','2026-10-13T16:20:57.519512772Z');
INSERT INTO "schema_migrations" VALUES('conversations',1,'conversations v1','2026-10-13T16:20:57.520070221Z');
INSERT INTO "schema_migrations" VALUES('foundation',1,'foundation reference tables','2026-10-13T16:20:57.524166111Z');
INSERT INTO "schema_migrations" VALUES('git',1,'git v1','2026-10-13T16:20:57.524725905Z');
INSERT INTO "schema_migrations" VALUES('knowledge',1,'knowledge v1','2026-10-13T16:20:57.525415184Z');
INSERT INTO "schema_migrations" VALUES('maintenance',1,'maintenance v1','2026-10-13T16:20:57.526072202Z');
INSERT INTO "schema_migrations" VALUES('maintenance',2,'maintenance v2 proposal generations','2026-10-13T16:20:57.526294875Z');
INSERT INTO "schema_migrations" VALUES('members',1,'members v1','2026-10-13T16:20:57.52668193Z');
INSERT INTO "schema_migrations" VALUES('memory',1,'memory v1','2026-10-13T16:20:57.530171719Z');
INSERT INTO "schema_migrations" VALUES('memory',2,'memory v2 per-member links','2026-10-13T16:20:57.532754224Z');
INSERT INTO "schema_migrations" VALUES('office_host',1,'office host v1','2026-10-13T16:20:57.533168562Z');
INSERT INTO "schema_migrations" VALUES('office_host',2,'office host v2 (settings)','2026-10-13T16:20:57.533397095Z');
INSERT INTO "schema_migrations" VALUES('office_host',3,'office host v3 (recovery plans)','2026-10-13T16:20:57.533653973Z');
INSERT INTO "schema_migrations" VALUES('tasks_executions',1,'tasks and executions v1','2026-10-13T16:20:57.534397695Z');
INSERT INTO "schema_migrations" VALUES('tools_computer',1,'tools computer v1','2026-10-13T16:20:57.539469881Z');
INSERT INTO "schema_migrations" VALUES('tools_computer',2,'tools computer v2 foreground lease','2026-10-13T16:20:57.539842058Z');
INSERT INTO "schema_migrations" VALUES('tools_computer',3,'tools computer v3 repair legacy lease shape','2026-10-13T16:20:57.540098525Z');
INSERT INTO "schema_migrations" VALUES('workflows',1,'workflows v1','2026-10-13T16:20:57.540674028Z');
INSERT INTO "schema_migrations" VALUES('workspaces_projects',1,'workspaces and projects v1','2026-10-13T16:20:57.5413126Z');
INSERT INTO "schema_migrations" VALUES('workspaces_projects',2,'workspace compositions and discovered checkouts','2026-10-13T16:20:57.545151642Z');
CREATE TABLE skills (
    skill_id      TEXT PRIMARY KEY,
    item_id       TEXT NOT NULL REFERENCES knowledge_items(item_id),
    entry_path    TEXT NOT NULL,
    enabled       INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    registered_at TEXT NOT NULL,
    disabled_at   TEXT
);
CREATE TABLE task_briefs (
    brief_id     TEXT PRIMARY KEY,
    task_id      TEXT NOT NULL REFERENCES tasks(task_id),
    brief_json   TEXT NOT NULL,
    generated_at TEXT NOT NULL
);
CREATE TABLE task_outcomes (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id      TEXT NOT NULL REFERENCES tasks(task_id),
    kind         TEXT NOT NULL CHECK (kind IN ('completed', 'corrected', 'withdrawn', 'reopened')),
    evidence     TEXT NOT NULL,
    actor        TEXT NOT NULL,
    recorded_at  TEXT NOT NULL
);
CREATE TABLE task_results (
    result_id     TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL REFERENCES tasks(task_id),
    execution_id  TEXT,
    source        TEXT NOT NULL CHECK (source IN ('process', 'qa', 'pr_ci', 'user')),
    kind          TEXT NOT NULL,
    dedup_key     TEXT NOT NULL UNIQUE,
    payload       TEXT NOT NULL,
    recorded_at   TEXT NOT NULL
);
CREATE TABLE task_worktrees (
    worktree_id   TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL,
    repo_root     TEXT NOT NULL,
    worktree_path TEXT NOT NULL,
    branch        TEXT NOT NULL,
    base_sha      TEXT NOT NULL,
    source        TEXT NOT NULL CHECK (source IN ('created', 'adopted')),
    created_at    TEXT NOT NULL,
    released_at   TEXT,
    UNIQUE(repo_root, branch)
);
CREATE TABLE tasks (
    task_id             TEXT PRIMARY KEY,
    goal                TEXT NOT NULL CHECK (length(trim(goal)) > 0),
    constraints_json    TEXT NOT NULL DEFAULT '[]',
    assignee_member_id  TEXT,
    workspace_id        TEXT,
    project_id          TEXT,
    status              TEXT NOT NULL DEFAULT 'open'
                        CHECK (status IN ('open', 'in_progress', 'done', 'cancelled')),
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
INSERT INTO "tasks" VALUES('task-819af55124ab45ee93f777633d08a726','Migrate the office host','[]',NULL,NULL,NULL,'open','2026-10-13T16:20:57.594623093Z','2026-10-13T16:20:57.594623093Z');
CREATE TABLE terminal_events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    terminal_id  TEXT NOT NULL,
    owner        TEXT NOT NULL CHECK (owner IN ('member_execution', 'user_shell', 'agent_cli', 'test_run')),
    execution_id TEXT,
    event_kind   TEXT NOT NULL CHECK (event_kind IN ('spawned', 'input', 'resize', 'snapshot', 'stopped', 'exited')),
    payload      TEXT NOT NULL,
    occurred_at  TEXT NOT NULL,
    CHECK ((owner = 'member_execution') = (execution_id IS NOT NULL))
);
INSERT INTO "terminal_events" VALUES(1,'term-1ff83170c87b42eeb3998bad93c3e7d1','user_shell',NULL,'spawned','{"pid":557074,"pid_start_marker":"pid=557074 argv0=sleep spawned_at=2026-10-13T16:20:57.739931454Z"}','2026-10-13T16:20:57.740074224Z');
INSERT INTO "terminal_events" VALUES(2,'term-1ff83170c87b42eeb3998bad93c3e7d1','user_shell',NULL,'stopped','{"pid":557074,"pid_start_marker":"pid=557074 argv0=sleep spawned_at=2026-10-13T16:20:57.739931454Z"}','2026-10-13T16:20:57.931538868Z');
CREATE TABLE tool_probe_cache (
    argv_json       TEXT PRIMARY KEY,
    available       INTEGER NOT NULL CHECK (available IN (0, 1)),
    detail          TEXT NOT NULL,
    probed_at       TEXT NOT NULL
);
CREATE TABLE worker_credentials (
    credential_id TEXT PRIMARY KEY,
    channel_id    TEXT NOT NULL UNIQUE,
    execution_id  TEXT,
    member_id     TEXT NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('active', 'revoked')),
    issued_at     TEXT NOT NULL,
    revoked_at    TEXT
);
CREATE TABLE workflow_configs (
    config_id    TEXT PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    config_json  TEXT NOT NULL,
    created_at   TEXT NOT NULL
);
CREATE TABLE workflow_runs (
    run_id        TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL REFERENCES tasks(task_id),
    config_id     TEXT NOT NULL REFERENCES workflow_configs(config_id),
    status        TEXT NOT NULL CHECK (status IN ('running', 'paused', 'completed', 'aborted')),
    current_step  INTEGER NOT NULL,
    head_sha      TEXT NOT NULL,
    pause_reason  TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);
CREATE TABLE workflow_step_records (
    record_id   TEXT PRIMARY KEY,
    run_id      TEXT NOT NULL REFERENCES workflow_runs(run_id),
    step_index  INTEGER NOT NULL,
    step_id     TEXT NOT NULL,
    role        TEXT NOT NULL,
    actor_member_id TEXT NOT NULL,
    outcome     TEXT NOT NULL CHECK (outcome IN ('passed', 'failed')),
    evidence_json TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
CREATE TABLE workspace_compositions (
 workspace_id TEXT PRIMARY KEY REFERENCES workspaces(workspace_id),
 source_path TEXT UNIQUE, document TEXT NOT NULL DEFAULT '{}', opened_at TEXT NOT NULL
);
CREATE TABLE workspace_folders (
 workspace_id TEXT NOT NULL REFERENCES workspaces(workspace_id),
 ordinal INTEGER NOT NULL, project_id TEXT REFERENCES projects(project_id),
 folder TEXT NOT NULL, resolved_path TEXT, diagnostic TEXT,
 PRIMARY KEY(workspace_id, ordinal)
);
CREATE TABLE workspaces (
    workspace_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    created_at   TEXT NOT NULL
);
CREATE TRIGGER office_events_no_update
BEFORE UPDATE ON office_events
BEGIN SELECT RAISE(ABORT, 'office_events is append-only'); END;
CREATE TRIGGER office_events_no_delete
BEFORE DELETE ON office_events
BEGIN SELECT RAISE(ABORT, 'office_events is append-only'); END;
CREATE UNIQUE INDEX one_active_workflow_run_per_task
    ON workflow_runs(task_id) WHERE status IN ('running', 'paused');
DELETE FROM "sqlite_sequence";
INSERT INTO "sqlite_sequence" VALUES('terminal_events',2);
INSERT INTO "sqlite_sequence" VALUES('office_events',6);
INSERT INTO "sqlite_sequence" VALUES('office_recovery_events',3);
COMMIT;
