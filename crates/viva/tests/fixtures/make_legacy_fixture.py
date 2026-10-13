"""Regenerate `legacy-office-db.sql` with the PRE-removal `viva` binary.

Build viva 0.3.6 (main at 1cdb00f) and run, with a SHORT scratch directory
(the server's unix socket path must fit in SUN_LEN):

    python3 make_legacy_fixture.py <old-viva-binary> /tmp/vfx <out.sql>

then put the provenance header from the committed fixture back on top.
"""
import os
import shutil
import sqlite3
import subprocess
import sys
import time

old_bin, scratch, out = sys.argv[1:4]
shutil.rmtree(scratch, ignore_errors=True)
os.makedirs(scratch)
home = os.path.join(scratch, "home")
repo = os.path.join(scratch, "repo")
os.makedirs(repo)
env = dict(os.environ, VIVA_HOME=home)
git_env = dict(env, GIT_AUTHOR_NAME="f", GIT_AUTHOR_EMAIL="f@example.invalid",
               GIT_COMMITTER_NAME="f", GIT_COMMITTER_EMAIL="f@example.invalid")
for cmd in (["git", "init", "-q", "-b", "main"], ["git", "commit", "-q", "--allow-empty", "-m", "fixture"]):
    subprocess.run(cmd, cwd=repo, env=git_env, check=True)


def viva(*args, check=True):
    return subprocess.run([old_bin, *args], env=env, check=check, capture_output=True, text=True)


viva("init")
viva("create-project", "--name", "audit", "--repo", repo)
viva("create-task", "--goal", "audit task")
viva("terminal", "create", "--purpose", "audit", "--", "sleep", "20")  # starts the resident server
viva("pause")
viva("resume")
viva("sync", "auto-pull", "on")
viva("sync", "now")
viva("shutdown")
db = os.path.join(home, "office.db")
for _ in range(50):
    if not os.path.exists(os.path.join(home, "office.sock")):
        break
    time.sleep(0.1)

con = sqlite3.connect(db)
con.execute("PRAGMA foreign_keys=ON")
task_id = con.execute("select task_id from tasks").fetchone()[0]
now = "2026-10-13T00:00:00Z"
with con:
    # Data in every table the migration renames.
    con.execute("INSERT INTO office_sessions VALUES ('sess-fixture-chat','conversation',\"Haisu's office chat\",NULL,'mem-fixture','pi','native-1',?)", (now,))
    con.execute("INSERT INTO office_sessions VALUES ('sess-fixture-run','task_execution','run',?,'mem-fixture','pi',NULL,?)", (task_id, now))
    con.execute("INSERT INTO office_executions VALUES ('exec-fixture-running',?, 'sess-fixture-run','mem-fixture','{}','running',NULL,NULL,?,?)", (task_id, now, now))
    con.execute("INSERT INTO office_executions VALUES ('exec-fixture-done',?, NULL,'mem-fixture','{}','completed',0,'evidence: tests green',?,?)", (task_id, now, now))
    con.execute("INSERT INTO office_recovery_events(host_id,kind,detail,recorded_at) VALUES ('host-fixture','previous_host_crashed','host `host-old` did not record its exit',?)", (now,))
    con.execute("INSERT INTO office_recovery_events(host_id,kind,detail,recorded_at) VALUES ('host-fixture','live_handoff_out','1 live terminal(s) transferred',?)", (now,))
    con.execute("INSERT INTO office_settings VALUES ('workbench_layout','{\"version\":1}')")
    # The other tables whose stored schema referenced the renamed tables.
    con.execute("INSERT INTO conversation_nodes(node_id,session_id,display_name,harness,created_at) VALUES ('node-fixture','sess-fixture-chat','main','pi',?)", (now,))
    con.execute("INSERT INTO launch_intents(intent_id,task_id,request_key,member_id,state,execution_id,intended_at) VALUES ('intent-fixture',?,'key-fixture','mem-fixture','confirmed','exec-fixture-running',?)", (task_id, now))
    # Event rows: Viva-written values with each legacy spelling, plus
    # USER-written content that merely contains the word and must survive.
    ins = "INSERT INTO office_events(event_id,occurred_at,domain,kind,subject_type,subject_id,origin,payload) VALUES (?,?,?,?,?,?,?,?)"
    con.execute(ins, ("evt-fixture-denied", now, "office_host", "socket_grant_denied", "socket_request", "req-1", "office_host", '{"reason":"grant expired"}'))
    con.execute(ins, ("evt-fixture-user", now, "tasks_executions", "office_paused", "office", "user-note", "cli", '{"note":"typed by a person, not by Viva"}'))
    # Free-text values the old code could persist (control ledger, gh evidence).
    con.execute("INSERT INTO control_requests(request_id,idempotency_key,caller_channel,caller_role,kind,status,rejection,result_json,received_at) VALUES ('req-fixture',NULL,'chan-x','worker','ping','rejected','untrusted caller: channel `chan-x` was never issued by this office',NULL,?)", (now,))
    con.execute("INSERT INTO github_evidence(evidence_id,task_id,worktree_id,kind,subject,head_sha,state_json,fetched_at,stale) VALUES ('ev-fixture',?,NULL,'pr','pr 1',NULL,?,?,0)",
                (task_id, '{"state":"unavailable","reason":"write rejected: `gh pr merge` is outside the office\'s read-only surface"}', now))
    # User-typed names that contain the word (never rewritten).
    con.execute("UPDATE projects SET display_name = \"Haisu's office\"")
    con.execute("UPDATE tasks SET goal = 'Migrate the office host'")
con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
fk = con.execute("PRAGMA foreign_key_check").fetchall()
assert not fk, fk
con.close()

lines = []
con = sqlite3.connect(db)
for line in con.iterdump():
    lines.append(line)
text = "\n".join(lines) + "\n"
text = text.replace(repo, "/fixture/repo")
text = text.replace(os.path.realpath(repo), "/fixture/repo")
with open(out, "w") as fh:
    fh.write(text)
print("rows:", {t: con.execute(f'select count(*) from {t}').fetchone()[0] for t in
      ("office_events", "office_sessions", "office_executions", "office_hosts", "office_settings", "office_recovery_events")})
