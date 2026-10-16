//! Harness transcript adapters.
//!
//! Paths and record shapes were checked on this machine on 2026-10-16, not
//! copied from a write-up:
//!
//! - Claude Code: `~/.claude/projects/<encoded-cwd>/*.jsonl`. The session id
//!   is `sessionId`. Curatable turns are `type: user|assistant` records whose
//!   `message.content` is either a string or a list of `{type: "text", text}`.
//!   `parentUuid` points at the previous entry in the same file. Main files
//!   have no session-level parent field. Sidechain transcripts live under
//!   `subagents/` (`isSidechain: true`): they repeat the parent `sessionId`
//!   and carry their own `agentId`. Those use `agentId` as the session id
//!   and the parent `sessionId` as the parent, so a subagent is not merged
//!   into the parent session.
//! - Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. The header is
//!   `type: session_meta`; the session id is `payload.id` (it is the id in
//!   the filename). `payload.parent_thread_id`, when present, is another
//!   file's `payload.id`. Turns are `type: response_item` with
//!   `payload.type == "message"` and role `user` or `assistant`. Text lives
//!   in `content[].text` where `type` is `input_text` or `output_text`.
//! - Pi: the `pi` binary is not on PATH here. The installed
//!   `@earendil-works/pi-coding-agent` writes
//!   `~/.pi/agent/sessions/--<cwd-with-slashes-as-dashes>--/<timestamp>_<id>.jsonl`
//!   (or `$PI_CODING_AGENT_DIR/sessions/...`). The header is
//!   `{type: "session", id, parentSession?}`. Turns are
//!   `{type: "message", message: {role, content}}`. `parentSession` is the
//!   parent file path.
//!
//! Execution records store attribution, not the transcript, so Pi's body
//! comes from this file. An execution link only answers who the session
//! belongs to.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::foundation::error::{Error, Result};

pub const HARNESS_CLAUDE: &str = "claude-code";
pub const HARNESS_CODEX: &str = "codex";
pub const HARNESS_PI: &str = "pi";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSession {
    pub harness: &'static str,
    pub session_id: String,
    pub parent_session: Option<String>,
    pub turns: Vec<ParsedTurn>,
}

pub fn canonical_harness(name: &str) -> Option<&'static str> {
    match name.trim() {
        "claude" | "claude-code" => Some(HARNESS_CLAUDE),
        "codex" => Some(HARNESS_CODEX),
        "pi" => Some(HARNESS_PI),
        _ => None,
    }
}

pub fn valid_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
}

pub fn parse_transcript(harness: &str, path: &Path) -> Result<Option<ParsedSession>> {
    let harness = canonical_harness(harness).ok_or_else(|| {
        Error::Validation(format!(
            "harness `{harness}` is not collected; expected claude-code, codex, or pi"
        ))
    })?;
    let values = read_jsonl(path)?;
    let parsed = match harness {
        HARNESS_CLAUDE => parse_claude(&values, path),
        HARNESS_CODEX => parse_codex(&values),
        HARNESS_PI => parse_pi(&values, path),
        _ => None,
    };
    Ok(parsed.filter(|session| valid_session_id(&session.session_id) && !session.turns.is_empty()))
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    let mut values = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            values.push(value);
        }
    }
    Ok(values)
}

fn parse_claude(values: &[Value], path: &Path) -> Option<ParsedSession> {
    let parent_id = values.iter().find_map(|value| {
        string_field(value, "sessionId").or_else(|| string_field(value, "session_id"))
    })?;
    let sidechain = values
        .iter()
        .any(|value| value.get("isSidechain").and_then(Value::as_bool) == Some(true));
    let (session_id, parent_session) = if sidechain {
        let own = values
            .iter()
            .find_map(|value| string_field(value, "agentId"))
            .filter(|agent| valid_session_id(agent) && agent != &parent_id)
            .or_else(|| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .filter(|stem| valid_session_id(stem) && *stem != parent_id)
                    .map(str::to_string)
            })?;
        (own, Some(parent_id))
    } else {
        (parent_id, None)
    };
    let mut turns = Vec::new();
    for value in values {
        let Some(kind) = string_field(value, "type") else {
            continue;
        };
        if kind != "user" && kind != "assistant" {
            continue;
        }
        if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let role = string_field(message, "role").unwrap_or(kind);
        if role != "user" && role != "assistant" {
            continue;
        }
        push_turn(&mut turns, &role, message.get("content"));
    }
    Some(ParsedSession {
        harness: HARNESS_CLAUDE,
        session_id,
        parent_session,
        turns,
    })
}

fn parse_codex(values: &[Value]) -> Option<ParsedSession> {
    let header = values
        .iter()
        .find(|value| string_field(value, "type").as_deref() == Some("session_meta"))?;
    let payload = header.get("payload")?;
    let session_id = string_field(payload, "id").or_else(|| string_field(payload, "session_id"))?;
    let parent_session =
        string_field(payload, "parent_thread_id").filter(|id| valid_session_id(id));
    let mut turns = Vec::new();
    for value in values {
        if string_field(value, "type").as_deref() != Some("response_item") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if string_field(payload, "type").as_deref() != Some("message") {
            continue;
        }
        let Some(role) = string_field(payload, "role") else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        push_turn(&mut turns, &role, payload.get("content"));
    }
    Some(ParsedSession {
        harness: HARNESS_CODEX,
        session_id,
        parent_session,
        turns,
    })
}

fn parse_pi(values: &[Value], path: &Path) -> Option<ParsedSession> {
    let header = values
        .iter()
        .find(|value| string_field(value, "type").as_deref() == Some("session"))?;
    let session_id = string_field(header, "id")?;
    let parent_session = string_field(header, "parentSession")
        .and_then(|parent| resolve_pi_parent(&parent, path.parent().unwrap_or(Path::new(""))));
    let mut turns = Vec::new();
    for value in values {
        if string_field(value, "type").as_deref() != Some("message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        let Some(role) = string_field(message, "role") else {
            continue;
        };
        if role != "user" && role != "assistant" {
            continue;
        }
        push_turn(&mut turns, &role, message.get("content"));
    }
    Some(ParsedSession {
        harness: HARNESS_PI,
        session_id,
        parent_session,
        turns,
    })
}

fn resolve_pi_parent(parent: &str, sibling_dir: &Path) -> Option<String> {
    if valid_session_id(parent) && !parent.contains('/') && !parent.contains('\\') {
        return Some(parent.to_string());
    }
    let raw = PathBuf::from(parent);
    let candidate = if raw.is_absolute() {
        raw.clone()
    } else {
        sibling_dir.join(&raw)
    };
    if candidate.is_file()
        && let Ok(values) = read_jsonl(&candidate)
        && let Some(id) = values.iter().find_map(|value| {
            if string_field(value, "type").as_deref() == Some("session") {
                string_field(value, "id")
            } else {
                None
            }
        })
        && valid_session_id(&id)
    {
        return Some(id);
    }
    raw.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('_'))
        .map(|(_, id)| id.to_string())
        .filter(|id| valid_session_id(id))
}

fn push_turn(turns: &mut Vec<ParsedTurn>, role: &str, content: Option<&Value>) {
    let Some(content) = content else {
        return;
    };
    let text = text_from_content(content);
    if text.is_empty() {
        return;
    }
    turns.push(ParsedTurn {
        role: role.to_string(),
        content: text,
    });
}

fn text_from_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.trim().to_string(),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                let Some(object) = item.as_object() else {
                    continue;
                };
                let kind = object.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(kind, "text" | "input_text" | "output_text") {
                    continue;
                }
                let Some(text) = object.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let text = text.trim();
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_keeps_user_and_assistant_text_and_skips_meta() {
        let raw = r#"{"type":"user","isMeta":true,"sessionId":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","message":{"role":"user","content":"hook noise"}}
{"type":"user","sessionId":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","message":{"role":"user","content":[{"type":"text","text":"Review the archive plan"}]}}
{"type":"assistant","sessionId":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","message":{"role":"assistant","content":[{"type":"text","text":"Keep it per member."}]}}
"#;
        let dir = std::env::temp_dir().join(format!("viva-parse-claude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(&path, raw).unwrap();
        let parsed = parse_transcript(HARNESS_CLAUDE, &path).unwrap().unwrap();
        assert_eq!(parsed.session_id, "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].content, "Review the archive plan");
        assert!(parsed.parent_session.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_sidechain_keeps_its_agent_id_and_points_at_the_parent_session() {
        let raw = r#"{"type":"user","isSidechain":true,"sessionId":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","agentId":"agent-side-1","message":{"role":"user","content":"Look only at this subagent."}}
{"type":"assistant","isSidechain":true,"sessionId":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","agentId":"agent-side-1","message":{"role":"assistant","content":"Subagent answer."}}
"#;
        let dir =
            std::env::temp_dir().join(format!("viva-parse-claude-side-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("subagents")).unwrap();
        let path = dir.join("subagents").join("not-the-agent-id.jsonl");
        std::fs::write(&path, raw).unwrap();
        let parsed = parse_transcript(HARNESS_CLAUDE, &path).unwrap().unwrap();
        assert_eq!(parsed.session_id, "agent-side-1");
        assert_eq!(
            parsed.parent_session.as_deref(),
            Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
        );
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].content, "Look only at this subagent.");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
