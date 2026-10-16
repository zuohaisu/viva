//! The `viva` binary: Viva's executable entry point.
//!
//! V01 scope: initialize a private `VIVA_HOME`, prove the SQLite store with
//! real events across process restarts, and report state (`doctor`). The TUI
//! and supervision arrive with later milestones; this entry never claims
//! more than Viva currently does.

use std::process::ExitCode;
use std::str::FromStr as _;

use viva::foundation::events::{self, NewEvent};
use viva::foundation::paths::{database_path, ensure_private_dir, viva_home};
use viva::foundation::store::{KNOWN_DOMAINS, Store};
use viva::foundation::{Error, Result};

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

fn run(args: &[String]) -> Result<()> {
    let (machine, args) = peel_machine_flag(args)?;
    let _machine = if let Some(id) = machine {
        Some(arm_machine(&id, &args)?)
    } else {
        None
    };
    match args.first().map(String::as_str) {
        // Bare `viva` IS the workbench: attach as a client of the resident
        // server (auto-starting one when none runs). The TUI is a
        // detachable view now — ctrl+b q detaches, the server keeps every
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
        // status`, ... — with no namespace to type through.
        Some(
            "start" | "server" | "server-restart" | "status" | "dispatch" | "terminals"
            | "stop-terminal" | "result" | "shutdown" | "pause" | "resume" | "brief"
            | "create-task" | "create-project" | "grant" | "handoff",
        ) => cmd_control(&args),
        Some("workspace") => cmd_workspace(args.get(1..).unwrap_or(&[])),
        Some("machine") => cmd_machine(args.get(1..).unwrap_or(&[])),
        Some("open") => {
            // `viva open <path>` = open the workspace, then the workbench.
            // The tty gate comes first so a failing call never leaves a
            // resident server (or a half-opened workspace) behind.
            require_interactive_terminal()?;
            let path = args
                .get(1)
                .ok_or_else(|| Error::Validation("open needs a path".into()))?;
            let reply = workspace_call(&["open".into(), path.clone()])?;
            eprintln!(
                "{}",
                reply["message"].as_str().unwrap_or("workspace opened")
            );
            cmd_workbench()
        }
        Some("terminal") => cmd_terminal(args.get(1..).unwrap_or(&[])),
        Some("scene") => cmd_scene(args.get(1..).unwrap_or(&[])),
        Some("plugin") => cmd_plugin(args.get(1..).unwrap_or(&[])),
        Some("agent") => cmd_agent(args.get(1..).unwrap_or(&[])),
        Some("integration") => cmd_integration(args.get(1..).unwrap_or(&[])),
        Some("events") => cmd_events(args.get(1..).unwrap_or(&[])),
        Some("sync") => cmd_sync(args.get(1..).unwrap_or(&[])),
        Some("recovery") => cmd_recovery(args.get(1..).unwrap_or(&[])),
        Some("conversations") => cmd_conversations(args.get(1..).unwrap_or(&[])),
        Some("workbench") => cmd_workbench(),
        Some("data") => cmd_data(args.get(1..).unwrap_or(&[])),
        Some("tools") => cmd_tools(args.get(1..).unwrap_or(&[])),
        Some("memory") => cmd_memory(args.get(1..).unwrap_or(&[])),
        Some("skill") => cmd_skill(args.get(1..).unwrap_or(&[])),
        Some("harness") => cmd_harness(args.get(1..).unwrap_or(&[])),
        Some("workflow") => cmd_workflow(args.get(1..).unwrap_or(&[])),
        Some("maintenance") => cmd_maintenance(args.get(1..).unwrap_or(&[])),
        Some("version" | "--version" | "-V") => {
            println!("viva {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            print_usage();
            Err(Error::Validation("unknown or missing subcommand".into()))
        }
    }
}

/// Honest gate BEFORE any server is started: a workbench without a real
/// terminal cannot work, and auto-starting a resident server for a call
/// that must fail would leak a runtime.
fn require_interactive_terminal() -> Result<()> {
    #[cfg(unix)]
    let stdin_is_tty = unsafe { libc::isatty(0) == 1 };
    #[cfg(not(unix))]
    let stdin_is_tty = true;
    if !stdin_is_tty {
        return Err(Error::Validation(
            "viva workbench needs an interactive terminal (stdin is not a tty); \
             use `viva server` for the headless host and `viva terminal` for headless \
             terminals"
                .into(),
        ));
    }
    Ok(())
}

/// `viva workbench` — the interactive parallel-development workbench, run
/// as a CLIENT of the resident server. If no healthy server answers,
/// one is spawned detached first (its own process group, log in
/// `server.log`). `ctrl+b q` detaches the client; the server keeps every terminal
/// running until an explicit `viva shutdown` (ADR 0012, issue #43).
fn cmd_workbench() -> Result<()> {
    require_interactive_terminal()?;
    let home = viva::foundation::paths::viva_home(None);
    let client = viva::server::Client::ensure_server(&home)?;
    eprintln!(
        "workbench: attached to the resident server — ctrl+b q detaches, \
         the server keeps running (`viva shutdown` stops it); home {}",
        home.display()
    );
    viva::tui::workbench::run_client(client)
}

/// `viva agent prompt|wait` and `viva events` (S5, issue #47): the
/// orchestration primitives, reachable from the CLI envelope. Agents call
/// these with --member/--grant so the socket gate can authorize them; a
/// bare human caller acts as the owner.
fn cmd_agent(args: &[String]) -> Result<()> {
    let home = viva::foundation::paths::viva_home(None);
    viva::foundation::paths::ensure_private_dir(&home)?;
    let mut client = viva::server::Client::ensure_server(&home)?;
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
            .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
        named.insert(flag.trim_start_matches("--").to_string(), value.clone());
        i += 1;
    }
    let get = |key: &str| named.get(key).cloned();
    let require = |key: &str| -> Result<String> {
        get(key).ok_or_else(|| Error::Validation(format!("agent needs --{key}")))
    };
    let wait_timeout = get("timeout").and_then(|t| t.parse::<u64>().ok());
    let kind = match args.first().map(String::as_str) {
        Some("prompt") => viva::server::RequestKind::AgentPrompt {
            terminal_id: require("terminal")?,
            prompt: require("text")?,
        },
        Some("wait") => viva::server::RequestKind::AgentWait {
            terminal_id: require("terminal")?,
            status: require("status")?,
            timeout_secs: wait_timeout.unwrap_or(30),
        },
        Some("report") => viva::server::RequestKind::AgentReport {
            terminal_id: require("terminal")?,
            agent: require("agent")?,
            status: require("status")?,
            detail: get("detail").unwrap_or_default(),
            reset_at: get("reset-at"),
            native_session: get("session"),
            resume_command: get("resume"),
        },
        _ => {
            return Err(Error::Validation(
                "agent needs prompt | wait | report".into(),
            ));
        }
    };
    let mut request = viva::server::new_request(kind);
    request.member = get("member");
    request.grant = get("grant");
    let response = if let viva::server::RequestKind::AgentWait { timeout_secs, .. } = &request.kind
    {
        // call_request_with_timeout carries --member/--grant; the old
        // call_with_timeout rebuilt the request and dropped them.
        let budget = std::time::Duration::from_secs(timeout_secs + 15);
        client.call_request_with_timeout(request, budget)?
    } else {
        client.call_request(request)?
    };
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// `viva integration install|uninstall|status <name> [--root <dir>]`.
/// Writes one file at that agent's own config location. `--root` is the
/// home directory; tests pass a temporary one.
fn cmd_integration(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).ok_or_else(|| {
        Error::Validation("integration needs install | uninstall | status".into())
    })?;
    let (name, root) = parse_integration_name_and_root(&args[1..])?;
    let home = match root {
        Some(path) => std::path::PathBuf::from(path),
        None => std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| Error::Validation("HOME is not set; pass --root".into()))?,
    };
    let status = match action {
        "install" => viva::agents::integration::install(&name, &home)?,
        "uninstall" => viva::agents::integration::uninstall(&name, &home)?,
        "status" => viva::agents::integration::status(&name, &home)?,
        _ => {
            return Err(Error::Validation(
                "integration needs install | uninstall | status".into(),
            ));
        }
    };
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

/// Positional agent name plus optional `--root`, in any order after the action.
fn parse_integration_name_and_root(args: &[String]) -> Result<(String, Option<String>)> {
    let mut name: Option<String> = None;
    let mut root: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--root" {
            root = Some(
                args.get(i + 1)
                    .cloned()
                    .ok_or_else(|| Error::Validation("--root needs a directory".into()))?,
            );
            i += 2;
            continue;
        }
        if args[i].starts_with("--") {
            i += 1;
            continue;
        }
        if name.is_some() {
            return Err(Error::Validation(
                "integration accepts only one agent name".into(),
            ));
        }
        name = Some(args[i].clone());
        i += 1;
    }
    let name = name.ok_or_else(|| Error::Validation("integration needs an agent name".into()))?;
    Ok((name, root))
}

/// `viva events [--since <seq>] [--limit <n>]` — the durable event feed
/// (S5): an orchestrator reconnects with its last seen seq.
fn cmd_events(args: &[String]) -> Result<()> {
    let (since, limit) = parse_events_args(args)?;
    let home = viva::foundation::paths::viva_home(None);
    let mut client = viva::server::Client::ensure_server(&home)?;
    let response = client.call(viva::server::RequestKind::EventsFeed {
        since_seq: since,
        limit,
    })?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// Pure parser for `viva events [--since <seq>] [--limit <n>]` so the
/// off-by-one regression (QA F2: flags start at index 0 — the subcommand
/// is already stripped) stays covered by a unit test.
fn parse_events_args(args: &[String]) -> Result<(u64, u32)> {
    let mut since = 0u64;
    let mut limit = 100u32;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                i += 1;
                since = args
                    .get(i)
                    .ok_or_else(|| Error::Validation("--since needs a value".into()))?
                    .parse()
                    .map_err(|_| Error::Validation("--since must be a number".into()))?;
            }
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .ok_or_else(|| Error::Validation("--limit needs a value".into()))?
                    .parse()
                    .map_err(|_| Error::Validation("--limit must be a number".into()))?;
            }
            other => {
                return Err(Error::Validation(format!("unknown events flag `{other}`")));
            }
        }
        i += 1;
    }
    Ok((since, limit))
}

/// `viva sync …` (V15-4): the auto_pull switch and manual cycles.
fn cmd_sync(args: &[String]) -> Result<()> {
    let home = viva::foundation::paths::viva_home(None);
    let mut client = viva::server::Client::ensure_server(&home)?;
    match args.first().map(String::as_str) {
        Some("now") | None => {
            // A cycle can run several 30s-timeout git commands; the
            // client's 10s default read timeout would report failure for
            // work that actually succeeded (QA F14).
            let response = client.call_with_timeout(
                viva::server::RequestKind::SyncNow,
                std::time::Duration::from_secs(150),
            )?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Some("auto-pull") => {
            let on = match args.get(1).map(String::as_str) {
                Some("on") => true,
                Some("off") => false,
                other => {
                    return Err(Error::Validation(format!(
                        "auto-pull needs on|off (got {other:?})"
                    )));
                }
            };
            let response = client.call(viva::server::RequestKind::SetAutoPull { on })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
        }
        Some("status") => {
            let response = client.call_with_timeout(
                viva::server::RequestKind::WorkbenchView,
                std::time::Duration::from_secs(30),
            )?;
            let note = response.get("sync_note").cloned().unwrap_or_default();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "auto_pull": response.get("auto_pull"),
                    "last_sync": note,
                }))?
            );
        }
        _ => {
            return Err(Error::Validation(
                "sync needs now | auto-pull | status".into(),
            ));
        }
    }
    Ok(())
}

/// `viva recovery …` (V15-5): the rate-limit recovery plans.
fn cmd_recovery(args: &[String]) -> Result<()> {
    let home = viva::foundation::paths::viva_home(None);
    let mut client = viva::server::Client::ensure_server(&home)?;
    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].as_str();
        if !flag.starts_with("--") {
            i += 1;
            continue;
        }
        i += 1;
        let value = args
            .get(i)
            .cloned()
            .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
        named.insert(flag.trim_start_matches("--").to_string(), value);
        i += 1;
    }
    let get = |key: &str| named.get(key).cloned();
    let kind = match args.first().map(String::as_str) {
        Some("list") => viva::server::RequestKind::RecoveryList,
        Some("schedule") => viva::server::RequestKind::RecoverySchedule {
            terminal_id: get("terminal")
                .ok_or_else(|| Error::Validation("schedule needs --terminal".into()))?,
            reset_at: get("at")
                .ok_or_else(|| Error::Validation("schedule needs --at <rfc3339>".into()))?,
            task_id: get("task"),
        },
        Some("cancel") => viva::server::RequestKind::RecoveryCancel {
            plan_id: get("id")
                .ok_or_else(|| Error::Validation("cancel needs --id".into()))?
                .parse()
                .map_err(|_| Error::Validation("--id must be a number".into()))?,
        },
        _ => {
            return Err(Error::Validation(
                "recovery needs list | schedule | cancel".into(),
            ));
        }
    };
    let response = client.call(kind)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

/// `viva terminal …` — headless terminal control over the resident server:
/// create fixed-size sessions, read snapshots, resize, stop. No UI needed;
/// a resident server is auto-started when none runs (issue #43).
fn cmd_terminal(args: &[String]) -> Result<()> {
    let home = viva::foundation::paths::viva_home(None);
    viva::foundation::paths::ensure_private_dir(&home)?;
    let mut client = viva::server::Client::ensure_server(&home)?;
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
                            Error::Validation(format!("flag --{flag} needs a value"))
                        })?;
                        match flag.as_str() {
                            "cols" => {
                                cols = value.parse().map_err(|_| {
                                    Error::Validation("--cols must be a number".into())
                                })?
                            }
                            "rows" => {
                                rows = value.parse().map_err(|_| {
                                    Error::Validation("--rows must be a number".into())
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
                        return Err(Error::Validation(format!(
                            "unknown terminal create flag `{other}`"
                        )));
                    }
                }
                i += 1;
            }
            if argv.is_empty() {
                return Err(Error::Validation(
                    "terminal create needs an explicit argv after `--`".into(),
                ));
            }
            let cwd = cwd
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let response = client.call(viva::server::RequestKind::TerminalCreate {
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
                Error::Validation("usage: viva terminal snapshot <terminal-id>".into())
            })?;
            let response = client.call(viva::server::RequestKind::TerminalSnapshot {
                terminal_id: terminal_id.clone(),
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
        Some("resize") => {
            let terminal_id = args.get(1).ok_or_else(|| {
                Error::Validation(
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
                    .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
                named.insert(flag.to_string(), value.clone());
                i += 1;
            }
            let cols: u16 = named
                .get("--cols")
                .ok_or_else(|| Error::Validation("resize needs --cols".into()))?
                .parse()
                .map_err(|_| Error::Validation("--cols must be a number".into()))?;
            let rows: u16 = named
                .get("--rows")
                .ok_or_else(|| Error::Validation("resize needs --rows".into()))?
                .parse()
                .map_err(|_| Error::Validation("--rows must be a number".into()))?;
            let response = client.call(viva::server::RequestKind::TerminalResize {
                terminal_id: terminal_id.clone(),
                cols,
                rows,
            })?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
        _ => Err(Error::Validation(
            "terminal needs create | snapshot | resize (stop via `viva stop-terminal`)".into(),
        )),
    }
}

fn cmd_plugin(args: &[String]) -> Result<()> {
    let home = viva::foundation::paths::viva_home(None);
    viva::foundation::paths::ensure_private_dir(&home)?;
    match args.first().map(String::as_str) {
        Some("install") => {
            if args.len() != 2 {
                return Err(Error::Validation(
                    "usage: viva plugin install <directory>".into(),
                ));
            }
            let bin = std::env::current_exe().map_err(|err| {
                Error::Validation(format!("cannot locate the viva binary: {err}"))
            })?;
            let info = viva::plugins::install(&home, &args[1], &bin)?;
            println!("{}", serde_json::to_string_pretty(&info)?);
            Ok(())
        }
        Some("uninstall") => {
            if args.len() != 2 {
                return Err(Error::Validation(
                    "usage: viva plugin uninstall <id>".into(),
                ));
            }
            viva::plugins::uninstall(&home, &args[1])?;
            println!("{}", serde_json::json!({ "uninstalled": args[1] }));
            Ok(())
        }
        Some("list") => {
            if args.len() != 1 {
                return Err(Error::Validation("usage: viva plugin list".into()));
            }
            let listed = viva::plugins::list(&home)?;
            println!("{}", serde_json::to_string_pretty(&listed)?);
            Ok(())
        }
        _ => Err(Error::Validation(
            "plugin needs install <directory> | uninstall <id> | list".into(),
        )),
    }
}

fn cmd_scene(args: &[String]) -> Result<()> {
    let home = viva::foundation::paths::viva_home(None);
    viva::foundation::paths::ensure_private_dir(&home)?;
    let (member, grant, args) = split_member_grant(args)?;
    let op = parse_scene_op(&args)?;
    let mut client = viva::server::Client::ensure_server(&home)?;
    let mut request = viva::server::new_request(viva::server::RequestKind::WorkbenchTree { op });
    request.member = member;
    request.grant = grant;
    let response = client.call_request(request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn split_member_grant(args: &[String]) -> Result<(Option<String>, Option<String>, Vec<String>)> {
    let mut member = None;
    let mut grant = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--member" | "--grant" => {
                let flag = args[i].as_str();
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
                if flag == "--member" {
                    member = Some(value.clone());
                } else {
                    grant = Some(value.clone());
                }
            }
            other => rest.push(other.to_string()),
        }
        i += 1;
    }
    Ok((member, grant, rest))
}

fn scene_flags(args: &[String]) -> Result<std::collections::HashMap<String, String>> {
    let mut flags = std::collections::HashMap::new();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if !flag.starts_with("--") {
            return Err(Error::Validation(format!(
                "unexpected argument `{flag}` (see `viva help`)"
            )));
        }
        i += 1;
        let value = args
            .get(i)
            .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
        flags.insert(flag.trim_start_matches("--").to_string(), value.clone());
        i += 1;
    }
    Ok(flags)
}

fn flag<'a>(flags: &'a std::collections::HashMap<String, String>, name: &str) -> Result<&'a str> {
    flags
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| Error::Validation(format!("scene needs --{name}")))
}

fn opt_flag(flags: &std::collections::HashMap<String, String>, name: &str) -> Option<String> {
    flags.get(name).cloned()
}

fn parse_direction(text: &str) -> Result<String> {
    match text {
        "right" | "r" => Ok("right".into()),
        "down" | "d" => Ok("down".into()),
        other => Err(Error::Validation(format!(
            "direction `{other}` must be right or down"
        ))),
    }
}

fn parse_path(text: &str) -> Result<Vec<bool>> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|part| match part.trim() {
            "0" => Ok(false),
            "1" => Ok(true),
            other => Err(Error::Validation(format!(
                "path step `{other}` must be 0 or 1"
            ))),
        })
        .collect()
}

fn parse_on(text: &str) -> Result<bool> {
    match text {
        "true" | "on" | "1" => Ok(true),
        "false" | "off" | "0" => Ok(false),
        other => Err(Error::Validation(format!(
            "--on `{other}` must be true or false"
        ))),
    }
}

fn parse_scene_op(args: &[String]) -> Result<viva::server::WorkbenchTreeOp> {
    use viva::server::WorkbenchTreeOp;
    let usage = "scene needs list | create | rename | close | focus | tab | pane (see `viva help`)";
    match args.first().map(String::as_str) {
        Some("list") => {
            if args.len() != 1 {
                return Err(Error::Validation("scene list takes no arguments".into()));
            }
            Ok(WorkbenchTreeOp::List)
        }
        Some("create") => {
            let flags = scene_flags(&args[1..])?;
            Ok(WorkbenchTreeOp::CreateScene {
                id: opt_flag(&flags, "id"),
                name: opt_flag(&flags, "name").unwrap_or_default(),
            })
        }
        Some("rename") => {
            let flags = scene_flags(&args[1..])?;
            Ok(WorkbenchTreeOp::RenameScene {
                id: flag(&flags, "id")?.to_string(),
                name: flag(&flags, "name")?.to_string(),
            })
        }
        Some("close") => {
            let flags = scene_flags(&args[1..])?;
            Ok(WorkbenchTreeOp::CloseScene {
                id: flag(&flags, "id")?.to_string(),
            })
        }
        Some("focus") => {
            let flags = scene_flags(&args[1..])?;
            Ok(WorkbenchTreeOp::FocusScene {
                id: flag(&flags, "id")?.to_string(),
            })
        }
        Some("tab") => parse_tab_op(&args[1..]),
        Some("pane") => parse_pane_op(&args[1..]),
        _ => Err(Error::Validation(usage.into())),
    }
}

fn parse_tab_op(args: &[String]) -> Result<viva::server::WorkbenchTreeOp> {
    use viva::server::WorkbenchTreeOp;
    let flags = scene_flags(args.get(1..).unwrap_or(&[]))?;
    match args.first().map(String::as_str) {
        Some("create") => Ok(WorkbenchTreeOp::CreateTab {
            scene_id: opt_flag(&flags, "scene"),
            name: flag(&flags, "name")?.to_string(),
        }),
        Some("rename") => Ok(WorkbenchTreeOp::RenameTab {
            scene_id: opt_flag(&flags, "scene"),
            tab_id: flag(&flags, "id")?.to_string(),
            name: flag(&flags, "name")?.to_string(),
        }),
        Some("close") => Ok(WorkbenchTreeOp::CloseTab {
            scene_id: opt_flag(&flags, "scene"),
            tab_id: flag(&flags, "id")?.to_string(),
        }),
        Some("focus") => Ok(WorkbenchTreeOp::FocusTab {
            scene_id: opt_flag(&flags, "scene"),
            tab_id: flag(&flags, "id")?.to_string(),
        }),
        _ => Err(Error::Validation(
            "scene tab needs create | rename | close | focus".into(),
        )),
    }
}

fn parse_pane_op(args: &[String]) -> Result<viva::server::WorkbenchTreeOp> {
    use viva::server::WorkbenchTreeOp;
    let flags = scene_flags(args.get(1..).unwrap_or(&[]))?;
    let scene_id = opt_flag(&flags, "scene");
    let tab_id = opt_flag(&flags, "tab");
    match args.first().map(String::as_str) {
        Some("create") => Ok(WorkbenchTreeOp::CreatePane {
            scene_id,
            tab_id,
            terminal_id: opt_flag(&flags, "terminal"),
            direction: opt_flag(&flags, "direction")
                .map(|value| parse_direction(&value))
                .transpose()?,
        }),
        Some("rename") => Ok(WorkbenchTreeOp::RenamePane {
            scene_id,
            tab_id,
            pane_id: flag(&flags, "id")?.to_string(),
            name: flag(&flags, "name")?.to_string(),
        }),
        Some("close") => Ok(WorkbenchTreeOp::ClosePane {
            scene_id,
            tab_id,
            pane_id: flag(&flags, "id")?.to_string(),
        }),
        Some("focus") => Ok(WorkbenchTreeOp::FocusPane {
            scene_id,
            tab_id,
            pane_id: flag(&flags, "id")?.to_string(),
        }),
        Some("split") => Ok(WorkbenchTreeOp::Split {
            scene_id,
            tab_id,
            pane_id: opt_flag(&flags, "pane"),
            direction: parse_direction(flag(&flags, "direction")?)?,
            terminal_id: opt_flag(&flags, "terminal"),
        }),
        Some("zoom") => Ok(WorkbenchTreeOp::Zoom {
            scene_id,
            tab_id,
            on: opt_flag(&flags, "on")
                .map(|value| parse_on(&value))
                .transpose()?,
        }),
        Some("resize") => {
            let ratio: u16 = flag(&flags, "ratio")?
                .parse()
                .map_err(|_| Error::Validation("--ratio must be a number".into()))?;
            Ok(WorkbenchTreeOp::Resize {
                scene_id,
                tab_id,
                path: opt_flag(&flags, "path")
                    .map(|value| parse_path(&value))
                    .transpose()?
                    .unwrap_or_default(),
                ratio,
            })
        }
        Some("input") => {
            let text = opt_flag(&flags, "text");
            let hex = opt_flag(&flags, "hex");
            let bytes_hex = match (text, hex) {
                (Some(text), None) => viva::server::hex_encode(text.as_bytes()),
                (None, Some(hex)) => hex,
                _ => {
                    return Err(Error::Validation(
                        "pane input needs exactly one of --text or --hex".into(),
                    ));
                }
            };
            Ok(WorkbenchTreeOp::PaneInput {
                scene_id,
                tab_id,
                pane_id: flag(&flags, "id")?.to_string(),
                bytes_hex,
            })
        }
        Some("snapshot") => Ok(WorkbenchTreeOp::PaneSnapshot {
            scene_id,
            tab_id,
            pane_id: flag(&flags, "id")?.to_string(),
        }),
        _ => Err(Error::Validation(
            "scene pane needs create | rename | close | focus | split | zoom | resize | input | snapshot"
                .into(),
        )),
    }
}

fn print_usage() {
    println!(
        "viva — local-first personal AI collaboration system

USAGE:
    viva
        Attach the interactive parallel-development workbench to the
        resident server (one is spawned detached when none runs).
        ctrl+b q detaches the client; the server keeps every terminal running.

    viva help
        Print this usage text.

    viva init
        Create VIVA_HOME (default ~/.viva, override with $VIVA_HOME) and the
        database, applying all registered migrations.

    viva doctor
        Report state root, database, schema versions and table counts.

    viva open <path>
        Open a repo, a folder or a VS Code `.code-workspace` file as the
        current workspace, then attach the workbench. The same path always
        maps to the same workspace; the file itself is never modified.

    viva workspace [list | open <path> | new <name> | select <id|all> |
                    add <dir> | remove <project-id|path> | save <file> |
                    refresh]
        Compose workspaces without the TUI (owner only). `save` writes a
        `.code-workspace` file that VS Code opens; only its `folders` list
        changes.         Removing a folder never deletes files or stops processes.

    viva machine add --target <user@host> [--label <name>] [--id <id>]
                     [--session <name>]
    viva machine list
    viva machine rename <id> <label>
    viva machine disable <id> | enable <id> | remove <id>
    viva machine bridge --session <name>
        Save an SSH machine: id, label, target, remote session, enabled.
        Passwords, private keys, and agent tickets stay with OpenSSH.
        `bridge` is what SSH runs on the remote machine to reach that
        session's socket. A missing `viva` there fails; nothing is copied
        or installed.

    viva --machine <id> <command>
        Send <command> to that machine's viva server. This computer's
        default socket is unchanged. The workbench always stays here.

    viva update [--check] [--no-restart]
        Upgrade to the latest stable version (GitHub Release or global npm).
        --check only checks. After installing, a running resident server is
        restarted through the live handover (--no-restart skips); VIVA_HOME
        data is untouched.

    viva event add <domain> <kind> <subject-type> <subject-id> [payload-json]
        Append one event (origin: cli).

    viva event list [limit]
        List the most recent events (default 20).

    viva server
        Run the resident server in the foreground (the same as
        `viva start`): owns the store, every terminal and the control
        channel until `viva shutdown` arrives. Clients may detach freely.

    viva status
        Viva snapshot. Uses the live channel when a host is active, and
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

    viva scene list
    viva scene create [--id <id>] [--name <name>] | rename --id <id> --name <name>
        | close --id <id> | focus --id <id>
    viva scene tab create [--scene <id>] --name <name>
        | rename --id <tab> --name <name> [--scene <id>]
        | close --id <tab> [--scene <id>] | focus --id <tab> [--scene <id>]
    viva scene pane create [--scene <id>] [--tab <id>] [--terminal <id>]
        [--direction right|down]
        | rename --id <pane> --name <name> [--scene <id>] [--tab <id>]
        | close --id <pane> [--scene <id>] [--tab <id>]
        | focus --id <pane> [--scene <id>] [--tab <id>]
        | split --direction right|down [--terminal <id>] [--pane <id>]
          [--scene <id>] [--tab <id>]
        | zoom [--on true|false] [--scene <id>] [--tab <id>]
        | resize --ratio <10-90> [--path 0,1] [--scene <id>] [--tab <id>]
        | input --id <pane> (--text <text> | --hex <hex>)
        | snapshot --id <pane>
        List and edit the resident scene/tab/pane tree. Closing a pane does
        not stop its terminal. --member and --grant follow the socket rules.
        Two clients share the tree; a later focus or size wins for that tab.

    viva plugin install <directory>
        Install a plugin from a local directory that contains
        viva-plugin.json, and start its entry process. URLs, herdr.dev,
        and marketplace endpoints are refused. The process receives
        pane.opened and popup events on a private socket and operates
        panes with `viva scene`. A member still needs a live grant.
        Protected actions (merge, approve, push to a protected branch,
        self-authorize) are refused. popup is not a pane and has no pane id.

    viva plugin uninstall <id>
        Stop that plugin process and remove the installed copy.

    viva plugin list
        List installed plugins.

    viva stop-terminal <terminal-id>
        Stop one terminal (its process group only).

    viva result <task-id>
        Show the results recorded for a task.

    viva create-project --name <name> --repo <dir>
        Register the MAIN checkout of a repository (the target of the
        V15-4 automatic fast-forward when auto_pull is enabled).

    viva create-task --goal <text> | viva grant --member <id>
        --task <id> --action <a> --mode <m> | viva brief <task-id> |
        viva handoff --task <id> --member <id> --summary <text>
        Task, grant, brief and handoff management.

    viva sync now | viva sync auto-pull on|off | viva sync status
        V15-4: run one sync cycle now; toggle the auto_pull switch
        (default off — main checkout fast-forward only); show state.

    viva recovery list
    viva recovery schedule --terminal <id> --at <rfc3339> [--task <id>]
    viva recovery cancel --id <n>
        V15-5: rate-limit recovery plans — schedule a wakeup that prompts
        the agent to continue, list, or cancel.

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
    viva agent report --terminal <id> --agent <name> \
        --status <working|blocked|done|idle|unknown|rate_limited> \
        [--detail <text>] [--session <ref>] [--resume <command>] \
        [--reset-at <rfc3339>] [--member <id> --grant <id>]
        Prompt an agent's terminal, wait for its reported status, or
        record that status. A session reference is stored and never run.
        With no --member, the caller is the owner. A member still needs
        a live grant carrying agent_report.

    viva integration install|uninstall|status <name> [--root <dir>]
        Optional hook for one agent: pi, claude, codex, opencode, kimi,
        qodercli, cursor, grok, hermes. Install writes only that agent's
        file. Uninstall deletes that file when Viva wrote it. macOS only.

    viva events [--since <seq>] [--limit <n>]
        The durable event feed: reconnect with your last seen seq
        and miss nothing.

    viva conversations <create|fork|rename|set-native|attach-task|detach-task|archive|tree|handoff> [flags]
        Conversation metadata (Viva-owned tree; the harness owns the transcript).

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
        store may still apply pending schema migrations.

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

    viva skill create --name <text> --body-file <path> (--execution <id> | --manual)
    viva skill write --skill <id> --body-file <path> (--execution <id> | --manual)
    viva skill get --skill <id>
    viva skill versions --skill <id>
    viva skill version-get --skill <id> --version <n>
    viva skill rollback <skill> <version>
    viva skill project --skill <id> --out <path>
        Team skill bodies with append-only versions (#140). Every write
        records version, time, operator and source execution (or Haisu
        manual). Rollback copies an older version forward; history stays.
        `get` and `project` read the current version for callbacks and
        member projection.

    viva memory search --member <id> [--project <id>] --query <text>
        Search the linked external memory for one member's scope
        (read-only). Unavailable stores say so; unclaimable hits are
        counted, not silently mixed in.

    viva memory remember --member <id> [--project <id>] --content <text>
                         --source <provenance> [--category <c>] [--tags <t>]
        Write one native fact with mandatory provenance.

    viva memory archive|restore --fact <id> --reason <text>
        Exit paths over Viva's link layer. Nothing here deletes.

    viva memory status
        Report native memory in the Viva store (fact counts and any
        legacy Hermes link rows kept for provenance only).

    viva harness matrix
        Cross-harness capability matrix (launch / interactive / status /
        handoff / native resume / fork) with tested, unsupported or unknown
        labels. Probes Pi on this machine when listed.

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

fn open_store() -> Result<Store> {
    let home = viva_home(None);
    ensure_private_dir(&home)?;
    // The product entry opens the FULL composition (every delivered
    // domain), not just the foundation slice — `viva init` must leave a
    // store Viva can actually use.
    Store::open(&database_path(&home), viva::server::migrations())
}

fn cmd_init() -> Result<()> {
    let home = viva_home(None);
    ensure_private_dir(&home)?;
    let store = open_store()?;
    println!("viva home: {}", home.display());
    println!("database:  {}", database_path(&home).display());
    for (domain, version) in store.schema_versions() {
        println!("schema:    {domain} v{version}");
    }
    Ok(())
}

fn cmd_doctor() -> Result<()> {
    let home = viva_home(None);
    let db = database_path(&home);
    println!("home:      {}", home.display());
    println!(
        "database:  {} ({})",
        db.display(),
        if db.exists() { "present" } else { "missing" }
    );
    let store = open_store()?;
    println!("state:     openable");
    for (domain, version) in store.schema_versions() {
        println!("schema:    {domain} v{version}");
    }
    for (domain, version, name, applied_at) in store.applied_migrations()? {
        println!("migration: {domain} v{version} `{name}` at {applied_at}");
    }
    for table in [
        "events",
        "sessions",
        "executions",
        "launch_specs",
        "terminal_events",
        "control_requests",
    ] {
        println!("count:     {table} = {}", store.row_count(table)?);
    }
    let matrix = viva::harness::capabilities::build(true);
    println!(
        "harness:   {} rows (run `viva harness matrix` for JSON)",
        matrix.rows.len()
    );
    Ok(())
}

fn cmd_harness(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("matrix") => {
            let matrix = viva::harness::capabilities::build(true);
            println!("{}", serde_json::to_string_pretty(&matrix)?);
            Ok(())
        }
        _ => {
            print_usage();
            Err(Error::Validation("harness needs `matrix`".into()))
        }
    }
}

fn cmd_event(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("add") => cmd_event_add(args.get(1..).unwrap_or(&[])),
        Some("list") => cmd_event_list(args.get(1..).unwrap_or(&[])),
        _ => {
            print_usage();
            Err(Error::Validation("event needs `add` or `list`".into()))
        }
    }
}

fn cmd_event_add(args: &[String]) -> Result<()> {
    let [domain, kind, subject_type, subject_id, rest @ ..] = args else {
        return Err(Error::Validation(
            "usage: viva event add <domain> <kind> <subject-type> <subject-id> [payload-json]"
                .into(),
        ));
    };
    if rest.len() > 1 {
        return Err(Error::Validation(
            "usage: viva event add <domain> <kind> <subject-type> <subject-id> [payload-json]"
                .into(),
        ));
    }
    let domain = parse_domain(domain)?;
    let payload = match rest.first() {
        Some(text) => serde_json::from_str(text)?,
        None => serde_json::Value::Null,
    };
    let store = open_store()?;
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

fn cmd_event_list(args: &[String]) -> Result<()> {
    if args.len() > 1 {
        return Err(Error::Validation("usage: viva event list [limit]".into()));
    }
    let limit: u32 = match args.first() {
        Some(text) => text
            .parse()
            .map_err(|_| Error::Validation(format!("`{text}` is not a valid limit")))?,
        None => 20,
    };
    let store = open_store()?;
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

fn parse_domain(name: &str) -> Result<viva::foundation::store::Domain> {
    KNOWN_DOMAINS
        .iter()
        .copied()
        .find(|domain| domain.as_str() == name)
        .ok_or_else(|| {
            Error::Validation(format!(
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
// Server control plane (V07)
// ---------------------------------------------------------------------------

fn cmd_control(args: &[String]) -> Result<()> {
    let home = viva::foundation::paths::viva_home(None);
    match args.first().map(String::as_str) {
        Some("server" | "start") => {
            let resume = args.get(1).map(String::as_str) == Some("--resume");
            cmd_control_start(&home, resume)
        }
        Some("server-restart") => cmd_server_restart(&home),
        Some("status") => cmd_control_status(&home),
        Some("dispatch") => cmd_control_dispatch(&home, args.get(1..).unwrap_or(&[])),
        Some("terminals") => cmd_control_query(&home, viva::server::RequestKind::TerminalList),
        Some("stop-terminal") => {
            let terminal_id = args.get(1).ok_or_else(|| {
                Error::Validation("usage: viva stop-terminal <terminal-id>".into())
            })?;
            cmd_control_query(
                &home,
                viva::server::RequestKind::TerminalStop {
                    terminal_id: terminal_id.clone(),
                },
            )
        }
        Some("result") => {
            let task_id = args
                .get(1)
                .ok_or_else(|| Error::Validation("usage: viva result <task-id>".into()))?;
            cmd_control_query(
                &home,
                viva::server::RequestKind::TaskResults {
                    task_id: task_id.clone(),
                },
            )
        }
        Some("shutdown") => {
            let close_policy = std::env::var("VIVA_CLOSE_POLICY")
                .ok()
                .filter(|p| p == "pause");
            cmd_control_query(&home, viva::server::RequestKind::Shutdown { close_policy })?;
            // Human commentary goes to stderr; stdout stays pure JSON for
            // callers that parse it (the extension envelope, tooling).
            eprintln!("server: shutdown accepted; owned terminals stopped, handoff persisted");
            Ok(())
        }
        Some("pause") => {
            cmd_control_query(&home, viva::server::RequestKind::Pause)?;
            eprintln!(
                "server: paused - no new dispatch, no maintenance; running executions keep running"
            );
            Ok(())
        }
        Some("resume") => {
            cmd_control_query(&home, viva::server::RequestKind::Resume)?;
            eprintln!("server: resumed - dispatch and maintenance are live again");
            Ok(())
        }
        Some("brief") => cmd_control_brief(args.get(1..).unwrap_or(&[])),
        Some("create-task") => cmd_control_create_task(args.get(1..).unwrap_or(&[])),
        Some("create-project") => cmd_control_create_project(args.get(1..).unwrap_or(&[])),
        Some("grant") => cmd_control_grant(args.get(1..).unwrap_or(&[])),
        Some("handoff") => cmd_control_handoff(&home, args.get(1..).unwrap_or(&[])),
        _ => {
            print_usage();
            Err(Error::Validation(
                "viva needs start | status | dispatch | terminals | stop-terminal | result | brief | create-task | grant | handoff | shutdown"
                    .into(),
            ))
        }
    }
}

/// `viva brief <task-id>` — offline, read-only task brief from the
/// store (the extension's context-entry endpoint).
fn cmd_control_brief(args: &[String]) -> Result<()> {
    let task_id = args
        .first()
        .ok_or_else(|| Error::Validation("usage: viva brief <task-id>".into()))?;
    let task_id = viva::foundation::ids::TaskId::from_str(task_id)?;
    let home = viva::foundation::paths::viva_home(None);
    let db = viva::foundation::paths::database_path(&home);
    if !db.exists() {
        return Err(Error::Validation(format!(
            "no Viva store at {} (run `viva init` first); nothing was created",
            db.display()
        )));
    }
    viva::foundation::paths::ensure_private_dir(&home)?;
    let store = viva::foundation::store::Store::open(&db, viva::server::migrations())?;
    let tasks = viva::tasks::TaskRegistry::new(&store);
    let brief = tasks.generate_brief(&task_id)?;
    println!("{}", serde_json::to_string_pretty(&brief)?);
    Ok(())
}

/// `viva handoff --task <id> --member <id> --summary <text>` —
/// records a member-reported handoff over the live channel. A member
/// report is a fact about who said what; it never completes a task.
fn cmd_control_handoff(home: &std::path::Path, args: &[String]) -> Result<()> {
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
            .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
        match flag {
            "--task" => task_id = Some(value),
            "--member" => member_id = Some(value),
            "--summary" => summary = Some(value),
            other => {
                return Err(Error::Validation(format!("unknown handoff flag `{other}`")));
            }
        }
        i += 1;
    }
    let (Some(task_id), Some(member_id), Some(summary)) = (task_id, member_id, summary) else {
        return Err(Error::Validation(
            "usage: viva handoff --task <id> --member <id> --summary <text>".into(),
        ));
    };
    let response = viva::server::send_request(
        home,
        viva::server::new_request(viva::server::RequestKind::Handoff {
            task_id,
            member_id,
            summary,
        }),
    )?;
    print_response(&response)
}

fn cmd_control_start(home: &std::path::Path, resume: bool) -> Result<()> {
    if resume {
        // Live-handoff resume (S3): adopt the transferred terminals and
        // serve. Blocks until this host is shut down.
        eprintln!(
            "server (pid {}) resuming from live handoff — home {}",
            std::process::id(),
            home.display()
        );
        viva::server::resume_server(home)?;
        eprintln!("resumed host released the control channel");
        return Ok(());
    }
    let host = viva::server::Host::open(home)?;
    // The previous process is gone. Restore the layout with new shells.
    // `server --resume` does not reach here; that path keeps live processes.
    viva::server::restore_workspace(&host.shared())?;
    eprintln!(
        "server {} active (pid {}) — home {}",
        host.shared().host_id,
        std::process::id(),
        home.display()
    );
    host.serve()?;
    eprintln!("server released the control channel");
    Ok(())
}

/// `viva server-restart` — upgrade path: the running server transfers its
/// live terminals to a freshly spawned resumed server (S3, issue #45).
fn cmd_server_restart(home: &std::path::Path) -> Result<()> {
    let new_pid = viva::server::restart_server(home)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "restarted": true,
            "new_pid": new_pid,
        }))?
    );
    Ok(())
}

fn cmd_control_status(home: &std::path::Path) -> Result<()> {
    let request = viva::server::new_request(viva::server::RequestKind::Status);
    match viva::server::send_request(home, request) {
        Ok(response) => print_response(&response),
        Err(live_err) if viva::machines::invocation_target().is_some() => Err(live_err),
        Err(live_err) => {
            // Read-only status is answerable offline by design; say both
            // facts plainly.
            eprintln!("server: live channel unavailable: {live_err}");
            let offline = viva::server::offline_status(home)?;
            println!("{}", serde_json::to_string_pretty(&offline)?);
            Ok(())
        }
    }
}

fn cmd_control_query(home: &std::path::Path, kind: viva::server::RequestKind) -> Result<()> {
    let response = viva::server::send_request(home, viva::server::new_request(kind))?;
    print_response(&response)
}

fn cmd_control_dispatch(home: &std::path::Path, args: &[String]) -> Result<()> {
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
        let value = |i: &mut usize| -> Result<String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))
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
                return Err(Error::Validation(format!(
                    "unknown dispatch flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    let (Some(task_id), Some(member_id), Some(grant_id), Some(request_key), Some(cwd)) =
        (task_id, member_id, grant_id, request_key, cwd)
    else {
        return Err(Error::Validation(
            "usage: viva dispatch --task <id> --member <id> --grant <id> \
             --request-key <key> --cwd <dir> [--worktree <id>] -- argv..."
                .into(),
        ));
    };
    if argv.is_empty() {
        return Err(Error::Validation(
            "dispatch needs an explicit argv after `--` (no joined shell strings)".into(),
        ));
    }
    let response = viva::server::send_request(
        home,
        viva::server::new_request(viva::server::RequestKind::Dispatch {
            task_id,
            member_id,
            grant_id,
            request_key,
            argv,
            cwd,
            worktree_id,
        }),
    )?;
    print_response(&response)
}

fn print_response(response: &viva::server::Response) -> Result<()> {
    if response.ok {
        println!(
            "{}",
            serde_json::to_string_pretty(
                response.result.as_ref().unwrap_or(&serde_json::Value::Null)
            )?
        );
        Ok(())
    } else {
        Err(Error::Validation(
            response
                .error
                .clone()
                .unwrap_or_else(|| "rejected without a reason".into()),
        ))
    }
}

// ---------------------------------------------------------------------------
// Conversation metadata CLI (V10) — Viva-owned display names, fork trees
// and handoff records. The harness-native transcript is never copied here.
// ---------------------------------------------------------------------------

fn cmd_conversations(args: &[String]) -> Result<()> {
    let home = viva::foundation::paths::viva_home(None);
    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(&home),
        viva::server::migrations(),
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
            .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
        named.insert(flag.to_string(), value);
        i += 1;
    }
    let get = |key: &str| named.get(key).cloned();
    let require = |key: &str| -> Result<String> {
        get(key).ok_or_else(|| Error::Validation(format!("conversations {args:?} needs --{key}")))
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
            return Err(Error::Validation(
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
fn cmd_tools(args: &[String]) -> Result<()> {
    use std::str::FromStr as _;

    // args[0] is the tool family (`computer`); args[1..] its command.
    if args.first().map(String::as_str) != Some("computer") {
        return Err(Error::Validation(
            "usage: viva tools computer lane | audit | smoke --task <id> \
             --member <id> --grant <id> | release-lease --task <id> --acquired-at <time> \
             --member <id> --grant <id> --reason <text> --confirm"
                .into(),
        ));
    }
    match args.get(1).map(String::as_str) {
        Some("lane") => {
            if args.len() != 2 {
                return Err(Error::Validation("lane accepts no flags".into()));
            }
            let store = open_store()?;
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
                        return Err(Error::Validation("duplicate --confirm".into()));
                    }
                    confirm = true;
                    i += 1;
                    continue;
                }
                if !["--task", "--acquired-at", "--member", "--grant", "--reason"].contains(&flag)
                    || named.contains_key(flag)
                {
                    return Err(Error::Validation(format!(
                        "unexpected or duplicate release-lease flag `{flag}`"
                    )));
                }
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
                named.insert(flag, value.as_str());
                i += 1;
            }
            if !confirm {
                return Err(Error::Validation(
                    "release-lease requires --confirm after checking the original action stopped"
                        .into(),
                ));
            }
            let require = |flag| {
                named
                    .get(flag)
                    .copied()
                    .ok_or_else(|| Error::Validation(format!("release-lease needs {flag}")))
            };
            let task = viva::foundation::ids::TaskId::from_str(require("--task")?)?;
            let member = viva::foundation::ids::MemberId::from_str(require("--member")?)?;
            let grant = viva::foundation::ids::GrantId::from_str(require("--grant")?)?;
            let store = open_store()?;
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
            let store = open_store()?;
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
                    .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
                match flag {
                    "--task" => task_id = Some(value),
                    "--member" => member_id = Some(value),
                    "--grant" => grant_id = Some(value),
                    other => {
                        return Err(Error::Validation(format!("unknown smoke flag `{other}`")));
                    }
                }
                i += 1;
            }
            let (Some(task_id), Some(member_id), Some(grant_id)) = (task_id, member_id, grant_id)
            else {
                return Err(Error::Validation(
                    "usage: viva tools computer smoke --task <id> --member <id> --grant <id>"
                        .into(),
                ));
            };
            let task_id = viva::foundation::ids::TaskId::from_str(&task_id)?;
            let member_id = viva::foundation::ids::MemberId::from_str(&member_id)?;
            let grant_id = viva::foundation::ids::GrantId::from_str(&grant_id)?;

            let store = open_store()?;
            // Smoke evidence binds to a real Viva task, not an invented one.
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
                return Err(Error::Validation(format!(
                    "smoke incomplete: {executed}/2 tasks executed — see the evidence \
                     above for the honest state"
                )));
            }
            Ok(())
        }
        other => Err(Error::Validation(format!(
            "unknown tools computer command `{}`; expected `lane`, `release-lease`, `audit` or `smoke`",
            other.unwrap_or("<missing>")
        ))),
    }
}

/// A shareable summary of one computer action: verdict and verification
/// facts, with the raw tool snapshots replaced by size + fingerprint.
fn summarize_action(record: &viva::tools::computer::ActionRecord) -> Result<serde_json::Value> {
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

/// `viva create-project --name <name> --repo <dir>` — register the MAIN
/// checkout of a repository (V15-4, QA F11): without this row the
/// auto-sync cycle has nothing to fast-forward. Read-only worktree
/// adoption remains the worktree service's own flow.
fn cmd_control_create_project(args: &[String]) -> Result<()> {
    let mut name = None;
    let mut repo = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                name = args.get(i).cloned();
            }
            "--repo" => {
                i += 1;
                repo = args.get(i).cloned();
            }
            other => {
                return Err(Error::Validation(format!(
                    "unknown create-project flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    let (Some(name), Some(repo)) = (name, repo) else {
        return Err(Error::Validation(
            "usage: viva create-project --name <name> --repo <dir>".into(),
        ));
    };
    let store = open_store()?;
    let project = viva::projects::ProjectRegistry::new(&store).register(None, name, repo)?;
    println!("{}", serde_json::to_string_pretty(&project)?);
    Ok(())
}

/// `viva create-task --goal <text>` — open one task. This is
/// an owner-side CLI: whoever runs the binary creates the task in their
/// own VIVA_HOME.
fn cmd_control_create_task(args: &[String]) -> Result<()> {
    let mut goal = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--goal" => {
                i += 1;
                goal = args.get(i).cloned();
            }
            other => {
                return Err(Error::Validation(format!(
                    "unknown create-task flag `{other}`"
                )));
            }
        }
        i += 1;
    }
    let goal =
        goal.ok_or_else(|| Error::Validation("usage: viva create-task --goal <text>".into()))?;
    let store = open_store()?;
    let task =
        viva::tasks::TaskRegistry::new(&store).create_task(goal, vec![], None, None, None)?;
    println!("{}", serde_json::to_string_pretty(&task)?);
    Ok(())
}

/// `viva grant --member <id> --task <id> --action <a> --mode <m>
/// [--expires <rfc3339>]` — an owner-side root grant (the CLI runner IS
/// the user; the engine records issuer=user). Protected actions are
/// refused by the engine itself.
fn cmd_control_grant(args: &[String]) -> Result<()> {
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
            .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
        match flag {
            "--member" => member = Some(value),
            "--task" => task = Some(value),
            "--action" => action = Some(value),
            "--mode" => mode = Some(value),
            "--expires" => expires = Some(value),
            other => {
                return Err(Error::Validation(format!("unknown grant flag `{other}`")));
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
        Error::Validation(
            "usage: viva grant --member <id> --task <id> --action <a> --mode <m> \
             [--expires <rfc3339>]"
                .into(),
        )
    })?;
    let mode = viva::authority::GrantMode::from_str_value(&mode.ok_or_else(|| {
        Error::Validation(
            "--mode is required (READ | PROPOSE | ACT_WITH_APPROVAL | ACT_AUTONOMOUSLY)".into(),
        )
    })?)?;
    let store = open_store()?;
    let engine = viva::authority::AuthorityEngine::new(&store);
    let grant = engine.issue_root_grant(member, task, vec![action], mode, expires)?;
    println!("{}", serde_json::to_string_pretty(&grant)?);
    Ok(())
}

/// `viva skill …` — team skill body versioning (#140) and member skill sets (#139).
fn cmd_skill(args: &[String]) -> Result<()> {
    use std::str::FromStr as _;

    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut positional: Vec<String> = Vec::new();
    let mut manual = false;
    let mut i = 0;
    while i < args.len() {
        let token = args[i].as_str();
        if token.starts_with("--") {
            if token == "--manual" {
                manual = true;
                i += 1;
                continue;
            }
            i += 1;
            let value = args
                .get(i)
                .cloned()
                .ok_or_else(|| Error::Validation(format!("flag {token} needs a value")))?;
            named.insert(token.to_string(), value);
            i += 1;
        } else {
            positional.push(args[i].clone());
            i += 1;
        }
    }
    let get = |key: &str| named.get(&format!("--{key}")).cloned();
    let require = |key: &str| -> Result<String> {
        get(key).ok_or_else(|| Error::Validation(format!("skill needs --{key}")))
    };
    let body_from_flags = || -> Result<String> {
        if let Some(path) = get("body-file") {
            return std::fs::read_to_string(&path)
                .map_err(|e| Error::Validation(format!("could not read --body-file {path}: {e}")));
        }
        get("body")
            .ok_or_else(|| Error::Validation("skill write needs --body-file or --body".into()))
    };
    let source_from_flags = || -> Result<viva::skills::SkillBodySource> {
        let execution = get("execution");
        if manual && execution.is_some() {
            return Err(Error::Validation(
                "Haisu manual edits use --manual only; do not pass --execution".into(),
            ));
        }
        if manual {
            return Ok(viva::skills::SkillBodySource::HaisuManual);
        }
        if let Some(id) = execution {
            if id.trim().is_empty() {
                return Err(Error::Validation(
                    "execution-sourced writes need a non-empty --execution".into(),
                ));
            }
            return Ok(viva::skills::SkillBodySource::Execution {
                execution_id: viva::foundation::ids::ExecutionId::from_str(&id)?,
            });
        }
        Err(Error::Validation(
            "skill writes need --execution <id> or --manual for Haisu edits".into(),
        ))
    };

    let store = open_store()?;
    let skills = viva::skills::SkillBodyStore::new(&store);
    match positional.first().map(String::as_str) {
        Some("create") => {
            let name = require("name")?;
            let body = body_from_flags()?;
            let source = source_from_flags()?;
            let current = skills.create(name, body, source)?;
            println!("{}", serde_json::to_string_pretty(&current)?);
            Ok(())
        }
        Some("write") => {
            let skill_id = require("skill")?;
            let body = body_from_flags()?;
            let source = source_from_flags()?;
            let current = skills.write_body(
                &skill_id,
                body,
                viva::skills::SkillBodyWriteKind::Modify,
                source,
            )?;
            println!("{}", serde_json::to_string_pretty(&current)?);
            Ok(())
        }
        Some("get") => {
            if get("member").is_some() {
                let member = viva::foundation::ids::MemberId::from_str(&require("member")?)?;
                let skill_id = require("skill")?;
                let execution =
                    viva::foundation::ids::ExecutionId::from_str(&require("execution")?)?;
                let sets = viva::knowledge::SkillSetService::new(&store);
                let read = sets.get_body(&member, &skill_id, &execution, 1.0)?;
                println!("{}", serde_json::to_string_pretty(&read)?);
            } else {
                let skill_id = require("skill")?;
                match skills.current_body(&skill_id)? {
                    Some(current) => println!("{}", serde_json::to_string_pretty(&current)?),
                    None => {
                        return Err(Error::NotFound {
                            entity: "team_skill",
                            id: skill_id,
                        });
                    }
                }
            }
            Ok(())
        }
        Some("list") => {
            let member = viva::foundation::ids::MemberId::from_str(&require("member")?)?;
            let sets = viva::knowledge::SkillSetService::new(&store);
            let knowledge = viva::knowledge::KnowledgeRegistry::new(&store);
            let rows = sets.list_member_skills(&member)?;
            let mut out = Vec::new();
            for m in rows {
                if let Some(skill) = knowledge.skill(&m.skill_id)? {
                    out.push(serde_json::json!({
                        "membership": m,
                        "skill": skill,
                        "derived_from": sets.skill_derived_from(&m.skill_id)?,
                    }));
                }
            }
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        Some("search") => {
            let member = viva::foundation::ids::MemberId::from_str(&require("member")?)?;
            let query = require("query")?;
            let team = get("team").is_some();
            let sets = viva::knowledge::SkillSetService::new(&store);
            let hits = sets.search(&member, &query, team)?;
            println!("{}", serde_json::to_string_pretty(&hits)?);
            Ok(())
        }
        Some("claim") => {
            let member = viva::foundation::ids::MemberId::from_str(&require("member")?)?;
            let skill_id = require("skill")?;
            let execution = get("execution")
                .map(|e| viva::foundation::ids::ExecutionId::from_str(&e))
                .transpose()?;
            let sets = viva::knowledge::SkillSetService::new(&store);
            let membership = sets.claim(&member, &skill_id, execution.as_ref())?;
            println!("{}", serde_json::to_string_pretty(&membership)?);
            Ok(())
        }
        Some("sync") => {
            let member = viva::foundation::ids::MemberId::from_str(&require("member")?)?;
            let skill_id = require("skill")?;
            let execution = get("execution")
                .map(|e| viva::foundation::ids::ExecutionId::from_str(&e))
                .transpose()?;
            let sets = viva::knowledge::SkillSetService::new(&store);
            if let (Some(source), Some(title), Some(entry)) =
                (get("derived-from"), get("title"), get("entry"))
            {
                let registration = sets.sync_derived(
                    &member,
                    &source,
                    title,
                    std::path::PathBuf::from(entry),
                    get("provenance").unwrap_or_else(|| "sync rewrite".into()),
                    execution.as_ref(),
                )?;
                println!("{}", serde_json::to_string_pretty(&registration)?);
            } else {
                let membership = sets.sync_existing(&member, &skill_id, execution.as_ref())?;
                println!("{}", serde_json::to_string_pretty(&membership)?);
            }
            Ok(())
        }
        Some("restore") => {
            let member = viva::foundation::ids::MemberId::from_str(&require("member")?)?;
            let skill_id = require("skill")?;
            let sets = viva::knowledge::SkillSetService::new(&store);
            let membership = sets.restore_manual(&member, &skill_id)?;
            println!("{}", serde_json::to_string_pretty(&membership)?);
            Ok(())
        }
        Some("weekly") => {
            let sets = viva::knowledge::SkillSetService::new(&store);
            let report = sets.weekly_report()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Some("versions") => {
            let skill_id = require("skill")?;
            let versions = skills.list_versions(&skill_id)?;
            println!("{}", serde_json::to_string_pretty(&versions)?);
            Ok(())
        }
        Some("version-get") => {
            let skill_id = require("skill")?;
            let version: u32 = require("version")?
                .parse()
                .map_err(|_| Error::Validation("--version must be a number".into()))?;
            match skills.version_body(&skill_id, version)? {
                Some(row) => println!("{}", serde_json::to_string_pretty(&row)?),
                None => {
                    return Err(Error::NotFound {
                        entity: "skill_body_version",
                        id: format!("{skill_id}@{version}"),
                    });
                }
            }
            Ok(())
        }
        Some("rollback") => {
            let skill_id = positional
                .get(1)
                .cloned()
                .or_else(|| get("skill"))
                .ok_or_else(|| {
                    Error::Validation("skill rollback needs <skill> <version>".into())
                })?;
            let version_text = positional
                .get(2)
                .cloned()
                .or_else(|| get("version"))
                .ok_or_else(|| {
                    Error::Validation("skill rollback needs <skill> <version>".into())
                })?;
            let version: u32 = version_text
                .parse()
                .map_err(|_| Error::Validation("rollback version must be a number".into()))?;
            let current = skills.rollback(&skill_id, version)?;
            println!("{}", serde_json::to_string_pretty(&current)?);
            Ok(())
        }
        Some("project") => {
            let skill_id = require("skill")?;
            let dest = require("out")?;
            skills.write_current_projection_file(&skill_id, std::path::Path::new(&dest))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "skill_id": skill_id,
                    "projected_to": dest,
                }))?
            );
            Ok(())
        }
        _ => Err(Error::Validation(
            "skill needs create | write | get | versions | version-get | rollback | project | \
             list | search | claim | sync | restore | weekly (#139 member set commands)"
                .into(),
        )),
    }
}

/// `viva memory …` (F04) — Viva's namespace/audit layer over the
/// real external memory. Search/remember/archive all go through the
/// Viva link layer: member scoping, provenance, usage evidence, and
/// recoverable exit (no delete). The adapter subprocess always receives
/// an explicit db path.
fn cmd_memory(args: &[String]) -> Result<()> {
    use std::str::FromStr as _;

    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut bool_flags: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut i = 1; // args[0] is the subcommand
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--emphasis" {
            bool_flags.insert(flag.to_string());
            i += 1;
            continue;
        }
        i += 1;
        let value = args
            .get(i)
            .cloned()
            .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
        named.insert(flag.to_string(), value);
        i += 1;
    }
    let get = |key: &str| named.get(key).cloned();
    let require = |key: &str| -> Result<String> {
        get(key).ok_or_else(|| Error::Validation(format!("memory needs --{key}")))
    };
    let emphasis = bool_flags.contains("--emphasis");

    let store = open_store()?;
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
            let fact_id = require("--fact")?;
            let link = service.link_fact(
                &fact_id,
                &member_id,
                project.as_ref(),
                &require("--source")?,
            )?;
            println!("{}", serde_json::to_string_pretty(&link)?);
            Ok(())
        }
        Some("remember") => {
            let member_id = viva::foundation::ids::MemberId::from_str(&require("--member")?)?;
            let project = get("--project")
                .map(|p| viva::foundation::ids::ProjectId::from_str(&p))
                .transpose()?;
            let kind = match get("--kind").unwrap_or_else(|| "personal_memory".into()).as_str() {
                "project_knowledge" => viva::memory::KnowledgeKind::ProjectKnowledge,
                "team_knowledge" => viva::memory::KnowledgeKind::TeamKnowledge,
                _ => viva::memory::KnowledgeKind::PersonalMemory,
            };
            let result = service.remember_with_options(
                &member_id,
                project.as_ref(),
                &require("--content")?,
                &require("--source")?,
                &get("--category").unwrap_or_else(|| "general".into()),
                &get("--tags").unwrap_or_default(),
                kind,
                emphasis,
                get("--provenance").as_deref(),
                get("--summary").as_deref(),
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Some("admit") => {
            let result = service.admit_candidate(&require("--candidate")?)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Some("curate") => {
            let member_id = viva::foundation::ids::MemberId::from_str(&require("--member")?)?;
            let project = get("--project")
                .map(|p| viva::foundation::ids::ProjectId::from_str(&p))
                .transpose()?;
            let path = require("--turns-file")?;
            let raw = std::fs::read_to_string(&path)?;
            let turns: Vec<viva::memory::SessionTurn> = serde_json::from_str(&raw)
                .map_err(|e| Error::Validation(format!("turns-file JSON: {e}")))?;
            let provenance = get("--provenance").unwrap_or_else(|| "{}".into());
            let outcome = service.curate_session(
                &member_id,
                project.as_ref(),
                &turns,
                &provenance,
                emphasis,
            )?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            Ok(())
        }
        Some("record-use") => {
            let member_id = viva::foundation::ids::MemberId::from_str(&require("--member")?)?;
            service.record_execution_usage(
                &require("--fact")?,
                &member_id,
                &require("--execution")?,
                &get("--note").unwrap_or_default(),
            )?;
            println!("{}", serde_json::json!({ "ok": true }));
            Ok(())
        }
        Some(cmd @ ("archive" | "restore")) => {
            let fact_id = require("--fact")?;
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
                service.archive(&fact_id, &require("--reason")?, &actor, &authority)?
            } else {
                service.restore(&fact_id, &require("--reason")?, &actor, &authority)?
            };
            println!("{}", serde_json::to_string_pretty(&link)?);
            Ok(())
        }
        Some("status") => {
            let home = viva_home(None);
            let probe = service.native_status()?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "storage": "native",
                    "database": database_path(&home).display().to_string(),
                    "probe": probe,
                }))?
            );
            Ok(())
        }
        _ => Err(Error::Validation(
            "memory needs search | probe | link | remember | admit | curate | record-use | archive | restore | status".into(),
        )),
    }
}

/// `viva workflow …` (F01) — the reachable entry for delivery workflows.
/// The run head is anchored to the REAL git head of the given directory
/// (rev-parse through Viva's git runner), never self-reported.
/// `status` always shows the owning task's own status beside the run's
/// process progress: the run records process facts, the task keeps the
/// delivery state, and completion stays an appended task outcome.
fn cmd_workflow(args: &[String]) -> Result<()> {
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
                .ok_or_else(|| Error::Validation(format!("flag {arg} needs a value")))?;
            named.insert(flag.to_string(), value);
        } else {
            flags.push(arg.to_string());
        }
        i += 1;
    }
    let get = |key: &str| named.get(key).cloned();
    let require = |key: &str| -> Result<String> {
        get(key).ok_or_else(|| Error::Validation(format!("workflow needs --{key}")))
    };

    let store = open_store()?;
    let engine = viva::workflows::WorkflowEngine::new(&store);
    let resolve_head = |cwd: &str| -> Result<String> {
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
                Error::Validation(format!("git status failed in {cwd}: {failure}"))
            })?;
        if !status.success() || !status.stdout.trim().is_empty() {
            return Err(Error::Validation(format!(
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
                        return Err(Error::Validation(format!(
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
                    return Err(Error::Validation(format!(
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
        _ => Err(Error::Validation(
            "workflow needs register | start-run | record | resume | abort | status | history"
                .into(),
        )),
    }
}

/// `viva maintenance …` (F02) — the reachable entry for runtime knowledge
/// review and repository maintenance. Each review command opens one
/// bounded session and closes it: maintenance runs only inside such a
/// window, never as a daemon.
fn cmd_maintenance(args: &[String]) -> Result<()> {
    use std::str::FromStr as _;

    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut i = 1;
    while i < args.len() {
        let flag = args[i].as_str();
        i += 1;
        let value = args
            .get(i)
            .cloned()
            .ok_or_else(|| Error::Validation(format!("flag {flag} needs a value")))?;
        named.insert(flag.to_string(), value);
        i += 1;
    }
    let get = |key: &str| named.get(&format!("--{key}")).cloned();
    let require = |key: &str| -> Result<String> {
        get(key).ok_or_else(|| Error::Validation(format!("maintenance needs --{key}")))
    };
    let print_report = |report: &viva::maintenance::ScanReport| -> Result<()> {
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

    let store = open_store()?;
    // S6: an explicitly paused server runs no maintenance windows (the
    // pause persists, so this holds with no live host either).
    viva::server::assert_maintenance_allowed(&store)?;
    let maintenance = viva::maintenance::MaintenanceService::new(&store);
    match args.first().map(String::as_str) {
        Some("review-knowledge") => {
            let stale_days: u32 = match get("stale-days") {
                Some(text) => text
                    .parse()
                    .map_err(|_| {
                        Error::Validation("--stale-days must be a number".to_string())
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
        _ => Err(Error::Validation(
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
fn cmd_data(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("export") => cmd_data_export(args.get(1..).unwrap_or(&[])),
        _ => Err(Error::Validation("data needs `export --out <dir>`".into())),
    }
}

fn cmd_data_export(args: &[String]) -> Result<()> {
    let mut out_dir = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out_dir = args.get(i).cloned();
            }
            other => {
                return Err(Error::Validation(format!("unknown export flag `{other}`")));
            }
        }
        i += 1;
    }
    let Some(out_dir) = out_dir else {
        return Err(Error::Validation(
            "usage: viva data export --out <dir>".into(),
        ));
    };
    let out_path = std::path::PathBuf::from(out_dir);
    if out_path.exists() {
        return Err(Error::Validation(format!(
            "refusing to overwrite an existing export directory: {}",
            out_path.display()
        )));
    }

    let home = viva::foundation::paths::viva_home(None);
    let db = database_path(&home);
    if !db.exists() {
        return Err(Error::Validation(format!(
            "no Viva store at {} (run `viva init` first); nothing was created",
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
    eprintln!("warning: raw, unredacted export; keep this private and do not publish it");

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

fn write_private_export_json(path: &std::path::Path, value: &serde_json::Value) -> Result<()> {
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

/// One owner-only workspace request over the real channel.
fn workspace_call(args: &[String]) -> Result<serde_json::Value> {
    let home = viva_home(None);
    let mut client = viva::server::Client::ensure_server(&home)?;
    let action = args
        .first()
        .map(String::as_str)
        .unwrap_or("list")
        .to_string();
    let mut value = args.get(1).cloned();
    if matches!(action.as_str(), "open" | "add" | "save") {
        if let Some(v) = &value {
            value = Some(
                viva::workspaces::navigation::absolutize(v, &std::env::current_dir()?)
                    .display()
                    .to_string(),
            );
        }
    }
    let project_id = if action == "remove" {
        value.take()
    } else {
        None
    };
    client.call(viva::server::RequestKind::Workspace {
        action,
        value,
        project_id,
    })
}

fn peel_machine_flag(args: &[String]) -> Result<(Option<String>, Vec<String>)> {
    let mut machine = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--machine" {
            if machine.is_some() {
                return Err(Error::Validation("--machine was given twice".into()));
            }
            let id = args.get(i + 1).ok_or_else(|| {
                Error::Validation("--machine needs the id from `viva machine list`".into())
            })?;
            if id.starts_with('-') {
                return Err(Error::Validation("--machine needs an id".into()));
            }
            machine = Some(id.clone());
            i += 2;
            continue;
        }
        rest.push(args[i].clone());
        i += 1;
    }
    Ok((machine, rest))
}

fn arm_machine(id: &str, args: &[String]) -> Result<viva::machines::InvocationGuard> {
    match args.first().map(String::as_str) {
        None | Some("workbench" | "open") => {
            return Err(Error::Validation(
                "the workbench always uses this computer's socket; remote machines stay in the sidebar, and --machine is only for an explicit command".into(),
            ));
        }
        Some("machine") => {
            return Err(Error::Validation(
                "machine catalog commands always use this computer; omit --machine".into(),
            ));
        }
        Some("init" | "doctor" | "update" | "server" | "start" | "server-restart") => {
            return Err(Error::Validation(
                "--machine does not change this computer's socket and does not install or start viva on the remote; omit --machine for this command".into(),
            ));
        }
        _ => {}
    }
    let home = viva_home(None);
    let record = viva::machines::require(&home, id)?;
    if !record.enabled {
        return Err(Error::Validation(format!(
            "machine {} is disabled; input is refused",
            record.label
        )));
    }
    Ok(viva::machines::InvocationGuard::arm(record))
}

fn cmd_machine(args: &[String]) -> Result<()> {
    let home = viva_home(None);
    match args.first().map(String::as_str) {
        Some("list") | None => {
            let machines = viva::machines::load(&home)?;
            println!("{}", serde_json::to_string_pretty(&machines)?);
            Ok(())
        }
        Some("add") => {
            let mut target = None;
            let mut label = None;
            let mut id = None;
            let mut session = None;
            let mut i = 1;
            while i < args.len() {
                let flag = args[i].as_str();
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| Error::Validation(format!("{flag} needs a value")))?;
                match flag {
                    "--target" => target = Some(value.clone()),
                    "--label" => label = Some(value.clone()),
                    "--id" => id = Some(value.clone()),
                    "--session" => session = Some(value.clone()),
                    other => {
                        return Err(Error::Validation(format!(
                            "unknown machine add flag `{other}`"
                        )));
                    }
                }
                i += 2;
            }
            let target = target.ok_or_else(|| {
                Error::Validation("machine add needs --target <user@host>".into())
            })?;
            let record = viva::machines::add(
                &home,
                id.as_deref(),
                label.as_deref(),
                &target,
                session.as_deref(),
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
            Ok(())
        }
        Some("rename") => {
            let id = args
                .get(1)
                .ok_or_else(|| Error::Validation("machine rename needs <id> <label>".into()))?;
            let label = args
                .get(2)
                .ok_or_else(|| Error::Validation("machine rename needs <id> <label>".into()))?;
            let record = viva::machines::rename(&home, id, label)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
            Ok(())
        }
        Some("disable") => {
            let id = args
                .get(1)
                .ok_or_else(|| Error::Validation("machine disable needs <id>".into()))?;
            let record = viva::machines::set_enabled(&home, id, false)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
            Ok(())
        }
        Some("enable") => {
            let id = args
                .get(1)
                .ok_or_else(|| Error::Validation("machine enable needs <id>".into()))?;
            let record = viva::machines::set_enabled(&home, id, true)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
            Ok(())
        }
        Some("remove") => {
            let id = args
                .get(1)
                .ok_or_else(|| Error::Validation("machine remove needs <id>".into()))?;
            viva::machines::remove(&home, id)?;
            println!("{{\"removed\":\"{id}\"}}");
            Ok(())
        }
        Some("bridge") => {
            let mut session = None;
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--session" {
                    session = Some(args.get(i + 1).ok_or_else(|| {
                        Error::Validation("machine bridge needs --session <name>".into())
                    })?);
                    i += 2;
                    continue;
                }
                return Err(Error::Validation(format!(
                    "unknown machine bridge flag `{}`",
                    args[i]
                )));
            }
            viva::machines::run_bridge(session.map(String::as_str).unwrap_or("default"))
        }
        Some(other) => Err(Error::Validation(format!(
            "unknown machine command `{other}` (add, list, rename, disable, enable, remove)"
        ))),
    }
}

/// `viva workspace [list|open|new|select|add|remove|save|refresh] [value]`.
fn cmd_workspace(args: &[String]) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&workspace_call(args)?)?);
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

    #[test]
    fn machine_flag_is_removed_without_eating_the_command() {
        let args: Vec<String> = ["--machine", "lab", "status"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (id, rest) = peel_machine_flag(&args).unwrap();
        assert_eq!(id.as_deref(), Some("lab"));
        assert_eq!(rest, vec!["status".to_string()]);
        let (id, rest) =
            peel_machine_flag(&["status".into(), "--machine".into(), "other".into()]).unwrap();
        assert_eq!(id.as_deref(), Some("other"));
        assert_eq!(rest, vec!["status".to_string()]);
        assert!(peel_machine_flag(&["--machine".into()]).is_err());
    }

    #[test]
    fn integration_root_may_appear_before_agent_name() {
        let (name, root) = parse_integration_name_and_root(&[
            "--root".to_string(),
            "/tmp/home".to_string(),
            "pi".to_string(),
        ])
        .expect("parse");
        assert_eq!(name, "pi");
        assert_eq!(root.as_deref(), Some("/tmp/home"));

        let (name, root) = parse_integration_name_and_root(&[
            "pi".to_string(),
            "--root".to_string(),
            "/tmp/home".to_string(),
        ])
        .expect("parse");
        assert_eq!(name, "pi");
        assert_eq!(root.as_deref(), Some("/tmp/home"));
    }
}
