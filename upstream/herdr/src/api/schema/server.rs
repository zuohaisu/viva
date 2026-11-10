use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct PingParams {}

// Viva modification: explicit host capability, never a caller-role assertion.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VivaClientAuthenticateParams {
    pub credential: String,
    /// Compared with the OS fact, never accepted as identity on its own.
    #[serde(default)]
    pub process_lifetime: Option<String>,
}
impl std::fmt::Debug for VivaClientAuthenticateParams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VivaClientAuthenticateParams { credential: [REDACTED] }")
    }
}

// Viva modification: one canonical execution start, admitted only for a
// host-issued owner frontend capability and checked against the canonical
// grant/task/member by the composition root before any process is spawned.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VivaExecutionStartParams {
    pub task_id: String,
    pub member_id: String,
    pub grant_id: String,
    pub request_key: String,
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
}
impl std::fmt::Debug for VivaExecutionStartParams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // argv can carry credentials or task data; request Debug is not evidence.
        formatter.write_str("VivaExecutionStartParams { task_id, member_id, grant_id, request_key, argv: [REDACTED], cwd, worktree_id }")
    }
}

// Viva modification: owner-gated native control. Stop names one canonical
// execution; pause/resume affect only NEW dispatches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VivaExecutionStopParams {
    pub execution_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VivaPauseParams {
    #[serde(default)]
    pub paused: bool,
}

// Viva modification: the first member-initiated ingress. The credential is a
// host-issued worker credential, never an identity claim; the composition root
// also binds the calling OS process to the confirmed execution.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VivaMemberReportParams {
    pub credential: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}
impl std::fmt::Debug for VivaMemberReportParams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential is a bearer secret; Debug is never evidence.
        formatter.write_str(
            "VivaMemberReportParams { credential: [REDACTED], status, detail: [REDACTED] }",
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerLiveHandoffParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerSshAgentRegisterParams {
    /// Absolute remote-host agent socket. Registration lasts until this API connection closes.
    pub socket_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerCapabilities {
    // Viva modification: discoverable JSON extension; absent for standalone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viva_domain_schema: Option<u32>,
    pub live_handoff: bool,
    #[serde(default)]
    pub detached_server_daemon: bool,
    /// Stable client-owned endpoint generation supported by this server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_protocol_generation: Option<u32>,
    /// Whether this server supports explicit client-shell surface interest.
    #[serde(default)]
    pub surface_interest: bool,
    /// Whether this server supports endpoint health probes.
    #[serde(default)]
    pub health_check: bool,
    /// Supports connection-scoped `server.ssh_agent.register` on the local JSON API.
    #[serde(default)]
    pub ssh_agent_registration: bool,
}
