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
        // Bare `viva` IS the workbench: attach as a client of the resident
        // office server (auto-starting one when none runs). The TUI is a
        // detachable view now — `q` detaches, the server keeps every
        // terminal alive (ADR 0012 / issue #43). Usage: `viva help`.
        None => cmd_workbench(),
        Some("help" | "--help" | "-h") => {
            print_usage();
            Ok(())
        }
        Some("init") => cmd_init(),
        Some("doctor") => cmd_doctor(),
        Some("update") => viva::update::run(args.get(1..).unwrap_or(&[])),
        Some("event") => cmd_event(args.get(1..).unwrap_or(&[])),
        // The daily control plane is top-level — `viva start`, `viva
        // status`, ... — with no `office` namespace to type through.
        Some(
            "start" | "server" | "server-restart" | "status" | "dispatch" | "terminals"
            | "stop-terminal" | "result" | "shutdown" | "pause" | "resume" | "brief"
            | "create-task" | "grant" | "handoff",
        ) => cmd_office(args),
        Some("terminal") => cmd_terminal(args.get(1..).unwrap_or(&[])),
        Some("agent") => cmd_agent(args.get(1..).unwrap_or(&[])),
        Some("events") => cmd_events(args.get(1..).unwrap_or(&[])),
        Some("conversations") => cmd_conversations(args.get(1..).unwrap_or(&[])),
        Some("workbench") => cmd_workbench(),
        Some("data") => cmd_data(args.get(1..).unwrap_or(&[])),
        Some("tools") => cmd_tools(args.get(1..).unwrap_or(&[])),
        Some("memory") => cmd_memory(args.get(1..).unwrap_or(&[])),
        Some("workflow") => cmd_workflow(args.get(1..).unwrap_or(&[])),
        Some("maintenance") => cmd_maintenance(args.get(1..).unwrap_or(&[])),
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

/// `viva workbench` — the interactive parallel-development workbench, run
/// as a CLIENT of the resident office server. If no healthy server answers,
/// one is spawned detached first (its own process group, log in
/// `server.log`). `q` detaches the client; the server keeps every terminal
/// running until an explicit `viva shutdown` (ADR 0012, issue #43).
fn cmd_workbench() -> OfficeResult<()> {
    // Honest gate BEFORE any server is started: a workbench without a real
    // terminal cannot work, and auto-starting a resident server for a call
    // that must fail would leak a runtime.
    #[cfg(unix)]
    let stdin_is_tty = unsafe { libc::isatty(0) == 1 };
    #[cfg(not(unix))]
    let stdin_is_tty = true;
    if !stdin_is_tty {
        return Err(OfficeError::Validation(
            "viva workbench needs an interactive terminal (stdin is not a tty); \
             use `viva server` for the headless host and `viva terminal` for headless \
             terminals"
                .into(),
        ));
    }
    let home = viva::foundation::paths::viva_home(None);
    let client = viva::office::OfficeClient::ensure_server(&home)?;
    eprintln!(
        "workbench: attached to the resident office server — `q` detaches, \
         the server keeps running (`viva shutdown` stops it); home {}",
        home.display()
    );
    viva::tui::workbench::run_client(client)
}

/// `viva agent prompt|wait` and `viva events` (S5, issue #47): the
/// orchestration primitives, reachable from the CLI envelope. Agents call
/// these with --member/--grant so the socket gate can authorize them; a
/// bare human caller acts as the owner.
fn cmd_agent(args: &[String]) -> OfficeResult<()> {
    let home = viva::foundation::paths::viva_home(None);
    viva::foundation::paths::ensure_private_dir(&home)?;
    let mut client = viva::office::OfficeClient::ensure_server(&home)?;
    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // `args` arrives WITHOUT the `agent` subcommand; the subcommand word
    // itself carries no dashes and is skipped by the loop.
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if !flag.starts_with("--") {
            i += 1;
            continue;
        }
        i += 1;
        let value = args
            .get(i)
            .ok_or_else(|| OfficeError::Validation(format!("flag {flag} needs a value")))?;
        named.insert(flag.trim_start_matches("--").to_string(), value.clone());
        i += 1;
    }
    let get = |key: &str| named.get(key).cloned();
    let require = |key: &str| -> OfficeResult<String> {
        get(key).ok_or_else(|| OfficeError::Validation(format!("agent needs --{key}")))
    };
    let wait_timeout = get("timeout").and_then(|t| t.parse::<u64>().ok());
    let kind = match args.first().map(String::as_str) {
        Some("prompt") => viva::office::OfficeRequestKind::AgentPrompt {
            terminal_id: require("terminal")?,
            prompt: require("text")?,
        },
        Some("wait") => viva::office::OfficeRequestKind::AgentWait {
            terminal_id: require("terminal")?,
            status: require("status")?,
            timeout_secs: wait_timeout.unwrap_or(30),
        },
        _ => return Err(OfficeError::Validation("agent needs prompt | wait".into())),
    };
    let mut request = viva::office::new_request(kind);
    request.member = get("member");
    request.grant = get("grant");
    let response =
        if let viva::office::OfficeRequestKind::AgentWait { timeout_secs, .. } = &request.kind {
            client.call_with_timeout(
                request.kind.clone(),
                std::time::Duration::from_secs(timeout_secs + 15),
            )?
        } else {
            client.call_request(request)?
        };
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// `viva events [--since <seq>] [--limit <n>]` — the durable event feed
/// (S5): an orchestrator reconnects with its last seen seq.
fn cmd_events(args: &[String]) -> OfficeResult<()> {
    let (since, limit) = parse_events_args(args)?;
    let home = viva::foundation::paths::viva_home(None);
    let mut client = viva::office::OfficeClient::ensure_server(&home)?;
    let response = client.call(viva::office::OfficeRequestKind::EventsFeed {
        since_seq: since,
        limit,
    })?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// Pure parser for `viva events [--since <seq>] [--limit <n>]` so the
/// off-by-one regression (QA F2: flags start at index 0 — the subcommand
/// is already stripped) stays covered by a unit test.
fn parse_events_args(args: &[String]) -> OfficeResult<(u64, u32)> {
    let mut since = 0u64;
    let mut limit = 100u32;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                i += 1;
                since = args
                    .get(i)
                    .ok_or_else(|| OfficeError::Validation("--since needs a value".into()))?
                    .parse()
                    .map_err(|_| OfficeError::Validation("--since must be a number".into()))?;
            }
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .ok_or_else(|| OfficeError::Validation("--limit needs a value".into()))?
                    .parse()
                    .map_err(|_| OfficeError::Validation("--limit must be a number".into()))?;
            }
            other => {
                return Err(OfficeError::Validation(format!(
                    "unknown events flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    Ok((since, limit))
}

/// `viva terminal …` — headless terminal control over the resident server:
/// create fixed-size sessions, read snapshots, resize, stop. No UI needed;
/// a resident server is auto-started when none runs (issue #43).
fn cmd_terminal(args: &[String]) -> OfficeResult<()> {
    let home = viva::foundation::paths::viva_home(None);
    viva::foundation::paths::ensure_private_dir(&home)?;
    let mut client = viva::office::OfficeClient::ensure_server(&home)?;
    match args.first().map(String::as_str) {
        Some("create") => {
            let mut cols: u16 = 80;
            let mut rows: u16 = 24;
            let mut cwd: Option<String> = None;
            let mut purpose: Option<String> = None;
            let mut owner = "user_shell".to_string();
            let mut argv: Vec<String> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--cols" | "--rows" | "--cwd" | "--purpose" | "--owner" => {
                        let flag = args[i].trim_start_matches('-').to_string();
                        i += 1;
                        let value = args.get(i).ok_or_else(|| {
                            OfficeError::Validation(format!("flag --{flag} needs a value"))
                        })?;
                        match flag.as_str() {
                            "cols" => {
                                cols = value.parse().map_err(|_| {
                                    OfficeError::Validation("--cols must be a number".into())
                                })?
                            }
                            "rows" => {
                                rows = value.parse().map_err(|_| {
                                    OfficeError::Validation("--rows must be a number".into())
                                })?
                            }
                            "cwd" => cwd = Some(value.clone()),
                            "purpose" => purpose = Some(value.clone()),
                            "owner" => owner = value.clone(),
                            _ => unreachable!("flag names checked above"),
                        }
                    }
                    "--" => {
                        argv = args[i + 1..].to_vec();
                        break;
                    }
                    other => {
                        return Err(OfficeError::Validation(format!(
                            "unknown terminal create flag `{other}`"
                        )));
                    }
                }
                i += 1;
            }
            if argv.is_empty() {
                return Err(OfficeError::Validation(
                    "terminal create needs an explicit argv after `--`".into(),
                ));
            }
            let cwd = cwd
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let response = client.call(viva::office::OfficeRequestKind::TerminalCreate {
                argv,
                cwd: cwd.display().to_string(),
                env: vec![],
                cols,
                rows,
                purpose: purpose.unwrap_or_else(|| "headless session".into()),
                worktree_id: None,
                owner,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
        Some("snapshot") => {
            let terminal_id = args.get(1).ok_or_else(|| {
                OfficeError::Validation("usage: viva terminal snapshot <terminal-id>".into())
            })?;
            let response = client.call(viva::office::OfficeRequestKind::TerminalSnapshot {
                terminal_id: terminal_id.clone(),
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
        Some("resize") => {
            let terminal_id = args.get(1).ok_or_else(|| {
                OfficeError::Validation(
                    "usage: viva terminal resize <terminal-id> --cols N --rows N".into(),
                )
            })?;
            let mut named = std::collections::HashMap::new();
            let mut i = 2;
            while i < args.len() {
                let flag = args[i].as_str();
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| OfficeError::Validation(format!("flag {flag} needs a value")))?;
                named.insert(flag.to_string(), value.clone());
                i += 1;
            }
            let cols: u16 = named
                .get("--cols")
                .ok_or_else(|| OfficeError::Validation("resize needs --cols".into()))?
                .parse()
                .map_err(|_| OfficeError::Validation("--cols must be a number".into()))?;
            let rows: u16 = named
                .get("--rows")
                .ok_or_else(|| OfficeError::Validation("resize needs --rows".into()))?
                .parse()
                .map_err(|_| OfficeError::Validation("--rows must be a number".into()))?;
            let response = client.call(viva::office::OfficeRequestKind::TerminalResize {
                terminal_id: terminal_id.clone(),
                cols,
                rows,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
        _ => Err(OfficeError::Validation(
            "terminal needs create | snapshot | resize (stop via `viva stop-terminal`)".into(),
        )),
    }
}

fn print_usage() {
    println!(
        "viva — the Personal AI Office host

USAGE:
    viva
        Attach the interactive parallel-development workbench to the
        resident office server (one is spawned detached when none runs).
        `q` detaches the client; the server keeps every terminal running.

    viva help
        Print this usage text.

    viva init
        Create VIVA_HOME (default ~/.viva, override with $VIVA_HOME) and the
        office database, applying all registered migrations.

    viva doctor
        Report state root, database, schema versions and table counts.

    viva update [--check]
        Upgrade to the latest stable version (GitHub Release or global npm).
        --check only checks. Does not modify VIVA_HOME or restart servers.

    viva event add <domain> <kind> <subject-type> <subject-id> [payload-json]
        Append one office event (origin: cli).

    viva event list [limit]
        List the most recent office events (default 20).

    viva server
        Run the resident office server in the foreground (the same as
        `viva start`): owns the store, every terminal and the control
        channel until `viva shutdown` arrives. Clients may detach freely.

    viva status
        Office snapshot. Uses the live channel when a host is active, and
        falls back to offline store reads otherwise (read-only by design).

    viva dispatch --task <id> --member <id> --grant <id>
        --request-key <key> --cwd <dir> -- argv...
        Dispatch one task execution under a grant over the real channel.
        Idempotent by --request-key: a retry never spawns twice.

    viva terminals
        List terminals registered by the active host.

    viva terminal create [--cols N] [--rows N] [--cwd dir] [--purpose text] \
      [--owner user_shell|agent_cli|test_run] -- argv...
        Create one terminal in the resident server (headless — no UI
        needed). The server is auto-started when none runs.

    viva terminal snapshot <terminal-id>
        Dump one terminal's visible grid and scrollback (JSON).

    viva terminal resize <terminal-id> --cols N --rows N
        Resize one terminal's PTY.

    viva stop-terminal <terminal-id>
        Stop one terminal (its process group only).

    viva result <task-id>
        Show the results recorded for a task.

    viva create-task --goal <text> | viva grant --member <id>
        --task <id> --action <a> --mode <m> | viva brief <task-id> |
        viva handoff --task <id> --member <id> --summary <text>
        Task, grant, brief and handoff management.

    viva server-restart
        Upgrade path: hand every live terminal to a freshly spawned
        resumed server (fd-level live handoff) and exit gracefully.

    viva pause | viva resume
        Owner controls over the resident runtime: pause stops NEW dispatch
        and maintenance cycles (running executions are untouched); resume
        lifts the pause. The state persists across restarts.

    viva shutdown
        Ask the resident server to stop dispatch, stop owned terminals,
        persist the handoff and release the channel. Set VIVA_CLOSE_POLICY
        =pause to leave the next server paused.

    viva agent prompt --terminal <id> --text <text> [--member <id> --grant <id>]
    viva agent wait --terminal <id> --status <working|blocked|done|idle> \
        [--timeout <secs>] [--member <id> --grant <id>]
        The orchestration primitives (issue #47): prompt an agent's
        terminal, or wait server-side for its reported status.

    viva events [--since <seq>] [--limit <n>]
        The durable office event feed: reconnect with your last seen seq
        and miss nothing.

    viva conversations <create|fork|rename|set-native|attach-task|detach-task|archive|tree|handoff> [flags]
        Conversation metadata (office-owned tree; the harness owns the transcript).

    viva workbench
        Same as bare `viva`: attach the interactive workbench to the
        resident server.

    viva data export --out <dir>
        Read-only export (dump) of every fact table in this VIVA_HOME to
        JSON files plus a manifest. RAW/UNREDACTED sensitive asset export:
        private files only, do not publish. Never touches worktrees or
        historical data directories.

    viva tools computer lane
        List all foreground holders without reconciliation; opening the
        office may still apply pending schema migrations.

    viva tools computer release-lease --task <id> --acquired-at <timestamp> \
      --member <id> --grant <id> --reason <text> --confirm
        Release only an unknown-PID legacy holder after separately confirming
        its original action stopped. Needs a live owner-issued task grant;
        --confirm is an assertion, not proof of OS/process state.

    viva tools computer audit
        Probe the reused computer tools (orca computer, osascript) and
        record what is really available (read-only).

    viva tools computer smoke --task <id> --member <id> --grant <id>
        Run the two controlled smoke tasks (browser + native app) through
        the locate → act → verify chain under the given task-scoped grant.
        Read-only inspections; evidence rows are recorded either way.

    viva memory search --member <id> [--project <id>] --query <text>
        Search the linked external memory for one member's scope
        (read-only). Unavailable stores say so; unclaimable hits are
        counted, not silently mixed in.

    viva memory remember --member <id> [--project <id>] --content <text>
                         --source <provenance> [--category <c>] [--tags <t>]
        Write one fact through the real provider with mandatory provenance.

    viva memory archive|restore --fact <id> --reason <text>
        Exit paths over the office link layer. Nothing here deletes.

    viva memory status
        Report which store/checkout the office is wired to — from a real
        adapter round trip, not file existence.

    viva workflow register --builtin <delivery|read-only-review> | --file <config.json>
        Register a workflow configuration (data: steps, roles, evidence
        requirements, retry budgets, transitions).

    viva workflow start-run --task <id> --config <name> --cwd <dir>
        Start a run bound to the REAL git head of --cwd (resolved via
        rev-parse, never self-reported).

    viva workflow record --run <id> --member <id> --pass|--fail --evidence <json>
        Record one attempt of the current step. Evidence is a JSON array
        of objects with kind, note and optional head_sha, grant_id and
        references fields.

    viva workflow status|history --run <id>
        Progress (with the owning task's own status shown beside it) and
        the full recoverable record.

    viva workflow resume --run <id> --cwd <dir> | abort --run <id> --reason <text>
        Resume a paused run (optionally rebinding to a new real head) or
        abort it with a reason.

    viva maintenance review-knowledge [--stale-days <n>]
    viva maintenance review-worktrees | review-repo --repo <dir>
    viva maintenance proposals
    viva maintenance execute|dismiss --proposal <id> --grant <id> [--note <text>]
        Run one bounded maintenance window (session opened and closed by
        this command — no daemon). Execution and dismissal both require a
        live maintain_knowledge grant; worktree and cleanliness findings
        are human-review only.

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
        Some("server" | "start") => {
            let resume = args.get(1).map(String::as_str) == Some("--resume");
            cmd_office_start(&home, resume)
        }
        Some("server-restart") => cmd_server_restart(&home),
        Some("status") => cmd_office_status(&home),
        Some("dispatch") => cmd_office_dispatch(&home, args.get(1..).unwrap_or(&[])),
        Some("terminals") => cmd_office_query(&home, viva::office::OfficeRequestKind::TerminalList),
        Some("stop-terminal") => {
            let terminal_id = args.get(1).ok_or_else(|| {
                OfficeError::Validation("usage: viva stop-terminal <terminal-id>".into())
            })?;
            cmd_office_query(
                &home,
                viva::office::OfficeRequestKind::TerminalStop {
                    terminal_id: terminal_id.clone(),
                },
            )
        }
        Some("result") => {
            let task_id = args
                .get(1)
                .ok_or_else(|| OfficeError::Validation("usage: viva result <task-id>".into()))?;
            cmd_office_query(
                &home,
                viva::office::OfficeRequestKind::TaskResults {
                    task_id: task_id.clone(),
                },
            )
        }
        Some("shutdown") => {
            let close_policy = std::env::var("VIVA_CLOSE_POLICY")
                .ok()
                .filter(|p| p == "pause");
            cmd_office_query(
                &home,
                viva::office::OfficeRequestKind::Shutdown { close_policy },
            )?;
            // Human commentary goes to stderr; stdout stays pure JSON for
            // callers that parse it (the extension envelope, tooling).
            eprintln!("office: shutdown accepted; owned terminals stopped, handoff persisted");
            Ok(())
        }
        Some("pause") => {
            cmd_office_query(&home, viva::office::OfficeRequestKind::Pause)?;
            eprintln!(
                "office: paused - no new dispatch, no maintenance; running executions keep running"
            );
            Ok(())
        }
        Some("resume") => {
            cmd_office_query(&home, viva::office::OfficeRequestKind::Resume)?;
            eprintln!("office: resumed - dispatch and maintenance are live again");
            Ok(())
        }
        Some("brief") => cmd_office_brief(args.get(1..).unwrap_or(&[])),
        Some("create-task") => cmd_office_create_task(args.get(1..).unwrap_or(&[])),
        Some("grant") => cmd_office_grant(args.get(1..).unwrap_or(&[])),
        Some("handoff") => cmd_office_handoff(&home, args.get(1..).unwrap_or(&[])),
        _ => {
            print_usage();
            Err(OfficeError::Validation(
                "viva needs start | status | dispatch | terminals | stop-terminal | result | brief | create-task | grant | handoff | shutdown"
                    .into(),
            ))
        }
    }
}

/// `viva brief <task-id>` — offline, read-only task brief from the
/// store (the extension's context-entry endpoint).
fn cmd_office_brief(args: &[String]) -> OfficeResult<()> {
    let task_id = args
        .first()
        .ok_or_else(|| OfficeError::Validation("usage: viva brief <task-id>".into()))?;
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

/// `viva handoff --task <id> --member <id> --summary <text>` —
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
            "usage: viva handoff --task <id> --member <id> --summary <text>".into(),
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

fn cmd_office_start(home: &std::path::Path, resume: bool) -> OfficeResult<()> {
    if resume {
        // Live-handoff resume (S3): adopt the transferred terminals and
        // serve. Blocks until this host is shut down.
        eprintln!(
            "office host (pid {}) resuming from live handoff — home {}",
            std::process::id(),
            home.display()
        );
        viva::office::resume_server(home)?;
        eprintln!("resumed host released the control channel");
        return Ok(());
    }
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

/// `viva server-restart` — upgrade path: the running server transfers its
/// live terminals to a freshly spawned resumed server (S3, issue #45).
fn cmd_server_restart(home: &std::path::Path) -> OfficeResult<()> {
    let new_pid = viva::office::restart_server(home)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "restarted": true,
            "new_pid": new_pid,
        }))?
    );
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
            "usage: viva dispatch --task <id> --member <id> --grant <id> \
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
            let (handoff, duplicate) = registry.record_handoff_checked(
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
            // `duplicate: true` = an identical earlier handoff was reused.
            let mut value = serde_json::to_value(&handoff)?;
            value["duplicate"] = serde_json::Value::Bool(duplicate);
            println!("{}", serde_json::to_string_pretty(&value)?);
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

/// `viva tools computer …` (F03) — inspect/recover the foreground lane,
/// audit reused tools, or run two controlled smoke tasks. Lease recovery
/// needs an explicit task grant and operator assertion; smoke only inspects
/// real windows. Input actions are never exposed to chat through this CLI.
fn cmd_tools(args: &[String]) -> OfficeResult<()> {
    use std::str::FromStr as _;

    // args[0] is the tool family (`computer`); args[1..] its command.
    if args.first().map(String::as_str) != Some("computer") {
        return Err(OfficeError::Validation(
            "usage: viva tools computer lane | audit | smoke --task <id> \
             --member <id> --grant <id> | release-lease --task <id> --acquired-at <time> \
             --member <id> --grant <id> --reason <text> --confirm"
                .into(),
        ));
    }
    match args.get(1).map(String::as_str) {
        Some("lane") => {
            if args.len() != 2 {
                return Err(OfficeError::Validation("lane accepts no flags".into()));
            }
            let store = open_office_store()?;
            let rows = viva::tools::computer::ForegroundCoordinator::new(&store).leases()?;
            println!("{}", serde_json::to_string_pretty(&rows)?);
            Ok(())
        }
        Some("release-lease") => {
            let mut named = std::collections::HashMap::new();
            let mut confirm = false;
            let mut i = 2;
            while i < args.len() {
                let flag = args[i].as_str();
                if flag == "--confirm" {
                    if confirm {
                        return Err(OfficeError::Validation("duplicate --confirm".into()));
                    }
                    confirm = true;
                    i += 1;
                    continue;
                }
                if !["--task", "--acquired-at", "--member", "--grant", "--reason"].contains(&flag)
                    || named.contains_key(flag)
                {
                    return Err(OfficeError::Validation(format!(
                        "unexpected or duplicate release-lease flag `{flag}`"
                    )));
                }
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| OfficeError::Validation(format!("flag {flag} needs a value")))?;
                named.insert(flag, value.as_str());
                i += 1;
            }
            if !confirm {
                return Err(OfficeError::Validation(
                    "release-lease requires --confirm after checking the original action stopped"
                        .into(),
                ));
            }
            let require = |flag| {
                named
                    .get(flag)
                    .copied()
                    .ok_or_else(|| OfficeError::Validation(format!("release-lease needs {flag}")))
            };
            let task = viva::foundation::ids::TaskId::from_str(require("--task")?)?;
            let member = viva::foundation::ids::MemberId::from_str(require("--member")?)?;
            let grant = viva::foundation::ids::GrantId::from_str(require("--grant")?)?;
            let store = open_office_store()?;
            let authority = viva::authority::AuthorityEngine::new(&store);
            let actor = viva::authority::Actor::Member {
                member,
                grant: Some(grant),
            };
            viva::tools::computer::ForegroundCoordinator::new(&store).release_unknown_lease(
                &task,
                require("--acquired-at")?,
                require("--reason")?,
                &actor,
                &authority,
            )?;
            println!("lease released for {} (operator assertion recorded)", task);
            Ok(())
        }
        Some("audit") => {
            let store = open_office_store()?;
            let engine = viva::tools::computer::ComputerEngine::new(&store);
            for report in engine.audit()? {
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            Ok(())
        }
        Some("smoke") => {
            let mut task_id = None;
            let mut member_id = None;
            let mut grant_id = None;
            let mut i = 2;
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
                    "--grant" => grant_id = Some(value),
                    other => {
                        return Err(OfficeError::Validation(format!(
                            "unknown smoke flag `{other}`"
                        )));
                    }
                }
                i += 1;
            }
            let (Some(task_id), Some(member_id), Some(grant_id)) = (task_id, member_id, grant_id)
            else {
                return Err(OfficeError::Validation(
                    "usage: viva tools computer smoke --task <id> --member <id> --grant <id>"
                        .into(),
                ));
            };
            let task_id = viva::foundation::ids::TaskId::from_str(&task_id)?;
            let member_id = viva::foundation::ids::MemberId::from_str(&member_id)?;
            let grant_id = viva::foundation::ids::GrantId::from_str(&grant_id)?;

            let store = open_office_store()?;
            // Smoke evidence binds to a real office task, not an invented one.
            viva::tasks::TaskRegistry::new(&store).require_task(&task_id)?;
            let authority = viva::authority::AuthorityEngine::new(&store);
            let actor = viva::authority::Actor::Member {
                member: member_id,
                grant: Some(grant_id),
            };
            let engine = viva::tools::computer::ComputerEngine::new(&store);
            let reports = engine.audit()?;
            let mut executed = 0;
            let mut results = serde_json::Map::new();
            for (name, spec) in viva::tools::computer::smoke_specs() {
                let record = engine.execute(&authority, &actor, &task_id, &spec)?;
                if record.state == viva::tools::computer::ActionState::Executed {
                    executed += 1;
                }
                // The printed evidence SUMMARIZES the tool snapshots instead
                // of dumping them: `orca` state output inventories every
                // running application (names, bundle ids, pids) — machine-
                // private content that must not flow into shareable files
                // (QA finding N1). The full snapshots stay in this
                // VIVA_HOME's private store (computer_actions); the
                // summary keeps the verification facts.
                results.insert(name, summarize_action(&record)?);
            }
            results.insert("capabilities".into(), serde_json::to_value(&reports)?);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::Value::Object(results))?
            );
            if executed < 2 {
                return Err(OfficeError::Validation(format!(
                    "smoke incomplete: {executed}/2 tasks executed — see the evidence \
                     above for the honest state"
                )));
            }
            Ok(())
        }
        other => Err(OfficeError::Validation(format!(
            "unknown tools computer command `{}`; expected `lane`, `release-lease`, `audit` or `smoke`",
            other.unwrap_or("<missing>")
        ))),
    }
}

/// A shareable summary of one computer action: verdict and verification
/// facts, with the raw tool snapshots replaced by size + fingerprint.
fn summarize_action(
    record: &viva::tools::computer::ActionRecord,
) -> OfficeResult<serde_json::Value> {
    let digest = |text: &Option<String>| -> serde_json::Value {
        match text {
            None => serde_json::Value::Null,
            Some(text) => serde_json::json!({
                "redacted": true,
                "bytes": text.len(),
                "fingerprint": content_fingerprint(text),
            }),
        }
    };
    Ok(serde_json::json!({
        "state": record.state,
        "target": record.target,
        "reason": record.reason,
        "foreground": record.foreground,
        "pre_state": digest(&record.pre_state),
        "action_output": digest(&record.action_output),
        "post_state": digest(&record.post_state),
        "recorded_at": record.recorded_at,
    }))
}

/// A stable local fingerprint (FNV-1a, hex) so two runs' snapshots can be
/// told apart without carrying their content. Not cryptographic.
fn content_fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// `viva create-task --goal <text>` — open one office task. This is
/// an owner-side CLI: whoever runs the binary creates the task in their
/// own VIVA_HOME.
fn cmd_office_create_task(args: &[String]) -> OfficeResult<()> {
    let mut goal = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--goal" => {
                i += 1;
                goal = args.get(i).cloned();
            }
            other => {
                return Err(OfficeError::Validation(format!(
                    "unknown create-task flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    let goal = goal
        .ok_or_else(|| OfficeError::Validation("usage: viva create-task --goal <text>".into()))?;
    let store = open_office_store()?;
    let task =
        viva::tasks::TaskRegistry::new(&store).create_task(goal, vec![], None, None, None)?;
    println!("{}", serde_json::to_string_pretty(&task)?);
    Ok(())
}

/// `viva grant --member <id> --task <id> --action <a> --mode <m>
/// [--expires <rfc3339>]` — an owner-side root grant (the CLI runner IS
/// the user; the engine records issuer=user). Protected actions are
/// refused by the engine itself.
fn cmd_office_grant(args: &[String]) -> OfficeResult<()> {
    use std::str::FromStr as _;
    let mut member = None;
    let mut task = None;
    let mut action = None;
    let mut mode = None;
    let mut expires = None;
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        i += 1;
        let value = args
            .get(i)
            .cloned()
            .ok_or_else(|| OfficeError::Validation(format!("flag {flag} needs a value")))?;
        match flag {
            "--member" => member = Some(value),
            "--task" => task = Some(value),
            "--action" => action = Some(value),
            "--mode" => mode = Some(value),
            "--expires" => expires = Some(value),
            other => {
                return Err(OfficeError::Validation(format!(
                    "unknown grant flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    let member = member
        .map(|m| viva::foundation::ids::MemberId::from_str(&m))
        .transpose()?;
    let task = task
        .map(|t| viva::foundation::ids::TaskId::from_str(&t))
        .transpose()?;
    let action = action.ok_or_else(|| {
        OfficeError::Validation(
            "usage: viva grant --member <id> --task <id> --action <a> --mode <m> \
             [--expires <rfc3339>]"
                .into(),
        )
    })?;
    let mode = viva::authority::GrantMode::from_str_value(&mode.ok_or_else(|| {
        OfficeError::Validation(
            "--mode is required (READ | PROPOSE | ACT_WITH_APPROVAL | ACT_AUTONOMOUSLY)".into(),
        )
    })?)?;
    let store = open_office_store()?;
    let engine = viva::authority::AuthorityEngine::new(&store);
    let grant = engine.issue_root_grant(member, task, vec![action], mode, expires)?;
    println!("{}", serde_json::to_string_pretty(&grant)?);
    Ok(())
}

/// `viva memory …` (F04) — the office's namespace/audit layer over the
/// real external memory. Search/remember/archive all go through the
/// office link layer: member scoping, provenance, usage evidence, and
/// recoverable exit (no delete). The adapter subprocess always receives
/// an explicit db path.
fn cmd_memory(args: &[String]) -> OfficeResult<()> {
    use std::str::FromStr as _;

    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut i = 1; // args[0] is the subcommand
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
        get(key).ok_or_else(|| OfficeError::Validation(format!("memory needs --{key}")))
    };

    let store = open_office_store()?;
    let service = viva::memory::MemoryService::new(&store);
    match args.first().map(String::as_str) {
        Some("search") => {
            let viewer = viva::foundation::ids::MemberId::from_str(&require("--member")?)?;
            let project = get("--project")
                .map(|p| viva::foundation::ids::ProjectId::from_str(&p))
                .transpose()?;
            let query = require("--query")?;
            match service.search(&viewer, project.as_ref(), &query, 16 * 1024)? {
                viva::memory::MemorySearch::Fetched {
                    facts,
                    hidden_unclaimable,
                } => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "state": "fetched",
                            "facts": facts,
                            "hidden_unclaimable": hidden_unclaimable,
                        }))?
                    );
                }
                viva::memory::MemorySearch::Unavailable { reason } => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "state": "unavailable",
                            "reason": reason,
                        }))?
                    );
                }
            }
            Ok(())
        }
        Some("probe") => {
            let viewer = viva::foundation::ids::MemberId::from_str(&require("--member")?)?;
            let project = get("--project")
                .map(|p| viva::foundation::ids::ProjectId::from_str(&p))
                .transpose()?;
            match service.probe(&viewer, project.as_ref(), &require("--entity")?, 16 * 1024)? {
                viva::memory::MemorySearch::Fetched {
                    facts,
                    hidden_unclaimable,
                } => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "state": "fetched",
                            "facts": facts,
                            "hidden_unclaimable": hidden_unclaimable,
                        }))?
                    );
                }
                viva::memory::MemorySearch::Unavailable { reason } => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "state": "unavailable",
                            "reason": reason,
                        }))?
                    );
                }
            }
            Ok(())
        }
        Some("link") => {
            let member_id = viva::foundation::ids::MemberId::from_str(&require("--member")?)?;
            let project = get("--project")
                .map(|p| viva::foundation::ids::ProjectId::from_str(&p))
                .transpose()?;
            let fact_id: i64 = require("--fact")?.parse().map_err(|_| {
                OfficeError::Validation("--fact must be the external fact's integer id".into())
            })?;
            let link =
                service.link_fact(fact_id, &member_id, project.as_ref(), &require("--source")?)?;
            println!("{}", serde_json::to_string_pretty(&link)?);
            Ok(())
        }
        Some("remember") => {
            let member_id = viva::foundation::ids::MemberId::from_str(&require("--member")?)?;
            let project = get("--project")
                .map(|p| viva::foundation::ids::ProjectId::from_str(&p))
                .transpose()?;
            let link = service.remember(
                &member_id,
                project.as_ref(),
                &require("--content")?,
                &require("--source")?,
                &get("--category").unwrap_or_else(|| "general".into()),
                &get("--tags").unwrap_or_default(),
            )?;
            println!("{}", serde_json::to_string_pretty(&link)?);
            Ok(())
        }
        Some(cmd @ ("archive" | "restore")) => {
            let fact_id: i64 = require("--fact")?.parse().map_err(|_| {
                OfficeError::Validation("--fact must be the external fact's integer id".into())
            })?;
            let member_id = viva::foundation::ids::MemberId::from_str(&require("--member")?)?;
            let grant = get("--grant")
                .map(|g| viva::foundation::ids::GrantId::from_str(&g))
                .transpose()?;
            let actor = viva::authority::Actor::Member {
                member: member_id,
                grant,
            };
            let authority = viva::authority::AuthorityEngine::new(&store);
            let link = if cmd == "archive" {
                service.archive(fact_id, &require("--reason")?, &actor, &authority)?
            } else {
                service.restore(fact_id, &require("--reason")?, &actor, &authority)?
            };
            println!("{}", serde_json::to_string_pretty(&link)?);
            Ok(())
        }
        Some("status") => {
            let config = viva::memory::AdapterConfig::from_env();
            // A real probe, not a file-existence promise: run the adapter's
            // status command (read-only) and report what actually answered.
            let probe = match service.probe_adapter_status() {
                Ok(answer) => answer,
                Err(err) => serde_json::json!({
                    "ok": false,
                    "state": "unavailable",
                    "error": err.to_string(),
                }),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "python": config.python.display().to_string(),
                    "adapter": config.adapter_script.display().to_string(),
                    "adapter_present": config.adapter_script.is_file(),
                    "agent_dir": config.agent_dir.display().to_string(),
                    "agent_dir_present": config.agent_dir.is_dir(),
                    "db_path": config.db_path.display().to_string(),
                    "probe": probe,
                }))?
            );
            Ok(())
        }
        _ => Err(OfficeError::Validation(
            "memory needs search | probe | link | remember | archive | restore | status".into(),
        )),
    }
}

/// `viva workflow …` (F01) — the reachable entry for delivery workflows.
/// The run head is anchored to the REAL git head of the given directory
/// (rev-parse through the office's git runner), never self-reported.
/// `status` always shows the owning task's own status beside the run's
/// process progress: the run records process facts, the task keeps the
/// delivery state, and completion stays an appended task outcome.
fn cmd_workflow(args: &[String]) -> OfficeResult<()> {
    use std::str::FromStr as _;

    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut flags: Vec<String> = Vec::new();
    let mut i = 1; // args[0] is the subcommand
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--pass" || arg == "--fail" {
            // Boolean outcome flags take no value.
            flags.push(arg.to_string());
        } else if let Some(flag) = arg.strip_prefix("--") {
            i += 1;
            let value = args
                .get(i)
                .cloned()
                .ok_or_else(|| OfficeError::Validation(format!("flag {arg} needs a value")))?;
            named.insert(flag.to_string(), value);
        } else {
            flags.push(arg.to_string());
        }
        i += 1;
    }
    let get = |key: &str| named.get(key).cloned();
    let require = |key: &str| -> OfficeResult<String> {
        get(key).ok_or_else(|| OfficeError::Validation(format!("workflow needs --{key}")))
    };

    let store = open_office_store()?;
    let engine = viva::workflows::WorkflowEngine::new(&store);
    let resolve_head = |cwd: &str| -> OfficeResult<String> {
        let runner = viva::git::cli::CliRunner::default();
        let path = std::path::Path::new(cwd);
        // A SHA is only an honest content anchor when tracked files match
        // it. Apply the same gate at start and every resume (QA R3-N8).
        let status = runner
            .run(
                "git",
                path,
                &["status", "--porcelain", "--untracked-files=no"],
            )
            .map_err(|failure| {
                OfficeError::Validation(format!("git status failed in {cwd}: {failure}"))
            })?;
        if !status.success() || !status.stdout.trim().is_empty() {
            return Err(OfficeError::Validation(format!(
                "refusing to anchor a run in a dirty worktree ({cwd}) — uncommitted \
                 tracked changes mean \"verified at this head\" would be a claim about \
                 content the SHA does not name; commit first"
            )));
        }
        viva::git::cli::head_sha(&runner, path)
    };
    match args.first().map(String::as_str) {
        Some("register") => {
            let config = if let Some(builtin) = get("builtin") {
                match builtin.as_str() {
                    "delivery" => viva::workflows::WorkflowConfig::delivery_default(),
                    "read-only-review" => {
                        viva::workflows::WorkflowConfig::read_only_review_default()
                    }
                    other => {
                        return Err(OfficeError::Validation(format!(
                            "unknown builtin config `{other}` (delivery | read-only-review)"
                        )));
                    }
                }
            } else {
                let file = require("file")?;
                let text = std::fs::read_to_string(&file)?;
                serde_json::from_str(&text)?
            };
            let config_id = engine.register_config(&config)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "config_id": config_id,
                    "name": config.name,
                    "steps": config.steps.len(),
                }))?
            );
            Ok(())
        }
        Some("start-run") => {
            let task_id = viva::foundation::ids::TaskId::from_str(&require("task")?)?;
            let cwd = require("cwd")?;
            let head = resolve_head(&cwd)?;
            let run = engine.start_run(&task_id, &require("config")?, head)?;
            println!("{}", serde_json::to_string_pretty(&run)?);
            Ok(())
        }
        Some("record") => {
            let member = viva::foundation::ids::MemberId::from_str(&require("member")?)?;
            let passed = match flags.first().map(String::as_str) {
                Some("--pass") => true,
                Some("--fail") => false,
                other => {
                    return Err(OfficeError::Validation(format!(
                        "record needs --pass or --fail (got {other:?})"
                    )));
                }
            };
            let evidence_json = require("evidence")?;
            let evidence: Vec<viva::workflows::StepEvidence> =
                serde_json::from_str(&evidence_json)?;
            let advance = engine.record_step_result(&require("run")?, &member, passed, evidence)?;
            println!("{}", serde_json::to_string_pretty(&advance)?);
            Ok(())
        }
        Some("resume") => {
            // A paused run may only resume against a clean checkout, even
            // when its head has not changed. No unverified no-cwd bypass.
            let new_head = resolve_head(&require("cwd")?)?;
            let run = engine.resume_paused(&require("run")?, Some(new_head))?;
            println!("{}", serde_json::to_string_pretty(&run)?);
            Ok(())
        }
        Some("abort") => {
            engine.abort_run(&require("run")?, require("reason")?)?;
            println!("aborted");
            Ok(())
        }
        Some("status" | "history") => {
            let history = engine.run_history(&require("run")?)?;
            if args.first().map(String::as_str) == Some("status") {
                let tasks = viva::tasks::TaskRegistry::new(&store);
                let task = tasks.require_task(&history.run.task_id)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "run": history.run,
                        // Delivery state lives on the task; the run's
                        // status is process progress only.
                        "task_status": task.status.as_str(),
                        "current_step": history.config.steps.get(history.run.current_step),
                    }))?
                );
            } else {
                println!("{}", serde_json::to_string_pretty(&history)?);
            }
            Ok(())
        }
        _ => Err(OfficeError::Validation(
            "workflow needs register | start-run | record | resume | abort | status | history"
                .into(),
        )),
    }
}

/// `viva maintenance …` (F02) — the reachable entry for runtime knowledge
/// review and repository maintenance. Each review command opens one
/// bounded session and closes it: maintenance runs only inside such a
/// window, never as a daemon.
fn cmd_maintenance(args: &[String]) -> OfficeResult<()> {
    use std::str::FromStr as _;

    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
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
    let get = |key: &str| named.get(&format!("--{key}")).cloned();
    let require = |key: &str| -> OfficeResult<String> {
        get(key).ok_or_else(|| OfficeError::Validation(format!("maintenance needs --{key}")))
    };
    let print_report = |report: &viva::maintenance::ScanReport| -> OfficeResult<()> {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "scan_id": report.scan_id,
                "kind": report.kind,
                "proposed": report.proposed,
                "already_proposed": report.already_proposed,
                "notes": report.notes,
            }))?
        );
        Ok(())
    };

    let store = open_office_store()?;
    // S6: an explicitly paused office runs no maintenance windows (the
    // pause persists, so this holds with no live host either).
    viva::office::assert_maintenance_allowed(&store)?;
    let maintenance = viva::maintenance::MaintenanceService::new(&store);
    match args.first().map(String::as_str) {
        Some("review-knowledge") => {
            let stale_days: u32 = match get("stale-days") {
                Some(text) => text
                    .parse()
                    .map_err(|_| {
                        OfficeError::Validation("--stale-days must be a number".to_string())
                    })?,
                None => 180,
            };
            let registry = viva::knowledge::KnowledgeRegistry::new(&store);
            let session = viva::maintenance::MaintenanceSession::open(&store)?;
            // The window closes even when the scan errors — no leaked rows.
            let outcome = maintenance.review_knowledge(&session, &registry, stale_days);
            session.end(&store)?;
            print_report(&outcome?)
        }
        Some("review-worktrees") => {
            let protected = viva::git::worktrees::ProtectedRefs::new(Vec::new());
            let service = viva::git::worktrees::WorktreeService::new(&store, protected);
            let records = service.all_records()?;
            let session = viva::maintenance::MaintenanceSession::open(&store)?;
            let outcome = maintenance.review_worktrees(&session, &records);
            session.end(&store)?;
            print_report(&outcome?)
        }
        Some("review-repo") => {
            let repo = require("repo")?;
            let session = viva::maintenance::MaintenanceSession::open(&store)?;
            let outcome = maintenance.review_repo(&session, std::path::Path::new(&repo));
            session.end(&store)?;
            print_report(&outcome?)
        }
        Some("proposals") => {
            println!(
                "{}",
                serde_json::to_string_pretty(&maintenance.open_proposals()?)?
            );
            Ok(())
        }
        Some(cmd @ ("execute" | "dismiss")) => {
            let actor = viva::authority::Actor::Member {
                member: viva::foundation::ids::MemberId::from_str(&require("member")?)?,
                grant: Some(viva::foundation::ids::GrantId::from_str(&require("grant")?)?),
            };
            let authority = viva::authority::AuthorityEngine::new(&store);
            let note = get("note").unwrap_or_default();
            let proposal = if cmd == "execute" {
                let registry = viva::knowledge::KnowledgeRegistry::new(&store);
                maintenance.execute_proposal(
                    &require("proposal")?,
                    &actor,
                    &authority,
                    &registry,
                    note,
                )?
            } else {
                maintenance.dismiss_proposal(&require("proposal")?, &actor, &authority, note)?
            };
            println!("{}", serde_json::to_string_pretty(&proposal)?);
            Ok(())
        }
        _ => Err(OfficeError::Validation(
            "maintenance needs review-knowledge | review-worktrees | review-repo | proposals | execute | dismiss"
                .into(),
        )),
    }
}

/// `viva data export --out <dir>` — read-only asset inventory and dump
/// (V13): every user table in this VIVA_HOME goes to one JSON file per
/// table plus a manifest. This is an export for inspection and preserve,
/// not a restorable backup: there is no schema migration or import path.
/// The database is opened READ-ONLY and nothing outside the store is
/// touched: no worktrees, no historical Ticket Autopilot data, no other
/// agents' homes. This is a RAW asset export, not a privacy-safe artifact;
/// owner-only output permissions and manifest flags make that explicit.
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
    // Create a new private directory, never widen or overwrite an existing
    // location. The export keeps raw facts for preservation, including
    // machine-private audit data; it is not safe to commit/share.
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(&out_path)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&out_path)?;
    eprintln!("warning: raw, unredacted office export; keep this private and do not publish it");

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
    manifest.insert("redacted".into(), serde_json::Value::Bool(false));
    manifest.insert(
        "contains_sensitive_data".into(),
        serde_json::Value::Bool(true),
    );
    manifest.insert(
        "safety_notice".into(),
        serde_json::Value::String(
            "RAW asset export; may contain credentials and private computer evidence; do not publish".into(),
        ),
    );
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
        write_private_export_json(&file, &serde_json::Value::Array(list))?;
    }
    manifest.insert("tables".into(), serde_json::Value::Object(table_rows));
    write_private_export_json(
        &out_path.join("manifest.json"),
        &serde_json::Value::Object(manifest),
    )?;
    println!("exported {} tables to {}", tables.len(), out_path.display());
    Ok(())
}

fn write_private_export_json(
    path: &std::path::Path,
    value: &serde_json::Value,
) -> OfficeResult<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    serde_json::to_writer_pretty(file, value)?;
    Ok(())
}

#[cfg(test)]
mod cli_arg_tests {
    use super::*;

    #[test]
    fn events_flags_parse_from_index_zero_qa_f2() {
        let args: Vec<String> = ["--since", "7", "--limit", "25"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (since, limit) = parse_events_args(&args).expect("parse");
        assert_eq!(since, 7);
        assert_eq!(limit, 25);
        // Defaults when no flags are given.
        let (since, limit) = parse_events_args(&[]).expect("parse defaults");
        assert_eq!((since, limit), (0, 100));
        // Unknown flags still fail loudly.
        assert!(parse_events_args(&["--bogus".to_string()]).is_err());
    }
}
