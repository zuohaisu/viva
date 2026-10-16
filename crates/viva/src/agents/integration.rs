//! Optional per-agent integrations.
//!
//! `install` writes one file at that agent's documented config location.
//! `uninstall` deletes that file when it is the file Viva wrote, and leaves
//! every other file alone. The hook reports status, a native session
//! reference, and a resume command through `viva agent report`. It does
//! not complete tasks, write QA, or promote anything into memory.
//!
//! The file bodies are compiled in. Nothing is downloaded.

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::foundation::error::{Error, Result};

pub const SUPPORTED: &[&str] = &[
    "pi", "claude", "codex", "opencode", "kimi", "qodercli", "cursor", "grok", "hermes",
];

const MARKER: &str = "viva-integration";

/// Python the hook runs. It reads the agent's JSON on stdin and reports
/// only when `VIVA_TERMINAL_ID` is set (a pane Viva spawned). No single
/// quotes: the command wraps this in `exec("""...""")`.
const REPORT_PY: &str = r#"import json,os,sys,subprocess
status,agent,prefix=sys.argv[1:4]
raw=sys.stdin.read()
data={}
try:
    parsed=json.loads(raw) if raw.strip() else {}
    data=parsed if isinstance(parsed, dict) else {}
except Exception:
    data={}
props=data.get("properties") if isinstance(data.get("properties"), dict) else {}
cands=[data.get(k) for k in ("session_id","sessionId","thread_id","conversation_id","conversationId","thread-id")]
cands=cands+[props.get(k) for k in ("sessionID","session_id","id")]
session=""
for value in cands:
    if isinstance(value, str) and value:
        session=value
        break
terminal=os.environ.get("VIVA_TERMINAL_ID", "")
if not terminal:
    raise SystemExit(0)
cmd=["viva","agent","report","--terminal",terminal,"--agent",agent,"--status",status]
if session:
    cmd.extend(["--session",session,"--resume",(prefix+" "+session).strip()])
subprocess.run(cmd)
"#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationStatus {
    pub name: String,
    pub installed: bool,
    pub path: String,
    pub detail: String,
}

pub fn install(name: &str, home: &Path) -> Result<IntegrationStatus> {
    require_macos()?;
    let spec = spec_for(name)?;
    let path = home.join(spec.relative);
    if path.exists() && !is_ours(&path)? {
        return Err(Error::Validation(format!(
            "refusing to replace {} (not a Viva integration file)",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, spec.body)?;
    Ok(IntegrationStatus {
        name: spec.name.to_string(),
        installed: true,
        path: path.display().to_string(),
        detail: "wrote the Viva integration file".into(),
    })
}

pub fn uninstall(name: &str, home: &Path) -> Result<IntegrationStatus> {
    require_macos()?;
    let spec = spec_for(name)?;
    let path = home.join(spec.relative);
    if !path.exists() {
        return Ok(IntegrationStatus {
            name: spec.name.to_string(),
            installed: false,
            path: path.display().to_string(),
            detail: "already absent".into(),
        });
    }
    if !is_ours(&path)? {
        return Err(Error::Validation(format!(
            "refusing to delete {} (not a Viva integration file)",
            path.display()
        )));
    }
    fs::remove_file(&path)?;
    Ok(IntegrationStatus {
        name: spec.name.to_string(),
        installed: false,
        path: path.display().to_string(),
        detail: "deleted the Viva integration file".into(),
    })
}

pub fn status(name: &str, home: &Path) -> Result<IntegrationStatus> {
    require_macos()?;
    let spec = spec_for(name)?;
    let path = home.join(spec.relative);
    if !path.exists() {
        return Ok(IntegrationStatus {
            name: spec.name.to_string(),
            installed: false,
            path: path.display().to_string(),
            detail: "not installed".into(),
        });
    }
    if is_ours(&path)? {
        return Ok(IntegrationStatus {
            name: spec.name.to_string(),
            installed: true,
            path: path.display().to_string(),
            detail: "Viva integration file is present".into(),
        });
    }
    Ok(IntegrationStatus {
        name: spec.name.to_string(),
        installed: false,
        path: path.display().to_string(),
        detail: "a different file is at the integration path; Viva will not change it".into(),
    })
}

fn require_macos() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(Error::Validation("viva integration is macOS-only".into()))
    }
}

fn is_ours(path: &Path) -> Result<bool> {
    let text = fs::read_to_string(path)?;
    Ok(text.contains(MARKER))
}

struct Spec {
    name: &'static str,
    relative: &'static str,
    body: String,
}

fn spec_for(name: &str) -> Result<Spec> {
    let key = name.trim().to_ascii_lowercase();
    let Some(name) = SUPPORTED
        .iter()
        .copied()
        .find(|supported| *supported == key)
    else {
        return Err(Error::Validation(format!(
            "integration `{name}` is not supported; supported: {}",
            SUPPORTED.join(", ")
        )));
    };
    let (relative, body) = match name {
        "pi" => (".pi/agent/extensions/viva-status.ts", pi_body()),
        "claude" => (
            ".claude/settings.json",
            hook_json("claude", "claude --resume", HookShape::Claude),
        ),
        "codex" => (
            ".codex/hooks.json",
            hook_json("codex", "codex resume", HookShape::Codex),
        ),
        "opencode" => (".config/opencode/plugins/viva.js", opencode_body()),
        "kimi" => (".kimi-code/config.toml", kimi_toml()),
        "qodercli" => (
            ".qoder/settings.json",
            hook_json("qodercli", "qodercli --resume", HookShape::Claude),
        ),
        "cursor" => (".cursor/hooks.json", cursor_json()),
        "grok" => (
            ".grok/hooks/viva.json",
            hook_json("grok", "grok -r", HookShape::Grok),
        ),
        "hermes" => (".hermes/config.yaml", hermes_yaml()),
        _ => unreachable!("supported name"),
    };
    Ok(Spec {
        name,
        relative,
        body,
    })
}

#[derive(Clone, Copy)]
enum HookShape {
    Claude,
    Codex,
    Grok,
}

fn report_command(status: &str, agent: &str, resume_prefix: &str) -> String {
    let mut command = String::from("python3 -c 'exec(\"\"\"");
    command.push_str(REPORT_PY);
    command.push_str("\"\"\")' ");
    command.push_str(status);
    command.push(' ');
    command.push_str(agent);
    command.push_str(" '");
    command.push_str(resume_prefix);
    command.push('\'');
    command
}

fn hook_group(command: &str) -> serde_json::Value {
    serde_json::json!([{
        "hooks": [{ "type": "command", "command": command }]
    }])
}

fn hook_json(agent: &str, resume_prefix: &str, shape: HookShape) -> String {
    let mut hooks = serde_json::Map::new();
    match shape {
        HookShape::Claude => {
            hooks.insert(
                "SessionStart".into(),
                hook_group(&report_command("idle", agent, resume_prefix)),
            );
            hooks.insert(
                "UserPromptSubmit".into(),
                hook_group(&report_command("working", agent, resume_prefix)),
            );
            hooks.insert(
                "Stop".into(),
                hook_group(&report_command("done", agent, resume_prefix)),
            );
            hooks.insert(
                "Notification".into(),
                serde_json::json!([{
                    "matcher": "permission_prompt",
                    "hooks": [{
                        "type": "command",
                        "command": report_command("blocked", agent, resume_prefix)
                    }]
                }]),
            );
        }
        HookShape::Codex => {
            hooks.insert(
                "SessionStart".into(),
                hook_group(&report_command("idle", agent, resume_prefix)),
            );
            hooks.insert(
                "UserPromptSubmit".into(),
                hook_group(&report_command("working", agent, resume_prefix)),
            );
            hooks.insert(
                "Stop".into(),
                hook_group(&report_command("done", agent, resume_prefix)),
            );
            hooks.insert(
                "PermissionRequest".into(),
                hook_group(&report_command("blocked", agent, resume_prefix)),
            );
        }
        HookShape::Grok => {
            hooks.insert(
                "SessionStart".into(),
                hook_group(&report_command("idle", agent, resume_prefix)),
            );
            hooks.insert(
                "Stop".into(),
                hook_group(&report_command("done", agent, resume_prefix)),
            );
        }
    }
    serde_json::to_string_pretty(&serde_json::json!({
        "viva_integration": MARKER,
        "hooks": hooks,
    }))
    .unwrap_or_else(|_| "{}".into())
}

fn cursor_json() -> String {
    let stop = report_command("done", "cursor", "cursor-agent --resume");
    let start = report_command("idle", "cursor", "cursor-agent --resume");
    serde_json::to_string_pretty(&serde_json::json!({
        "version": 1,
        "viva_integration": MARKER,
        "hooks": {
            "sessionStart": [{ "command": start }],
            "stop": [{ "command": stop }]
        }
    }))
    .unwrap_or_else(|_| "{}".into())
}

fn quote_double(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn kimi_toml() -> String {
    let done = quote_double(&report_command("done", "kimi", "kimi --resume"));
    let blocked = quote_double(&report_command("blocked", "kimi", "kimi --resume"));
    format!(
        "# {MARKER}\n[[hooks]]\nevent = \"Stop\"\ncommand = {done}\n\n[[hooks]]\nevent = \"PermissionRequest\"\ncommand = {blocked}\n"
    )
}

fn hermes_yaml() -> String {
    let working = quote_double(&report_command("working", "hermes", "hermes --resume"));
    let done = quote_double(&report_command("done", "hermes", "hermes --resume"));
    format!(
        "# {MARKER}\nhooks:\n  pre_llm_call:\n    - command: {working}\n  post_llm_call:\n    - command: {done}\n"
    )
}

fn pi_body() -> String {
    format!(
        r#"// {MARKER}
// Reports Pi lifecycle events through Pi's own argv exec. Does not write tasks, QA, or memory.
function sessionRef(ctx) {{
  try {{
    const file = ctx?.sessionManager?.getSessionFile?.();
    return typeof file === "string" ? file : "";
  }} catch {{
    return "";
  }}
}}

function report(pi, status, session) {{
  const terminal = process.env.VIVA_TERMINAL_ID;
  if (!terminal) return;
  const args = ["agent", "report", "--terminal", terminal, "--agent", "pi", "--status", status];
  if (session) {{
    args.push("--session", session, "--resume", "pi --resume " + session);
  }}
  void pi.exec("viva", args);
}}

export default function vivaStatus(pi) {{
  pi.on("session_start", (_event, ctx) => report(pi, "idle", sessionRef(ctx)));
  pi.on("before_agent_start", (_event, ctx) => report(pi, "working", sessionRef(ctx)));
  pi.on("agent_end", (_event, ctx) => report(pi, "done", sessionRef(ctx)));
  pi.on("ui_prompt_start", (_event, ctx) => report(pi, "blocked", sessionRef(ctx)));
}}
"#
    )
}

fn opencode_body() -> String {
    format!(
        r#"// {MARKER}
// OpenCode plugin. Reports session events through the plugin argv helper.
export const VivaStatus = async (ctx) => ({{
  event: async ({{ event }}) => {{
    const terminal = process.env.VIVA_TERMINAL_ID;
    if (!terminal || !ctx.$) return;
    const props = event?.properties || {{}};
    const info = props.info || {{}};
    const session = props.sessionID || props.session_id || info.id || "";
    let status = "";
    if (event?.type === "session.idle") status = "idle";
    if (event?.type === "session.status" && props.status?.type === "busy") status = "working";
    if (event?.type === "permission.asked") status = "blocked";
    if (!status) return;
    const args = ["agent", "report", "--terminal", terminal, "--agent", "opencode", "--status", status];
    if (session) args.push("--session", session, "--resume", "opencode --session " + session);
    await ctx.$("viva " + args.map((part) => JSON.stringify(part)).join(" "));
  }},
}});
"#
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn install_and_uninstall_use_a_temp_home_and_leave_other_files() {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path();
        let sibling = home.join(".pi/agent/extensions/other.ts");
        fs::create_dir_all(sibling.parent().expect("parent")).expect("dirs");
        fs::write(&sibling, "export default function other() {}\n").expect("sibling");

        let installed = install("pi", home).expect("install");
        assert!(installed.installed);
        let path = PathBuf::from(&installed.path);
        assert!(path.ends_with(".pi/agent/extensions/viva-status.ts"));
        let body = fs::read_to_string(&path).expect("read");
        assert!(body.contains(MARKER));
        assert!(!body.contains("herdr"));
        assert!(status("pi", home).expect("status").installed);

        install("pi", home).expect("second install refreshes the same file");
        assert!(sibling.exists(), "a neighbouring extension stays");

        let removed = uninstall("pi", home).expect("uninstall");
        assert!(!removed.installed);
        assert!(!path.exists());
        assert!(sibling.exists());
        assert!(!status("pi", home).expect("status").installed);
        uninstall("pi", home).expect("uninstall is fine when the file is already gone");
    }

    #[test]
    fn a_foreign_config_is_not_replaced_or_deleted() {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path();
        let path = home.join(".claude/settings.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, "{\n  \"permissions\": {}\n}\n").expect("user settings");

        let err = install("claude", home).expect_err("must refuse");
        assert!(
            err.to_string().contains("not a Viva integration file"),
            "{err}"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("unchanged"),
            "{\n  \"permissions\": {}\n}\n"
        );
        let err = uninstall("claude", home).expect_err("must refuse delete");
        assert!(
            err.to_string().contains("not a Viva integration file"),
            "{err}"
        );
        assert!(path.exists());
        assert!(!status("claude", home).expect("status").installed);
    }

    #[test]
    fn each_supported_name_writes_one_file_and_other_names_are_rejected() {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path();
        for name in SUPPORTED {
            let installed = install(name, home).expect(name);
            let path = PathBuf::from(&installed.path);
            assert!(path.is_file(), "{name}");
            let body = fs::read_to_string(&path).expect("body");
            assert!(body.contains(MARKER), "{name}");
            assert!(!body.to_ascii_lowercase().contains("herdr"), "{name}");
            uninstall(name, home).expect("uninstall");
            assert!(!path.exists(), "{name} file removed");
        }
        let err = install("amp", home).expect_err("unsupported");
        assert!(err.to_string().contains("not supported"), "{err}");
    }

    #[test]
    fn kimi_integration_uses_the_official_config_path_and_permission_request() {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path();
        let installed = install("kimi", home).expect("install");
        let path = PathBuf::from(&installed.path);
        assert!(path.ends_with(".kimi-code/config.toml"));
        let body = fs::read_to_string(&path).expect("read");
        assert!(body.contains("PermissionRequest"));
        assert!(!body.contains("permission_prompt"));
        uninstall("kimi", home).expect("uninstall");
    }

    #[test]
    fn hook_command_reports_through_viva_and_does_not_run_resume() {
        let command = report_command("done", "codex", "codex resume");
        assert!(command.contains("viva"));
        assert!(command.contains("--status"));
        assert!(command.contains("codex resume"));
        assert!(command.contains("subprocess.run"));
    }
}
