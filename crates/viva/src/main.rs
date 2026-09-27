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
