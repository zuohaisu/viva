//! Optional per-agent integrations.
//!
//! `install` writes that agent's documented config location. A file the
//! agent already uses is merged: Viva adds its own hooks and leaves every
//! other key in place. `uninstall` removes only those hooks. The file is
//! deleted only when nothing else remains. A neighbouring file is never
//! touched, and a directory is never removed.
//!
//! The hook reports status, a native session reference, and a resume
//! command through `viva agent report`. It does not complete tasks, write
//! QA, or promote anything into memory.
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
const REPORT_PY: &str = r#"# viva-integration
import json,os,sys,subprocess
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
    ensure_parent(&path)?;
    let detail = match spec.layout {
        Layout::Exclusive => install_exclusive(&path, &spec.body)?,
        Layout::Json => install_json(&path, &spec.body)?,
        Layout::Toml => install_toml(&path, &spec.body)?,
        Layout::Yaml => install_yaml(&path)?,
    };
    Ok(IntegrationStatus {
        name: spec.name.to_string(),
        installed: true,
        path: path.display().to_string(),
        detail,
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
    let (removed_file, detail) = match spec.layout {
        Layout::Exclusive => uninstall_exclusive(&path)?,
        Layout::Json => uninstall_json(&path)?,
        Layout::Toml => uninstall_toml(&path)?,
        Layout::Yaml => uninstall_yaml(&path)?,
    };
    Ok(IntegrationStatus {
        name: spec.name.to_string(),
        installed: false,
        path: path.display().to_string(),
        detail: if removed_file {
            format!("{detail} The file was removed because nothing else was in it.")
        } else {
            detail
        },
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
        detail: "not installed".into(),
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

#[derive(Clone, Copy)]
enum Layout {
    /// A file named for Viva. The whole file is the integration.
    Exclusive,
    /// A JSON config the agent already loads. Hooks are merged in.
    Json,
    /// A TOML config the agent already loads. Hook tables are appended.
    Toml,
    /// A YAML config the agent already loads. Hook lines are inserted.
    Yaml,
}

struct Spec {
    name: &'static str,
    relative: &'static str,
    body: String,
    layout: Layout,
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
    let (relative, body, layout) = match name {
        "pi" => (
            ".pi/agent/extensions/viva-status.ts",
            pi_body(),
            Layout::Exclusive,
        ),
        "claude" => (
            ".claude/settings.json",
            hook_json("claude", "claude --resume", HookShape::Claude),
            Layout::Json,
        ),
        "codex" => (
            ".codex/hooks.json",
            hook_json("codex", "codex resume", HookShape::Codex),
            Layout::Json,
        ),
        "opencode" => (
            ".config/opencode/plugins/viva.js",
            opencode_body(),
            Layout::Exclusive,
        ),
        "kimi" => (".kimi-code/config.toml", kimi_toml(), Layout::Toml),
        "qodercli" => (
            ".qoder/settings.json",
            hook_json("qodercli", "qodercli --resume", HookShape::Claude),
            Layout::Json,
        ),
        "cursor" => (".cursor/hooks.json", cursor_json(), Layout::Json),
        "grok" => (
            ".grok/hooks/viva.json",
            hook_json("grok", "grok -r", HookShape::Grok),
            Layout::Json,
        ),
        "hermes" => (".hermes/config.yaml", hermes_yaml(), Layout::Yaml),
        _ => unreachable!("supported name"),
    };
    Ok(Spec {
        name,
        relative,
        body,
        layout,
    })
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn install_exclusive(path: &Path, body: &str) -> Result<String> {
    if path.exists() && !is_ours(path)? {
        return Err(Error::Validation(format!(
            "refusing to replace {} (not a Viva integration file)",
            path.display()
        )));
    }
    fs::write(path, body)?;
    Ok("wrote the Viva integration file".into())
}

fn uninstall_exclusive(path: &Path) -> Result<(bool, String)> {
    if !is_ours(path)? {
        return Err(Error::Validation(format!(
            "refusing to change {} (it has no Viva integration); the file was left as it is",
            path.display()
        )));
    }
    fs::remove_file(path)?;
    Ok((true, "removed the Viva integration file.".into()))
}

fn install_json(path: &Path, body: &str) -> Result<String> {
    let incoming = parse_json_object(body, path)?;
    let mut doc = read_json_object(path)?;
    if let Some(hooks) = doc.get("hooks") {
        if !hooks.is_object() && !hooks.is_array() {
            return Err(Error::Validation(format!(
                "refusing to edit {} (hooks is not an object or array); Viva will not replace it",
                path.display()
            )));
        }
    }
    strip_viva_json(&mut doc);
    merge_json(&mut doc, &incoming);
    if !serde_json::to_string(&doc)
        .map_err(|_| Error::Validation("internal JSON encode failed".into()))?
        .contains(MARKER)
    {
        return Err(Error::Validation(format!(
            "refusing to edit {} (Viva hooks could not be merged); the file was left unchanged",
            path.display()
        )));
    }
    write_json(path, &doc)?;
    Ok("merged the Viva hooks into the existing file".into())
}

fn uninstall_json(path: &Path) -> Result<(bool, String)> {
    let text = fs::read_to_string(path)?;
    if !text.contains(MARKER) {
        return Err(refuse_unmarked(path));
    }
    let mut doc = parse_json_object(&text, path)?;
    strip_viva_json(&mut doc);
    if json_is_disposable(&doc) {
        fs::remove_file(path)?;
        Ok((true, "removed the Viva hooks.".into()))
    } else {
        write_json(path, &doc)?;
        Ok((
            false,
            "removed the Viva hooks and left the rest of the file in place".into(),
        ))
    }
}

fn install_toml(path: &Path, block: &str) -> Result<String> {
    let existing = read_if_present(path)?;
    let mut kept = strip_toml(&existing);
    append_block(&mut kept, block);
    fs::write(path, kept)?;
    Ok("merged the Viva hooks into the existing file".into())
}

fn uninstall_toml(path: &Path) -> Result<(bool, String)> {
    let text = fs::read_to_string(path)?;
    if !text.contains(MARKER) {
        return Err(refuse_unmarked(path));
    }
    finish_text_removal(path, &strip_toml(&text))
}

fn install_yaml(path: &Path) -> Result<String> {
    let existing = read_if_present(path)?;
    let mut doc = parse_yaml_mapping(&existing, path)?;
    strip_viva_yaml(&mut doc, path)?;
    merge_hermes_viva_hooks(&mut doc, path)?;
    write_yaml(path, &doc)?;
    Ok("merged the Viva hooks into the existing file".into())
}

fn uninstall_yaml(path: &Path) -> Result<(bool, String)> {
    let text = fs::read_to_string(path)?;
    if !text.contains(MARKER) {
        return Err(refuse_unmarked(path));
    }
    let mut doc = parse_yaml_mapping(&text, path)?;
    strip_viva_yaml(&mut doc, path)?;
    if yaml_is_disposable(&doc) {
        fs::remove_file(path)?;
        Ok((true, "removed the Viva hooks.".into()))
    } else {
        write_yaml(path, &doc)?;
        Ok((
            false,
            "removed the Viva hooks and left the rest of the file in place".into(),
        ))
    }
}

fn refuse_unmarked(path: &Path) -> Error {
    Error::Validation(format!(
        "refusing to change {} (it has no Viva integration); the file was left as it is",
        path.display()
    ))
}

fn read_if_present(path: &Path) -> Result<String> {
    if path.exists() {
        Ok(fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

fn append_block(kept: &mut String, block: &str) {
    if !kept.trim().is_empty() {
        if !kept.ends_with('\n') {
            kept.push('\n');
        }
        if !kept.ends_with("\n\n") {
            kept.push('\n');
        }
    } else {
        kept.clear();
    }
    kept.push_str(block.trim_end());
    kept.push('\n');
}

fn finish_text_removal(path: &Path, kept: &str) -> Result<(bool, String)> {
    if kept.trim().is_empty() {
        fs::remove_file(path)?;
        Ok((true, "removed the Viva hooks.".into()))
    } else {
        let mut body = kept.trim_end().to_string();
        body.push('\n');
        fs::write(path, body)?;
        Ok((
            false,
            "removed the Viva hooks and left the rest of the file in place".into(),
        ))
    }
}

fn read_json_object(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let text = fs::read_to_string(path)?;
    parse_json_object(&text, path)
}

fn parse_json_object(text: &str, path: &Path) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| {
        Error::Validation(format!(
            "refusing to edit {} (not valid JSON); Viva will not replace it",
            path.display()
        ))
    })?;
    if !value.is_object() {
        return Err(Error::Validation(format!(
            "refusing to edit {} (not a JSON object); Viva will not replace it",
            path.display()
        )));
    }
    Ok(value)
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    fs::write(path, text)?;
    Ok(())
}

fn json_is_disposable(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|obj| obj.is_empty())
}

fn strip_viva_json(value: &mut serde_json::Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if obj
        .get("viva_integration")
        .and_then(|item| item.as_str())
        .is_some_and(|text| text.contains(MARKER))
    {
        obj.remove("viva_integration");
    }
    if let Some(hooks) = obj.get_mut("hooks") {
        strip_hooks_value(hooks);
        let empty = match hooks {
            serde_json::Value::Object(map) => map.is_empty(),
            serde_json::Value::Array(items) => items.is_empty(),
            _ => false,
        };
        if empty {
            obj.remove("hooks");
        }
    }
}

fn strip_hooks_value(hooks: &mut serde_json::Value) {
    match hooks {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(serde_json::Value::Array(items)) = map.get_mut(&key) {
                    filter_hook_list(items);
                }
                let empty = map
                    .get(&key)
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(Vec::is_empty);
                if empty {
                    map.remove(&key);
                }
            }
        }
        serde_json::Value::Array(items) => filter_hook_list(items),
        _ => {}
    }
}

fn filter_hook_list(items: &mut Vec<serde_json::Value>) {
    items.retain_mut(|item| !drop_hook_item(item));
}

fn drop_hook_item(item: &mut serde_json::Value) -> bool {
    if item
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| command.contains(MARKER))
    {
        return true;
    }
    let marked = item
        .get("hooks")
        .is_some_and(|nested| nested.to_string().contains(MARKER));
    let Some(nested) = item.get_mut("hooks") else {
        return false;
    };
    strip_hooks_value(nested);
    marked && hooks_empty(nested)
}

fn hooks_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(items) => items.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn merge_json(dst: &mut serde_json::Value, src: &serde_json::Value) {
    let (Some(dst_obj), Some(src_obj)) = (dst.as_object_mut(), src.as_object()) else {
        return;
    };
    if let Some(version) = src_obj.get("version") {
        dst_obj.entry("version").or_insert_with(|| version.clone());
    }
    if let Some(src_hooks) = src_obj.get("hooks") {
        let dst_hooks = dst_obj.entry("hooks").or_insert_with(|| match src_hooks {
            serde_json::Value::Object(_) => serde_json::json!({}),
            _ => serde_json::json!([]),
        });
        merge_hooks(dst_hooks, src_hooks);
    }
}

fn merge_hooks(dst: &mut serde_json::Value, src: &serde_json::Value) {
    match (dst, src) {
        (serde_json::Value::Object(dst_map), serde_json::Value::Object(src_map)) => {
            for (event, src_items) in src_map {
                let dst_items = dst_map
                    .entry(event.clone())
                    .or_insert_with(|| serde_json::json!([]));
                let Some(dst_arr) = dst_items.as_array_mut() else {
                    continue;
                };
                let Some(src_arr) = src_items.as_array() else {
                    continue;
                };
                dst_arr.extend(src_arr.iter().cloned());
            }
        }
        (serde_json::Value::Array(dst_arr), serde_json::Value::Array(src_arr)) => {
            dst_arr.extend(src_arr.iter().cloned());
        }
        _ => {}
    }
}

fn strip_toml(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut drop = vec![false; lines.len()];
    let mut index = 0;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed == format!("# {MARKER}") || trimmed == MARKER {
            drop[index] = true;
            index += 1;
            continue;
        }
        if toml_header(lines[index]) {
            let mut end = index + 1;
            while end < lines.len() && !toml_header(lines[end]) {
                end += 1;
            }
            if lines[index..end].iter().any(|line| line.contains(MARKER)) {
                for slot in drop.iter_mut().take(end).skip(index) {
                    *slot = true;
                }
            }
            index = end;
            continue;
        }
        index += 1;
    }
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        if drop[index] {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.trim().is_empty() {
        String::new()
    } else {
        out
    }
}

fn toml_header(line: &str) -> bool {
    let trimmed = line.trim_start();
    !line.starts_with([' ', '\t']) && trimmed.starts_with('[')
}

fn parse_yaml_mapping(text: &str, path: &Path) -> Result<serde_yaml::Value> {
    if text.trim().is_empty() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }
    let value: serde_yaml::Value = serde_yaml::from_str(text).map_err(|_| {
        Error::Validation(format!(
            "refusing to edit {} (not valid YAML); Viva will not replace it",
            path.display()
        ))
    })?;
    if !value.is_mapping() {
        return Err(Error::Validation(format!(
            "refusing to edit {} (not a YAML mapping); Viva will not replace it",
            path.display()
        )));
    }
    Ok(value)
}

fn write_yaml(path: &Path, value: &serde_yaml::Value) -> Result<()> {
    let text = serde_yaml::to_string(value).map_err(|_| {
        Error::Validation(format!(
            "refusing to write {} (could not encode YAML); the file was left unchanged",
            path.display()
        ))
    })?;
    serde_yaml::from_str::<serde_yaml::Value>(&text).map_err(|_| {
        Error::Validation(format!(
            "refusing to write {} (produced invalid YAML); the file was left unchanged",
            path.display()
        ))
    })?;
    fs::write(path, format!("{text}\n"))?;
    Ok(())
}

fn yaml_is_disposable(value: &serde_yaml::Value) -> bool {
    value.as_mapping().is_some_and(|map| map.is_empty())
}

fn hooks_mapping_mut<'a>(
    root: &'a mut serde_yaml::Mapping,
    path: &Path,
) -> Result<&'a mut serde_yaml::Mapping> {
    match root.get_mut("hooks") {
        None => {
            root.insert(
                serde_yaml::Value::String("hooks".into()),
                serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
            );
        }
        Some(value) if value.is_mapping() => {}
        Some(_) => {
            return Err(Error::Validation(format!(
                "refusing to edit {} (hooks is not a YAML mapping); Viva will not replace it",
                path.display()
            )));
        }
    }
    Ok(root
        .get_mut("hooks")
        .and_then(serde_yaml::Value::as_mapping_mut)
        .expect("hooks mapping was just inserted"))
}

fn strip_viva_yaml(value: &mut serde_yaml::Value, path: &Path) -> Result<()> {
    let Some(root) = value.as_mapping_mut() else {
        return Ok(());
    };
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(());
    };
    if !hooks.is_mapping() {
        return Err(Error::Validation(format!(
            "refusing to edit {} (hooks is not a YAML mapping); Viva will not replace it",
            path.display()
        )));
    }
    let hooks = hooks.as_mapping_mut().expect("mapping");
    for key in ["pre_llm_call", "post_llm_call"] {
        let Some(list) = hooks.get_mut(key) else {
            continue;
        };
        filter_yaml_hook_list(list);
        let empty = list.as_sequence().is_some_and(|seq| seq.is_empty());
        if empty {
            hooks.remove(serde_yaml::Value::String(key.into()));
        }
    }
    if hooks.is_empty() {
        root.remove(serde_yaml::Value::String("hooks".into()));
    }
    Ok(())
}

fn filter_yaml_hook_list(list: &mut serde_yaml::Value) {
    let Some(seq) = list.as_sequence_mut() else {
        return;
    };
    seq.retain(|item| {
        !item
            .get("command")
            .and_then(serde_yaml::Value::as_str)
            .is_some_and(|command| command.contains(MARKER))
    });
}

fn merge_hermes_viva_hooks(value: &mut serde_yaml::Value, path: &Path) -> Result<()> {
    let Some(root) = value.as_mapping_mut() else {
        return Err(Error::Validation(format!(
            "refusing to edit {} (not a YAML mapping); Viva will not replace it",
            path.display()
        )));
    };
    let hooks = hooks_mapping_mut(root, path)?;
    let working = report_command("working", "hermes", "hermes --resume");
    let done = report_command("done", "hermes", "hermes --resume");
    append_yaml_hook(hooks, "pre_llm_call", &working);
    append_yaml_hook(hooks, "post_llm_call", &done);
    let encoded = serde_yaml::to_string(value).map_err(|_| {
        Error::Validation(format!(
            "refusing to edit {} (Viva hooks could not be merged); the file was left unchanged",
            path.display()
        ))
    })?;
    if !encoded.contains(MARKER) {
        return Err(Error::Validation(format!(
            "refusing to edit {} (Viva hooks could not be merged); the file was left unchanged",
            path.display()
        )));
    }
    Ok(())
}

fn append_yaml_hook(hooks: &mut serde_yaml::Mapping, key: &str, command: &str) {
    let entry = hooks
        .entry(serde_yaml::Value::String(key.into()))
        .or_insert_with(|| serde_yaml::Value::Sequence(vec![]));
    let Some(seq) = entry.as_sequence_mut() else {
        return;
    };
    let mut item = serde_yaml::Mapping::new();
    item.insert(
        serde_yaml::Value::String("command".into()),
        serde_yaml::Value::String(command.into()),
    );
    seq.push(serde_yaml::Value::Mapping(item));
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
    fn install_merges_claude_settings_and_uninstall_keeps_user_keys() {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path();
        let path = home.join(".claude/settings.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, "{\n  \"permissions\": {}\n}\n").expect("user settings");

        install("claude", home).expect("merge");
        let merged: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
        assert!(merged.get("permissions").is_some());
        assert!(merged.to_string().contains(MARKER));
        let first_hooks = merged["hooks"].to_string();

        install("claude", home).expect("second install refreshes hooks");
        let again: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(again["permissions"], merged["permissions"]);
        assert_eq!(again["hooks"].to_string(), first_hooks);

        let mut edited = again;
        edited["custom"] = serde_json::json!("keep-me");
        fs::write(
            &path,
            serde_json::to_string_pretty(&edited).expect("encode"),
        )
        .expect("user edit");

        uninstall("claude", home).expect("uninstall");
        assert!(path.exists(), "settings.json stays when the user has keys");
        assert!(path.parent().expect("parent").exists());
        let kept: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(kept["permissions"], serde_json::json!({}));
        assert_eq!(kept["custom"], "keep-me");
        assert!(kept.get("hooks").is_none());
        assert!(!kept.to_string().contains(MARKER));
        assert!(!status("claude", home).expect("status").installed);
    }

    #[test]
    fn uninstall_leaves_a_user_hook_and_refuses_an_unmarked_or_invalid_file() {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path();
        let path = home.join(".qoder/settings.json");
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(
            &path,
            r#"{"permissions":{"allow":["Read"]},"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo stay"}]}]}}"#,
        )
        .expect("user settings");

        install("qodercli", home).expect("merge");
        uninstall("qodercli", home).expect("uninstall");
        let kept: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(kept["permissions"]["allow"][0], "Read");
        assert!(kept["hooks"]["Stop"].to_string().contains("echo stay"));
        assert!(!kept.to_string().contains(MARKER));

        let unmarked = home.join(".claude/settings.json");
        fs::create_dir_all(unmarked.parent().expect("parent")).expect("dirs");
        fs::write(&unmarked, "{\"permissions\":{\"keep\":true}}\n").expect("plain");
        let err = uninstall("claude", home).expect_err("no marker");
        assert!(err.to_string().contains("left as it is"), "{err}");
        assert_eq!(
            fs::read_to_string(&unmarked).expect("unchanged"),
            "{\"permissions\":{\"keep\":true}}\n"
        );

        fs::write(&unmarked, "{not json\n").expect("broken");
        let err = install("claude", home).expect_err("invalid json");
        assert!(err.to_string().contains("not valid JSON"), "{err}");
        assert_eq!(
            fs::read_to_string(&unmarked).expect("unchanged"),
            "{not json\n"
        );

        fs::write(&unmarked, "{\"hooks\":\"oops\"}\n").expect("bad hooks type");
        let err = install("claude", home).expect_err("hooks not object");
        assert!(
            err.to_string().contains("hooks is not an object or array"),
            "{err}"
        );
        assert_eq!(
            fs::read_to_string(&unmarked).expect("unchanged"),
            "{\"hooks\":\"oops\"}\n"
        );

        let cursor = home.join(".cursor/hooks.json");
        fs::create_dir_all(cursor.parent().expect("parent")).expect("dirs");
        fs::write(
            &cursor,
            r#"{"version":1,"hooks":{"stop":[{"command":"echo stay"}]}}"#,
        )
        .expect("user hooks");
        install("cursor", home).expect("merge");
        uninstall("cursor", home).expect("uninstall");
        let kept: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&cursor).expect("read")).expect("json");
        assert_eq!(kept["version"], 1);
        assert!(kept["hooks"]["stop"].to_string().contains("echo stay"));
        assert!(!kept.to_string().contains(MARKER));

        fs::write(&cursor, "{\"version\":1}\n").expect("version only");
        install("cursor", home).expect("merge");
        uninstall("cursor", home).expect("uninstall");
        assert!(cursor.exists(), "version-only cursor file stays");
        let kept: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&cursor).expect("read")).expect("json");
        assert_eq!(kept["version"], 1);
        assert!(kept.get("hooks").is_none());
    }

    #[test]
    fn shared_toml_and_yaml_keep_user_lines() {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path();
        let kimi = home.join(".kimi-code/config.toml");
        fs::create_dir_all(kimi.parent().expect("parent")).expect("dirs");
        fs::write(&kimi, "model = \"kimi-k2\"\n").expect("user toml");
        install("kimi", home).expect("merge");
        assert!(
            fs::read_to_string(&kimi)
                .expect("read")
                .contains("PermissionRequest")
        );
        uninstall("kimi", home).expect("uninstall");
        let kept = fs::read_to_string(&kimi).expect("kept");
        assert!(kept.contains("model = \"kimi-k2\""));
        assert!(!kept.contains("PermissionRequest"));
        assert!(!kept.contains(MARKER));

        let hermes = home.join(".hermes/config.yaml");
        fs::create_dir_all(hermes.parent().expect("parent")).expect("dirs");
        fs::write(
            &hermes,
            "model: hermes\nhooks:\n  pre_tool_call:\n    - command: \"echo stay\"\n",
        )
        .expect("user yaml");
        install("hermes", home).expect("merge");
        let merged = fs::read_to_string(&hermes).expect("read");
        assert!(merged.contains("pre_llm_call"));
        assert!(merged.contains("echo stay"));
        uninstall("hermes", home).expect("uninstall");
        let kept = fs::read_to_string(&hermes).expect("kept");
        assert!(kept.contains("model: hermes"));
        assert!(kept.contains("echo stay"));
        assert!(!kept.contains(MARKER));
        assert!(!kept.contains("pre_llm_call"));
    }

    #[test]
    fn hermes_yaml_merge_handles_nonstandard_user_layouts() {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path();
        let hermes = home.join(".hermes/config.yaml");
        fs::create_dir_all(hermes.parent().expect("parent")).expect("dirs");

        fs::write(
            &hermes,
            "model: hermes\nhooks:\n    pre_tool_call:\n        - command: \"echo stay\"\n",
        )
        .expect("four-space hooks");
        install("hermes", home).expect("merge");
        let merged: serde_yaml::Value =
            serde_yaml::from_str(&fs::read_to_string(&hermes).expect("read")).expect("parse");
        let hooks = merged["hooks"].as_mapping().expect("hooks map");
        assert!(hooks.contains_key(serde_yaml::Value::String("pre_tool_call".into())));
        assert!(hooks.contains_key(serde_yaml::Value::String("pre_llm_call".into())));
        uninstall("hermes", home).expect("uninstall");
        let kept: serde_yaml::Value =
            serde_yaml::from_str(&fs::read_to_string(&hermes).expect("read")).expect("parse");
        assert_eq!(kept["model"], "hermes");
        assert!(
            kept["hooks"]["pre_tool_call"].as_sequence().expect("seq")[0]["command"]
                .as_str()
                .is_some_and(|c| c.contains("echo stay"))
        );

        fs::write(&hermes, "model: hermes\nhooks: {}\n").expect("flow hooks");
        install("hermes", home).expect("merge empty hooks");
        serde_yaml::from_str::<serde_yaml::Value>(&fs::read_to_string(&hermes).expect("read"))
            .expect("valid yaml after install");
        uninstall("hermes", home).expect("uninstall");
        let kept: serde_yaml::Value =
            serde_yaml::from_str(&fs::read_to_string(&hermes).expect("read")).expect("parse");
        assert_eq!(kept["model"], "hermes");
        assert!(!kept.to_string().contains(MARKER));
        assert!(kept.get("hooks").is_none_or(|hooks| {
            hooks.as_mapping().is_some_and(|map| {
                !map.contains_key(serde_yaml::Value::String("pre_llm_call".into()))
            })
        }));
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
