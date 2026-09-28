//! The `viva` binary: the office's executable entry point.
//!
//! V01 scope: initialize a private `VIVA_HOME`, prove the SQLite store with
//! real events across process restarts, and report state (`doctor`). The TUI
//! and supervision arrive with later milestones; this entry never claims
//! more than the office currently does.

use std::process::ExitCode;

use viva::foundation::events::{self, NewEvent};
use viva::foundation::paths::{database_path, ensure_private_dir, viva_home};
use viva::foundation::store::{KNOWN_DOMAINS, Store};
use viva::foundation::{OfficeError, OfficeResult, foundation_migrations};

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

    viva version"
    );
}

fn open_office_store() -> OfficeResult<Store> {
    let home = viva_home(None);
    ensure_private_dir(&home)?;
    Store::open(&database_path(&home), foundation_migrations())
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
            println!("office: shutdown accepted; owned terminals stopped, handoff persisted");
            Ok(())
        }
        _ => {
            print_usage();
            Err(OfficeError::Validation(
                "office needs start | status | dispatch | terminals | stop-terminal | result | shutdown"
                    .into(),
            ))
        }
    }
}

fn cmd_office_start(home: &std::path::Path) -> OfficeResult<()> {
    let host = viva::office::OfficeHost::open(home)?;
    println!(
        "office host {} active (pid {}) — home {}",
        host.shared().host_id,
        std::process::id(),
        home.display()
    );
    host.serve()?;
    println!("office host released the control channel");
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
