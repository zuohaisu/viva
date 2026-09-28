//! The `viva` binary: the office's executable entry point.
//!
//! V01 scope: initialize a private `VIVA_HOME`, prove the SQLite store with
//! real events across process restarts, and report state (`doctor`). The TUI
//! and supervision arrive with later milestones; this entry never claims
//! more than the office currently does.

use std::process::ExitCode;
use std::str::FromStr as _;

use viva::foundation::events::{self, NewEvent};
use viva::foundation::paths::{database_path, ensure_private_dir, viva_home};
use viva::foundation::store::{KNOWN_DOMAINS, Store};
use viva::foundation::{OfficeError, OfficeResult};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("viva: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> OfficeResult<()> {
    match args.first().map(String::as_str) {
        Some("init") => cmd_init(),
        Some("doctor") => cmd_doctor(),
        Some("event") => cmd_event(args.get(1..).unwrap_or(&[])),
        Some("office") => cmd_office(args.get(1..).unwrap_or(&[])),
        Some("conversations") => cmd_conversations(args.get(1..).unwrap_or(&[])),
        Some("workbench") => cmd_workbench(),
        Some("data") => cmd_data(args.get(1..).unwrap_or(&[])),
        Some("version" | "--version" | "-V") => {
            println!("viva {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            print_usage();
            Err(OfficeError::Validation(
                "unknown or missing subcommand".into(),
            ))
        }
    }
}

/// `viva workbench` — the interactive parallel-development workbench, as
/// THE active office host for this VIVA_HOME. One process owns the store,
/// the terminals and the UI; `q` really stops owned terminals, joins the
/// exit watchers, persists the handoff and restores the terminal.
fn cmd_workbench() -> OfficeResult<()> {
    let home = viva::foundation::paths::viva_home(None);
    viva::foundation::paths::ensure_private_dir(&home)?;
    let host = viva::office::OfficeHost::open(&home)?;
    let shared = host.shared();
    eprintln!(
        "workbench: office host {} active (pid {}) — home {}",
        shared.host_id,
        std::process::id(),
        home.display()
    );
    let server = host.serve_background();
    let result = viva::tui::workbench::run(std::sync::Arc::clone(&shared));
    // Whatever the loop's outcome, the serve loop must end and the channel
    // must be released (run() already shut the office down on `q`).
    shared
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = server.join();
    result
}

fn print_usage() {
    println!(
        "viva — the Personal AI Office host

USAGE:
    viva init
        Create VIVA_HOME (default ~/.viva, override with $VIVA_HOME) and the
        office database, applying all registered migrations.

    viva doctor
        Report state root, database, schema versions and table counts.

    viva event add <domain> <kind> <subject-type> <subject-id> [payload-json]
        Append one office event (origin: cli).

    viva event list [limit]
        List the most recent office events (default 20).

    viva office start
        Become the active office host for this VIVA_HOME (one per home).
        Serves the control channel until `viva office shutdown` arrives.

    viva office status
        Office snapshot. Uses the live channel when a host is active, and
        falls back to offline store reads otherwise (read-only by design).

    viva office dispatch --task <id> --member <id> --grant <id>
        --request-key <key> --cwd <dir> -- argv...
        Dispatch one task execution under a grant over the real channel.
        Idempotent by --request-key: a retry never spawns twice.

    viva office terminals
        List terminals registered by the active host.

    viva office stop-terminal <terminal-id>
        Stop one terminal (its process group only).

    viva office result <task-id>
        Show the results recorded for a task.

    viva office shutdown
        Ask the active host to stop dispatch, stop owned terminals, persist
        the handoff and release the channel.

    viva conversations <create|fork|rename|set-native|attach-task|detach-task|archive|tree|handoff> [flags]
        Conversation metadata (office-owned tree; the harness owns the transcript).

    viva workbench
        Run the interactive parallel-development workbench as THE active
        office host for this VIVA_HOME (needs a real terminal).

    viva data export --out <dir>
        Read-only export (dump) of every fact table in this VIVA_HOME to
        JSON files plus a manifest. Never touches anything outside the
        store — no worktrees, no historical data directories.

    viva version"
    );
}

fn open_office_store() -> OfficeResult<Store> {
    let home = viva_home(None);
    ensure_private_dir(&home)?;
    // The product entry opens the FULL office composition (every delivered
    // domain), not just the foundation slice — `viva init` must leave a
    // store the office can actually use.
    Store::open(&database_path(&home), viva::office::office_migrations())
}

fn cmd_init() -> OfficeResult<()> {
    let home = viva_home(None);
    ensure_private_dir(&home)?;
    let store = open_office_store()?;
    println!("viva home: {}", home.display());
    println!("database:  {}", database_path(&home).display());
    for (domain, version) in store.schema_versions() {
        println!("schema:    {domain} v{version}");
    }
    Ok(())
}

fn cmd_doctor() -> OfficeResult<()> {
    let home = viva_home(None);
    let db = database_path(&home);
    println!("home:      {}", home.display());
    println!(
        "database:  {} ({})",
        db.display(),
        if db.exists() { "present" } else { "missing" }
    );
    let store = open_office_store()?;
    println!("state:     openable");
    for (domain, version) in store.schema_versions() {
        println!("schema:    {domain} v{version}");
    }
    for (domain, version, name, applied_at) in store.applied_migrations()? {
        println!("migration: {domain} v{version} `{name}` at {applied_at}");
    }
    for table in [
        "office_events",
        "office_sessions",
        "office_executions",
        "launch_specs",
        "terminal_events",
        "control_requests",
    ] {
        println!("count:     {table} = {}", store.row_count(table)?);
    }
    Ok(())
}

fn cmd_event(args: &[String]) -> OfficeResult<()> {
    match args.first().map(String::as_str) {
        Some("add") => cmd_event_add(args.get(1..).unwrap_or(&[])),
        Some("list") => cmd_event_list(args.get(1..).unwrap_or(&[])),
        _ => {
            print_usage();
            Err(OfficeError::Validation(
                "event needs `add` or `list`".into(),
            ))
        }
    }
}

fn cmd_event_add(args: &[String]) -> OfficeResult<()> {
    let [domain, kind, subject_type, subject_id, rest @ ..] = args else {
        return Err(OfficeError::Validation(
            "usage: viva event add <domain> <kind> <subject-type> <subject-id> [payload-json]"
                .into(),
        ));
    };
    if rest.len() > 1 {
        return Err(OfficeError::Validation(
            "usage: viva event add <domain> <kind> <subject-type> <subject-id> [payload-json]"
                .into(),
        ));
    }
    let domain = parse_domain(domain)?;
    let payload = match rest.first() {
        Some(text) => serde_json::from_str(text)?,
        None => serde_json::Value::Null,
    };
    let store = open_office_store()?;
    let recorded = events::append(
        &store,
        NewEvent {
            domain,
            kind: kind.clone(),
            subject_type: subject_type.clone(),
            subject_id: subject_id.clone(),
            origin: "cli".into(),
            payload,
        },
    )?;
    println!("event {} seq {} recorded", recorded.event_id, recorded.seq);
    Ok(())
}

fn cmd_event_list(args: &[String]) -> OfficeResult<()> {
    if args.len() > 1 {
        return Err(OfficeError::Validation(
            "usage: viva event list [limit]".into(),
        ));
    }
    let limit: u32 = match args.first() {
        Some(text) => text
            .parse()
            .map_err(|_| OfficeError::Validation(format!("`{text}` is not a valid limit")))?,
        None => 20,
    };
    let store = open_office_store()?;
    for event in events::list(&store, limit)? {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            event.seq,
            event.event_id,
            event.domain,
            event.kind,
            event.subject_type,
            event.subject_id
        );
    }
    Ok(())
}

fn parse_domain(name: &str) -> OfficeResult<viva::foundation::store::Domain> {
    KNOWN_DOMAINS
        .iter()
        .copied()
        .find(|domain| domain.as_str() == name)
        .ok_or_else(|| {
            OfficeError::Validation(format!(
                "unknown domain `{name}`; known domains: {}",
                KNOWN_DOMAINS
                    .iter()
                    .map(|d| d.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

// ---------------------------------------------------------------------------
// Office control plane (V07)
// ---------------------------------------------------------------------------

fn cmd_office(args: &[String]) -> OfficeResult<()> {
    let home = viva::foundation::paths::viva_home(None);
    match args.first().map(String::as_str) {
        Some("start") => cmd_office_start(&home),
        Some("status") => cmd_office_status(&home),
        Some("dispatch") => cmd_office_dispatch(&home, args.get(1..).unwrap_or(&[])),
        Some("terminals") => cmd_office_query(&home, viva::office::OfficeRequestKind::TerminalList),
        Some("stop-terminal") => {
            let terminal_id = args.get(1).ok_or_else(|| {
                OfficeError::Validation("usage: viva office stop-terminal <terminal-id>".into())
            })?;
            cmd_office_query(
                &home,
                viva::office::OfficeRequestKind::TerminalStop {
                    terminal_id: terminal_id.clone(),
                },
            )
        }
        Some("result") => {
            let task_id = args.get(1).ok_or_else(|| {
                OfficeError::Validation("usage: viva office result <task-id>".into())
            })?;
            cmd_office_query(
                &home,
                viva::office::OfficeRequestKind::TaskResults {
                    task_id: task_id.clone(),
                },
            )
        }
        Some("shutdown") => {
            cmd_office_query(&home, viva::office::OfficeRequestKind::Shutdown)?;
            // Human commentary goes to stderr; stdout stays pure JSON for
            // callers that parse it (the extension envelope, tooling).
            eprintln!("office: shutdown accepted; owned terminals stopped, handoff persisted");
            Ok(())
        }
        Some("brief") => cmd_office_brief(args.get(1..).unwrap_or(&[])),
        Some("handoff") => cmd_office_handoff(&home, args.get(1..).unwrap_or(&[])),
        _ => {
            print_usage();
            Err(OfficeError::Validation(
                "office needs start | status | dispatch | terminals | stop-terminal | result | brief | handoff | shutdown"
                    .into(),
            ))
        }
    }
}

/// `viva office brief <task-id>` — offline, read-only task brief from the
/// store (the extension's context-entry endpoint).
fn cmd_office_brief(args: &[String]) -> OfficeResult<()> {
    let task_id = args
        .first()
        .ok_or_else(|| OfficeError::Validation("usage: viva office brief <task-id>".into()))?;
    let task_id = viva::foundation::ids::TaskId::from_str(task_id)?;
    let home = viva::foundation::paths::viva_home(None);
    let db = viva::foundation::paths::database_path(&home);
    if !db.exists() {
        return Err(OfficeError::Validation(format!(
            "no office store at {} (run `viva init` first); nothing was created",
            db.display()
        )));
    }
    viva::foundation::paths::ensure_private_dir(&home)?;
    let store = viva::foundation::store::Store::open(&db, viva::office::office_migrations())?;
    let tasks = viva::tasks::TaskRegistry::new(&store);
    let brief = tasks.generate_brief(&task_id)?;
    println!("{}", serde_json::to_string_pretty(&brief)?);
    Ok(())
}

/// `viva office handoff --task <id> --member <id> --summary <text>` —
/// records a member-reported handoff over the live channel. A member
/// report is a fact about who said what; it never completes a task.
fn cmd_office_handoff(home: &std::path::Path, args: &[String]) -> OfficeResult<()> {
    let mut task_id = None;
    let mut member_id = None;
    let mut summary = None;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        i += 1;
        let value = args
            .get(i)
            .cloned()
            .ok_or_else(|| OfficeError::Validation(format!("flag {flag} needs a value")))?;
        match flag {
            "--task" => task_id = Some(value),
            "--member" => member_id = Some(value),
            "--summary" => summary = Some(value),
            other => {
                return Err(OfficeError::Validation(format!(
                    "unknown handoff flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    let (Some(task_id), Some(member_id), Some(summary)) = (task_id, member_id, summary) else {
        return Err(OfficeError::Validation(
            "usage: viva office handoff --task <id> --member <id> --summary <text>".into(),
        ));
    };
    let response = viva::office::send_request(
        home,
        viva::office::new_request(viva::office::OfficeRequestKind::Handoff {
            task_id,
            member_id,
            summary,
        }),
    )?;
    print_office_response(&response)
}

fn cmd_office_start(home: &std::path::Path) -> OfficeResult<()> {
    let host = viva::office::OfficeHost::open(home)?;
    eprintln!(
        "office host {} active (pid {}) — home {}",
        host.shared().host_id,
        std::process::id(),
        home.display()
    );
    host.serve()?;
    eprintln!("office host released the control channel");
    Ok(())
}

fn cmd_office_status(home: &std::path::Path) -> OfficeResult<()> {
    let request = viva::office::new_request(viva::office::OfficeRequestKind::Status);
    match viva::office::send_request(home, request) {
        Ok(response) => print_office_response(&response),
        Err(live_err) => {
            // Read-only status is answerable offline by design; say both
            // facts plainly.
            eprintln!("office: live channel unavailable: {live_err}");
            let offline = viva::office::offline_status(home)?;
            println!("{}", serde_json::to_string_pretty(&offline)?);
            Ok(())
        }
    }
}

fn cmd_office_query(
    home: &std::path::Path,
    kind: viva::office::OfficeRequestKind,
) -> OfficeResult<()> {
    let response = viva::office::send_request(home, viva::office::new_request(kind))?;
    print_office_response(&response)
}

fn cmd_office_dispatch(home: &std::path::Path, args: &[String]) -> OfficeResult<()> {
    let mut task_id = None;
    let mut member_id = None;
    let mut grant_id = None;
    let mut request_key = None;
    let mut cwd: Option<String> = None;
    let mut worktree_id = None;
    let mut argv: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let value = |i: &mut usize| -> OfficeResult<String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| OfficeError::Validation(format!("flag {flag} needs a value")))
        };
        match flag {
            "--task" => task_id = Some(value(&mut i)?),
            "--member" => member_id = Some(value(&mut i)?),
            "--grant" => grant_id = Some(value(&mut i)?),
            "--request-key" => request_key = Some(value(&mut i)?),
            "--cwd" => cwd = Some(value(&mut i)?),
            "--worktree" => worktree_id = Some(value(&mut i)?),
            "--" => {
                argv = args[i + 1..].to_vec();
                break;
            }
            other => {
                return Err(OfficeError::Validation(format!(
                    "unknown dispatch flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    let (Some(task_id), Some(member_id), Some(grant_id), Some(request_key), Some(cwd)) =
        (task_id, member_id, grant_id, request_key, cwd)
    else {
        return Err(OfficeError::Validation(
            "usage: viva office dispatch --task <id> --member <id> --grant <id> \
             --request-key <key> --cwd <dir> [--worktree <id>] -- argv..."
                .into(),
        ));
    };
    if argv.is_empty() {
        return Err(OfficeError::Validation(
            "dispatch needs an explicit argv after `--` (no joined shell strings)".into(),
        ));
    }
    let response = viva::office::send_request(
        home,
        viva::office::new_request(viva::office::OfficeRequestKind::Dispatch {
            task_id,
            member_id,
            grant_id,
            request_key,
            argv,
            cwd,
            worktree_id,
        }),
    )?;
    print_office_response(&response)
}

fn print_office_response(response: &viva::office::OfficeResponse) -> OfficeResult<()> {
    if response.ok {
        println!(
            "{}",
            serde_json::to_string_pretty(
                response.result.as_ref().unwrap_or(&serde_json::Value::Null)
            )?
        );
        Ok(())
    } else {
        Err(OfficeError::Validation(
            response
                .error
                .clone()
                .unwrap_or_else(|| "rejected without a reason".into()),
        ))
    }
}

// ---------------------------------------------------------------------------
// Conversation metadata CLI (V10) — office-owned display names, fork trees
// and handoff records. The harness-native transcript is never copied here.
// ---------------------------------------------------------------------------

fn cmd_conversations(args: &[String]) -> OfficeResult<()> {
    let home = viva::foundation::paths::viva_home(None);
    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(&home),
        viva::office::office_migrations(),
    )?;
    let registry = viva::conversations::ConversationRegistry::new(&store);
    let mut named = std::collections::HashMap::new();
    // args[0] is the subcommand; everything after is --flag value pairs.
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].as_str();
        i += 1;
        let value = args
            .get(i)
            .cloned()
            .ok_or_else(|| OfficeError::Validation(format!("flag {flag} needs a value")))?;
        named.insert(flag.to_string(), value);
        i += 1;
    }
    let get = |key: &str| named.get(key).cloned();
    let require = |key: &str| -> OfficeResult<String> {
        get(key)
            .ok_or_else(|| OfficeError::Validation(format!("conversations {args:?} needs --{key}")))
    };

    match args.first().map(String::as_str) {
        Some("create") => {
            let session_id = viva::foundation::ids::SessionId::from_str(&require("--session")?)?;
            let node = registry.create_root(
                &session_id,
                require("--name")?,
                get("--harness").unwrap_or_else(|| "pi".into()),
            )?;
            println!("{}", serde_json::to_string_pretty(&node)?);
        }
        Some("fork") => {
            // Records a fork; call this only after the harness natively
            // forked (the extension enforces the ordering).
            let node = registry.record_fork(
                &require("--parent")?,
                require("--name")?,
                get("--native-session"),
                get("--native-node"),
            )?;
            println!("{}", serde_json::to_string_pretty(&node)?);
        }
        Some("rename") => {
            registry.rename(&require("--node")?, require("--name")?)?;
            println!("renamed");
        }
        Some("set-native") => {
            registry.set_native_ref(
                &require("--node")?,
                require("--native-session")?,
                get("--native-node"),
            )?;
            println!("native reference set");
        }
        Some("attach-task") => {
            registry.attach_task(
                &require("--node")?,
                &viva::foundation::ids::TaskId::from_str(&require("--task")?)?,
            )?;
            println!("task attached");
        }
        Some("detach-task") => {
            registry.detach_task(&require("--node")?)?;
            println!("task detached");
        }
        Some("archive") => {
            registry.archive(&require("--node")?)?;
            println!("archived (history and children untouched)");
        }
        Some("tree") => {
            let session_id = viva::foundation::ids::SessionId::from_str(&require("--session")?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&registry.tree(&session_id)?)?
            );
        }
        Some("handoff") => {
            let handoff = registry.record_handoff(
                &require("--node")?,
                require("--to")?,
                viva::conversations::ForkCapability::from_str_value(
                    &get("--capability").unwrap_or_else(|| "handoff_only".into()),
                )?,
                get("--native-session"),
                get("--member")
                    .map(|m| viva::foundation::ids::MemberId::from_str(&m))
                    .transpose()?,
                get("--task")
                    .map(|t| viva::foundation::ids::TaskId::from_str(&t))
                    .transpose()?,
                get("--brief"),
                get("--history-ref"),
            )?;
            println!("{}", serde_json::to_string_pretty(&handoff)?);
        }
        _ => {
            return Err(OfficeError::Validation(
                "conversations needs create | fork | rename | set-native | attach-task | detach-task | archive | tree | handoff"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// `viva data export --out <dir>` — read-only asset inventory and dump
/// (V13): every user table in this VIVA_HOME goes to one JSON file per
/// table plus a manifest. This is an export for inspection and preserve,
/// not a restorable backup: there is no schema migration or import path.
/// The database is opened READ-ONLY and nothing outside the store is
/// touched: no worktrees, no historical Ticket Autopilot data, no other
/// agents' homes.
fn cmd_data(args: &[String]) -> OfficeResult<()> {
    match args.first().map(String::as_str) {
        Some("export") => cmd_data_export(args.get(1..).unwrap_or(&[])),
        _ => Err(OfficeError::Validation(
            "data needs `export --out <dir>`".into(),
        )),
    }
}

fn cmd_data_export(args: &[String]) -> OfficeResult<()> {
    let mut out_dir = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out_dir = args.get(i).cloned();
            }
            other => {
                return Err(OfficeError::Validation(format!(
                    "unknown export flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    let Some(out_dir) = out_dir else {
        return Err(OfficeError::Validation(
            "usage: viva data export --out <dir>".into(),
        ));
    };
    let out_path = std::path::PathBuf::from(out_dir);
    if out_path.exists() {
        return Err(OfficeError::Validation(format!(
            "refusing to overwrite an existing export directory: {}",
            out_path.display()
        )));
    }

    let home = viva::foundation::paths::viva_home(None);
    let db = database_path(&home);
    if !db.exists() {
        return Err(OfficeError::Validation(format!(
            "no office store at {} (run `viva init` first); nothing was created",
            db.display()
        )));
    }
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // Only after the store is verified do we create the output directory,
    // so a failed export leaves no litter behind.
    std::fs::create_dir_all(&out_path)?;

    let mut tables: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table'
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut manifest = serde_json::Map::new();
    manifest.insert(
        "exported_at".into(),
        serde_json::Value::String(viva::foundation::ids::utc_now()),
    );
    manifest.insert(
        "viva_home".into(),
        serde_json::Value::String(home.display().to_string()),
    );
    manifest.insert("read_only".into(), serde_json::Value::Bool(true));
    let mut table_rows = serde_json::Map::new();
    tables.sort();
    for table in &tables {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\""))?;
        let column_names: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
        let rows = stmt.query_map([], |row| {
            let mut obj = serde_json::Map::new();
            for (idx, column) in column_names.iter().enumerate() {
                let value = match row.get_ref(idx)? {
                    rusqlite::types::ValueRef::Null => serde_json::Value::Null,
                    rusqlite::types::ValueRef::Integer(v) => serde_json::Value::from(v),
                    rusqlite::types::ValueRef::Real(v) => serde_json::Value::from(v),
                    rusqlite::types::ValueRef::Text(text) => {
                        serde_json::Value::from(String::from_utf8_lossy(text).into_owned())
                    }
                    rusqlite::types::ValueRef::Blob(blob) => serde_json::Value::from(
                        blob.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                    ),
                };
                obj.insert(column.clone(), value);
            }
            Ok(serde_json::Value::Object(obj))
        })?;
        let mut list = Vec::new();
        for row in rows {
            list.push(row?);
        }
        table_rows.insert(table.clone(), serde_json::Value::from(list.len() as i64));
        let file = out_path.join(format!("{table}.json"));
        std::fs::write(&file, serde_json::to_string_pretty(&list)?)?;
    }
    manifest.insert("tables".into(), serde_json::Value::Object(table_rows));
    std::fs::write(
        out_path.join("manifest.json"),
        serde_json::to_string_pretty(&serde_json::Value::Object(manifest))?,
    )?;
    println!("exported {} tables to {}", tables.len(), out_path.display());
    Ok(())
}
