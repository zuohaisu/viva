//! #191 P1: public domain CLI routes to the single native PTY owner.
//! Reuses the existing JSON API, canonical registries and controller admission.
//! Member effects not integrated here remain refused (P2), never legacy-routed.
use super::*;
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn socket(home: &Path) -> PathBuf {
    home.join("runtime/workbench/herdr.sock")
}

/// Audit fixed command names only; never argv, credentials or report text.
pub fn refuse_unintegrated_command(command: &str) -> Result<()> {
    let home = paths::viva_home(None);
    if paths::database_path(&home).exists() {
        let store =
            Store::attach_for_handoff(&paths::database_path(&home), crate::server::migrations())?;
        events::append(
            &store,
            NewEvent {
                domain: crate::server::DOMAIN_HOST,
                kind: "native_cli_refused".into(),
                subject_type: "native_ingress".into(),
                subject_id: uuid::Uuid::new_v4().to_string(),
                origin: "native_cli:unbound".into(),
                payload: json!({"command": command, "peer_pid": std::process::id(), "reason": "authority_not_integrated", "human_presence_verified": false}),
            },
        )?;
    }
    Err(Error::Validation(format!(
        "native {command} is not integrated (#191 P2/P3); no legacy host was started"
    )))
}

fn local_only() -> Result<()> {
    if crate::machines::invocation_target().is_some() {
        return Err(Error::Validation(
            "native remote control is not integrated; refusing local or legacy fallback".into(),
        ));
    }
    Ok(())
}

fn snapshot(home: &Path) -> Result<Value> {
    Ok(send_native_request(&socket(home), "viva.snapshot", json!({}))?["snapshot"].clone())
}

fn terminals(home: &Path, state: &Value) -> Result<Value> {
    let value = send_native_request(&socket(home), "pane.list", json!({}))?;
    let rows = value["panes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|pane| {
            let binding = state["terminals"].as_array().and_then(|rows| {
                rows.iter().find(|binding| {
                    binding["pane_id"] == pane["pane_id"]
                        && binding["workspace_id"] == pane["workspace_id"]
                })
            });
            json!({"terminal_id": pane["pane_id"], "native_terminal_id": pane["terminal_id"],
            "workspace_id": pane["workspace_id"], "owner": binding.map(|row| &row["owner"]),
            "execution_id": binding.map(|row| &row["execution_id"]),
            "purpose": pane["label"], "worktree_id": null})
        })
        .collect::<Vec<_>>();
    Ok(json!(rows))
}

fn effect(home: &Path, method: &str, params: Value) -> Result<Value> {
    authenticate_owner(&socket(home), home)?;
    send_native_request(&socket(home), method, params)
}

fn ensure_host(home: &Path) -> Result<()> {
    if snapshot(home).is_ok() {
        return Ok(());
    }
    owner_anchor::prepare_home(home)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("native-server.log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["runtime", "server"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    herdr::embedding::configure_host_daemon(&mut command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if snapshot(home).is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(Error::Validation(format!(
                "native host exited ({status}); inspect {} and retry; no legacy UI fallback",
                home.join("native-server.log").display()
            )));
        }
        if Instant::now() >= deadline {
            return Err(Error::Validation("native host readiness timed out; inspect native-server.log; no processes stopped or commands replayed".into()));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
pub fn authorize_domain_write() -> Result<()> {
    local_only()?;
    if [
        "VIVA_MEMBER_ID",
        "VIVA_EXECUTION_ID",
        "VIVA_GRANT_ID",
        "VIVA_WORKER_CREDENTIAL",
        "VIVA_MEMBER_CREDENTIAL",
        "VIVA_MEMBER_CHANNEL",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some())
    {
        return refuse_unintegrated_command("domain_write");
    }
    let home = paths::viva_home(None);
    if socket(&home).exists() {
        authenticate_owner(&socket(&home), &home)?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn authorize_domain_write() -> Result<()> {
    local_only()
}

/// Same Navigation contract as the former control surface, with no legacy Host.
#[cfg(unix)]
pub fn workspace_command(args: &[String]) -> Result<Value> {
    local_only()?;
    let home = paths::viva_home(None);
    let action = args.first().map(String::as_str).unwrap_or("list");
    if args.len() > 2 {
        return Err(Error::Validation(
            "workspace expects one action and at most one value".into(),
        ));
    }
    let mut value = args.get(1).cloned();
    if matches!(action, "open" | "add" | "save") {
        if let Some(path) = &value {
            value = Some(
                crate::workspaces::navigation::absolutize(path, &std::env::current_dir()?)
                    .display()
                    .to_string(),
            );
        }
    }
    ensure_host(&home)?;
    let reply = effect(
        &home,
        "viva.workspace",
        json!({"action": action, "value": value}),
    )?;
    Ok(reply["workspace"].clone())
}

#[cfg(unix)]
pub fn run_domain_command(args: &[String]) -> Result<()> {
    local_only()?;
    let home = paths::viva_home(None);
    let command = args.first().map(String::as_str).unwrap_or("");
    let result = match command {
        "server" | "start" => {
            if args.len() != 1 {
                return Err(Error::Validation("native server does not accept legacy --resume; native restart integration is tracked by #191 P3".into()));
            }
            return super::run(&["server".into()]);
        }
        "status" => match snapshot(&home) {
            Ok(mut value) => {
                value["terminals"] = terminals(&home, &value)?;
                value
            }
            Err(error) if socket(&home).exists() => return Err(error),
            Err(error) => {
                eprintln!("native live channel unavailable: {error}");
                crate::server::offline_status(&home)?
            }
        },
        "pause" | "resume" => effect(
            &home,
            "viva.host.pause",
            json!({"paused": command == "pause"}),
        )?,
        "dispatch" => {
            let params = dispatch_params(&args[1..])?;
            let result = if member_context() {
                member_effect(&home, json!({"action": "dispatch", "launch": params}))?
            } else {
                effect(&home, "viva.execution.start", params)?
            };
            execution_reply(&home, result)?
        }
        "terminals" => {
            json!({"terminals": terminals(&home, &snapshot(&home)?)?})
        }
        "stop-terminal" => {
            let target = args.get(1).ok_or_else(|| {
                Error::Validation("usage: viva stop-terminal <terminal-id>".into())
            })?;
            let state = snapshot(&home)?;
            let binding = state["terminals"]
                .as_array()
                .and_then(|rows| {
                    rows.iter()
                        .find(|row| row["terminal_id"].as_str() == Some(target))
                })
                .ok_or_else(|| {
                    Error::Validation(
                        "terminal is not a bound execution owned by this host; no process stopped"
                            .into(),
                    )
                })?;
            effect(
                &home,
                "viva.execution.stop",
                json!({"execution_id": binding["execution_id"]}),
            )?
        }
        "result" => {
            let task = args
                .get(1)
                .ok_or_else(|| Error::Validation("usage: viva result <task-id>".into()))?;
            let task = TaskId::from_str(task)?;
            let store = Store::attach_for_handoff(
                &paths::database_path(&home),
                crate::server::migrations(),
            )?;
            serde_json::to_value(TaskRegistry::new(&store).results_for_task(&task)?)?
        }
        "shutdown" => {
            // The host serializes canonical stop + shutdown with dispatch.
            // No client-side PID killing or snapshot/stop race.
            if std::env::var("VIVA_CLOSE_POLICY").as_deref() == Ok("pause") {
                effect(&home, "viva.host.pause", json!({"paused": true}))?;
            }
            effect(&home, "server.stop", json!({}))?
        }
        "handoff" => {
            let mut params = json!({"action": "handoff"});
            let mut index = 1;
            while index < args.len() {
                let key = match args[index].as_str() {
                    "--task" => "task_id",
                    "--member" => "member_id",
                    "--summary" => "detail",
                    _ => {
                        return Err(Error::Validation(
                            "handoff needs --task, --member, --summary".into(),
                        ));
                    }
                };
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| Error::Validation("handoff flag needs a value".into()))?;
                params[key] = json!(value);
                index += 2;
            }
            member_effect(&home, params)?
        }
        "server-restart" => return refuse_unintegrated_command(command),
        // Handoff/member-initiated orchestration remains P2, not an owner CLI
        // shortcut that discards the actual member process and its live grant.
        _ => return refuse_unintegrated_command(command),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn execution_reply(home: &Path, mut result: Value) -> Result<Value> {
    // A locator is recovered only from the serving host's current binding.
    result["terminal_id"] = result["pane_id"].clone();
    if result["terminal_id"].is_null() && result["intent_state"] == "confirmed" {
        let state = snapshot(home)?;
        if let Some(binding) = state["terminals"].as_array().and_then(|rows| {
            rows.iter()
                .find(|row| row["execution_id"] == result["execution_id"])
        }) {
            result["terminal_id"] = binding["pane_id"].clone();
        }
    }
    Ok(result)
}

fn member_context() -> bool {
    [
        "VIVA_MEMBER_ID",
        "VIVA_EXECUTION_ID",
        "VIVA_GRANT_ID",
        "VIVA_MEMBER_CREDENTIAL",
        "VIVA_MEMBER_CHANNEL",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some())
}

fn member_effect(home: &Path, mut params: Value) -> Result<Value> {
    params["credential"] = json!(std::env::var("VIVA_MEMBER_CREDENTIAL").ok().filter(|value| !value.is_empty()).ok_or_else(|| {
                    let _ = refuse_unintegrated_command("member.effect.missing_credential");
                    Error::Validation("member effect requires the host-issued VIVA_MEMBER_CREDENTIAL; no controller fallback".into())
                })?);
    params["grant_id"] = json!(std::env::var("VIVA_GRANT_ID").ok());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| Error::Validation("member confirmation timed out".into()))?;
        match send_native_request_with_timeout(
            &socket(home),
            "viva.member.effect",
            params.clone(),
            remaining,
        ) {
            Err(Error::Validation(message))
                if message.starts_with("viva.member.effect failed (member_not_ready):")
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(25))
            }
            result => return result,
        }
    }
}

fn dispatch_params(args: &[String]) -> Result<Value> {
    let mut params = json!({});
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        if flag == "--" {
            params["argv"] = json!(&args[index + 1..]);
            break;
        }
        let key = match flag.as_str() {
            "--task" => "task_id",
            "--member" => "member_id",
            "--grant" => "grant_id",
            "--request-key" => "request_key",
            "--cwd" => "cwd",
            "--worktree" => "worktree_id",
            _ => return Err(Error::Validation(format!("unknown dispatch flag {flag}"))),
        };
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::Validation(format!("{flag} needs a value")))?;
        if params.get(key).is_some() {
            return Err(Error::Validation(format!("duplicate dispatch flag {flag}")));
        }
        params[key] = json!(value);
        index += 2;
    }
    if ["task_id", "member_id", "grant_id", "request_key", "cwd"]
        .iter()
        .any(|key| params.get(key).is_none())
        || params["argv"].as_array().is_none_or(|argv| argv.is_empty())
    {
        return Err(Error::Validation("usage: viva dispatch --task <id> --member <id> --grant <id> --request-key <key> --cwd <dir> [--worktree <id>] -- argv...".into()));
    }
    Ok(params)
}

#[cfg(not(unix))]
pub fn run_domain_command(_args: &[String]) -> Result<()> {
    Err(Error::Validation(
        "native domain control is not supported on this platform".into(),
    ))
}
#[cfg(not(unix))]
pub fn workspace_command(_args: &[String]) -> Result<Value> {
    Err(Error::Validation(
        "native navigation is not supported on this platform".into(),
    ))
}
