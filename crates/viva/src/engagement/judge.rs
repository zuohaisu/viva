//! Swappable judgment-model bindings for engagement state (issue #118).
//!
//! The resolver calls a binding only after explicit switch and entry rules
//! both fail. Bindings return a typed domain plus a confidence; they do not
//! decide whether that confidence is enough. Jev is one binding name. A
//! script file is another. Cloud HTTP is a normal transport: when it is not
//! configured or the call fails, the binding reports unavailable and the
//! resolver stores no guess.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::EngagementDomain;

/// What the resolver is allowed to send. Field names are recorded on the
/// experience row; values are not copied there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JudgmentRequest {
    pub execution_id: String,
    pub member_id: String,
    pub repository: Option<String>,
    pub task_id: Option<String>,
    pub entry: Option<String>,
    pub utterance: Option<String>,
}

impl JudgmentRequest {
    pub fn to_body(&self) -> Value {
        json!({
            "execution_id": self.execution_id,
            "member_id": self.member_id,
            "repository": self.repository,
            "task_id": self.task_id,
            "entry": self.entry,
            "utterance": self.utterance,
        })
    }

    pub fn fields_sent(&self) -> Vec<String> {
        self.to_body()
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JudgmentDecision {
    pub domain: EngagementDomain,
    pub project_id: Option<String>,
    pub interlocutor: Option<String>,
    pub activity: Option<String>,
    pub confidence: f64,
    pub fields_sent: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgmentFailure {
    pub binding: String,
    pub detail: String,
    pub fields_sent: Vec<String>,
}

pub trait JudgmentModel: Send + Sync {
    fn binding_name(&self) -> &str;
    fn judge(
        &self,
        request: &JudgmentRequest,
    ) -> std::result::Result<JudgmentDecision, JudgmentFailure>;
}

/// Typed decision inside a model response. Confidence is not compared here.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedJudgment {
    pub domain: EngagementDomain,
    pub project_id: Option<String>,
    pub interlocutor: Option<String>,
    pub activity: Option<String>,
    pub confidence: f64,
}

pub fn parse_judgment_body(body: &str) -> std::result::Result<ParsedJudgment, String> {
    let value: Value = serde_json::from_str(body).map_err(|err| format!("judgment JSON: {err}"))?;
    let domain = match value.get("domain").and_then(Value::as_str) {
        Some("work") => EngagementDomain::Work,
        Some("personal") => EngagementDomain::Personal,
        Some(other) => {
            return Err(format!("judgment domain `{other}` is not work or personal"));
        }
        None => return Err("judgment response has no domain".into()),
    };
    let confidence = value
        .get("confidence")
        .and_then(Value::as_f64)
        .ok_or_else(|| "judgment response has no numeric confidence".to_string())?;
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return Err(format!("judgment confidence {confidence} is outside 0..=1"));
    }
    Ok(ParsedJudgment {
        domain,
        project_id: optional_text(&value, "project"),
        interlocutor: optional_text(&value, "interlocutor"),
        activity: optional_text(&value, "activity"),
        confidence,
    })
}

fn optional_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// In-process binding tests use to prove the resolver, not the transport.
pub struct FixtureModel {
    name: String,
    calls: AtomicU64,
    reply: Mutex<FixtureReply>,
}

#[derive(Debug, Clone)]
pub enum FixtureReply {
    Decision {
        domain: EngagementDomain,
        confidence: f64,
    },
    Unavailable,
}

impl FixtureModel {
    pub fn new(name: impl Into<String>, reply: FixtureReply) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            calls: AtomicU64::new(0),
            reply: Mutex::new(reply),
        })
    }

    pub fn set_reply(&self, reply: FixtureReply) {
        *self.reply.lock().expect("fixture reply") = reply;
    }

    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl JudgmentModel for FixtureModel {
    fn binding_name(&self) -> &str {
        &self.name
    }

    fn judge(
        &self,
        request: &JudgmentRequest,
    ) -> std::result::Result<JudgmentDecision, JudgmentFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let fields_sent = request.fields_sent();
        match self.reply.lock().expect("fixture reply").clone() {
            FixtureReply::Unavailable => Err(JudgmentFailure {
                binding: self.name.clone(),
                detail: "fixture judgment model is unavailable".into(),
                fields_sent,
            }),
            FixtureReply::Decision { domain, confidence } => Ok(JudgmentDecision {
                domain,
                project_id: None,
                interlocutor: None,
                activity: None,
                confidence,
                fields_sent,
            }),
        }
    }
}

/// Second binding: a local JSON file in the same response shape as HTTP.
pub struct ScriptModel {
    path: PathBuf,
}

impl ScriptModel {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl JudgmentModel for ScriptModel {
    fn binding_name(&self) -> &str {
        "script"
    }

    fn judge(
        &self,
        request: &JudgmentRequest,
    ) -> std::result::Result<JudgmentDecision, JudgmentFailure> {
        let fields_sent = request.fields_sent();
        if self.path.as_os_str().is_empty() {
            return Err(script_failure(
                fields_sent,
                "judgment script is not configured",
            ));
        }
        let body = match std::fs::read_to_string(&self.path) {
            Ok(body) => body,
            Err(err) => {
                return Err(script_failure(
                    fields_sent,
                    &format!("judgment script {}: {err}", self.path.display()),
                ));
            }
        };
        let parsed = match parse_judgment_body(&body) {
            Ok(parsed) => parsed,
            Err(detail) => return Err(script_failure(fields_sent, &detail)),
        };
        Ok(JudgmentDecision {
            domain: parsed.domain,
            project_id: parsed.project_id,
            interlocutor: parsed.interlocutor,
            activity: parsed.activity,
            confidence: parsed.confidence,
            fields_sent,
        })
    }
}

/// Jev, or any other model that speaks the typed JSON contract over HTTP.
/// An empty endpoint or a failed call is unavailable — the resolver stores
/// no domain from it.
pub struct HttpModel {
    binding_name: String,
    endpoint: String,
}

impl HttpModel {
    pub fn new(binding_name: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            binding_name: binding_name.into(),
            endpoint: endpoint.into(),
        }
    }
}

impl JudgmentModel for HttpModel {
    fn binding_name(&self) -> &str {
        &self.binding_name
    }

    fn judge(
        &self,
        request: &JudgmentRequest,
    ) -> std::result::Result<JudgmentDecision, JudgmentFailure> {
        let fields_sent = request.fields_sent();
        let endpoint = self.endpoint.trim();
        if endpoint.is_empty() {
            return Err(http_failure(
                &self.binding_name,
                fields_sent,
                &format!("{} judgment endpoint is not configured", self.binding_name),
            ));
        }
        if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
            return Err(http_failure(
                &self.binding_name,
                fields_sent,
                "judgment endpoint must be an http or https URL",
            ));
        }
        let body = match serde_json::to_string(&request.to_body()) {
            Ok(body) => body,
            Err(err) => {
                return Err(http_failure(
                    &self.binding_name,
                    fields_sent,
                    &err.to_string(),
                ));
            }
        };
        let response = match post_json(endpoint, &body) {
            Ok(response) => response,
            Err(detail) => {
                return Err(http_failure(&self.binding_name, fields_sent, &detail));
            }
        };
        let parsed = match parse_judgment_body(&response) {
            Ok(parsed) => parsed,
            Err(detail) => return Err(http_failure(&self.binding_name, fields_sent, &detail)),
        };
        Ok(JudgmentDecision {
            domain: parsed.domain,
            project_id: parsed.project_id,
            interlocutor: parsed.interlocutor,
            activity: parsed.activity,
            confidence: parsed.confidence,
            fields_sent,
        })
    }
}

struct UnavailableModel {
    name: String,
}

impl JudgmentModel for UnavailableModel {
    fn binding_name(&self) -> &str {
        &self.name
    }

    fn judge(
        &self,
        request: &JudgmentRequest,
    ) -> std::result::Result<JudgmentDecision, JudgmentFailure> {
        Err(JudgmentFailure {
            binding: self.name.clone(),
            detail: format!("judgment binding `{}` is not available", self.name),
            fields_sent: request.fields_sent(),
        })
    }
}

pub fn model_from_settings(binding: &str, endpoint: &str, script: &str) -> Arc<dyn JudgmentModel> {
    match binding {
        "script" => Arc::new(ScriptModel::new(script)),
        "jev" | "http" => Arc::new(HttpModel::new(binding, endpoint)),
        other => Arc::new(UnavailableModel {
            name: other.to_string(),
        }),
    }
}

fn script_failure(fields_sent: Vec<String>, detail: &str) -> JudgmentFailure {
    JudgmentFailure {
        binding: "script".into(),
        detail: detail.to_string(),
        fields_sent,
    }
}

fn http_failure(binding: &str, fields_sent: Vec<String>, detail: &str) -> JudgmentFailure {
    JudgmentFailure {
        binding: binding.to_string(),
        detail: detail.to_string(),
        fields_sent,
    }
}

fn post_json(endpoint: &str, body: &str) -> std::result::Result<String, String> {
    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--max-time",
            "20",
            "-H",
            "Content-Type: application/json",
            "-H",
            "Accept: application/json",
            "--data-binary",
            "@-",
            endpoint,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("curl: {err}"))?;
    use std::io::Write as _;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|err| format!("curl stdin: {err}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("curl wait: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "judgment HTTP failed ({}); {}",
            output.status,
            stderr.trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| format!("judgment HTTP body: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_typed_domain_and_rejects_a_guess_shaped_body() {
        let parsed = parse_judgment_body(
            r#"{"domain":"work","confidence":0.5,"project":"","interlocutor":"haisu"}"#,
        )
        .expect("parse");
        assert_eq!(parsed.domain, EngagementDomain::Work);
        assert_eq!(parsed.confidence, 0.5);
        assert_eq!(parsed.interlocutor.as_deref(), Some("haisu"));
        assert!(parsed.project_id.is_none());
        assert!(parse_judgment_body(r#"{"domain":"unknown","confidence":1}"#).is_err());
        assert!(parse_judgment_body(r#"{"domain":"work","confidence":1.2}"#).is_err());
    }

    #[test]
    fn request_records_the_fields_that_would_be_sent() {
        let request = JudgmentRequest {
            execution_id: "exec-1".into(),
            member_id: "member-1".into(),
            repository: Some("https://github.com/zuohaisu/viva".into()),
            task_id: None,
            entry: None,
            utterance: Some("hello".into()),
        };
        let names = request.fields_sent();
        for key in [
            "execution_id",
            "member_id",
            "repository",
            "task_id",
            "entry",
            "utterance",
        ] {
            assert!(names.iter().any(|name| name == key), "{names:?}");
        }
    }
}
