//! Member records, roles and model/tool bindings (V02, issue #11).
//!
//! Domain semantics (rollout §2 + ADR 0006/0009):
//! - Members are configuration, never hard-coded identity (`Viva ≠ Samuel`).
//!   Changing a role, model or tool binding updates configuration rows; the
//!   `member_id` and every record that references it stay stable across
//!   restarts.
//! - A configured binding is a *wish*, not a verified capability: tool
//!   probing runs an explicit argv with a timeout and bounded output, and the
//!   probe result — not the config string — is what the office treats as a
//!   capability. Missing tools / unsupported models fail with actionable
//!   errors; nothing silently substitutes a fallback.

use std::str::FromStr as _;
use std::time::{Duration, Instant};

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{MemberId, utc_now};
use crate::foundation::store::{DOMAIN_MEMBERS, MigrationRegistry, Store};

/// Register the `members` domain migrations. Domains own their own version
/// sequences (G0 contract §5); this domain currently has one version.
pub fn register_migrations(registry: MigrationRegistry) -> MigrationRegistry {
    registry.register(DOMAIN_MEMBERS, 1, "members v1", MEMBERS_V1_SQL)
}

pub const MEMBERS_V1_SQL: &str = r#"
CREATE TABLE members (
    member_id    TEXT PRIMARY KEY,
    display_name TEXT NOT NULL CHECK (length(trim(display_name)) > 0),
    created_at   TEXT NOT NULL
);

-- The current configuration of a member. Exactly one binding row per member;
-- updates rewrite this row but never touch the member identity.
CREATE TABLE member_bindings (
    member_id     TEXT PRIMARY KEY REFERENCES members(member_id),
    role          TEXT NOT NULL,
    model_binding TEXT NOT NULL,
    tools_json    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

-- Verified tool capability cache. Rows are probe *results* keyed by the exact
-- argv that produced them; a config string is never treated as a capability.
CREATE TABLE tool_probe_cache (
    argv_json       TEXT PRIMARY KEY,
    available       INTEGER NOT NULL CHECK (available IN (0, 1)),
    detail          TEXT NOT NULL,
    probed_at       TEXT NOT NULL
);
"#;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// A member of the office: an identity reference plus display configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRecord {
    pub member_id: MemberId,
    pub display_name: String,
    pub created_at: String,
}

/// The current role/model/tool configuration of a member. Names are
/// configuration data; nothing here claims a capability has been verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberBinding {
    pub member_id: MemberId,
    pub role: String,
    pub model_binding: String,
    pub tools: Vec<String>,
    pub updated_at: String,
}

/// The outcome of a real tool probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProbeResult {
    pub argv: Vec<String>,
    pub available: bool,
    pub detail: String,
    pub probed_at: String,
}

/// Which models this office accepts. A binding outside the list is an
/// actionable configuration error, never a silent substitution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPolicy {
    pub supported_models: Vec<String>,
}

impl ModelPolicy {
    pub fn new(supported_models: Vec<String>) -> Self {
        Self { supported_models }
    }

    pub fn ensure_supported(&self, model_binding: &str) -> OfficeResult<()> {
        if self.supported_models.iter().any(|m| m == model_binding) {
            return Ok(());
        }
        Err(OfficeError::Validation(format!(
            "model `{model_binding}` is not supported by this office; supported models: {}. \
             Update the member binding to one of these.",
            self.supported_models.join(", ")
        )))
    }
}

// ---------------------------------------------------------------------------
// Member registry
// ---------------------------------------------------------------------------

/// Member-domain registry operations over an open [`Store`].
pub struct MemberRegistry<'a> {
    store: &'a Store,
}

impl<'a> MemberRegistry<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    /// Register a member. Display names are configuration and may repeat;
    /// identity is the generated id.
    pub fn register(&self, display_name: impl Into<String>) -> OfficeResult<MemberRecord> {
        let display_name = display_name.into();
        if display_name.trim().is_empty() {
            return Err(OfficeError::Validation(
                "member display_name must not be empty".into(),
            ));
        }
        let record = MemberRecord {
            member_id: MemberId::new(),
            display_name,
            created_at: utc_now(),
        };
        self.store.connection().execute(
            "INSERT INTO members(member_id, display_name, created_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                record.member_id.as_str(),
                record.display_name,
                record.created_at
            ],
        )?;
        Ok(record)
    }

    pub fn get(&self, member_id: &MemberId) -> OfficeResult<Option<MemberRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT member_id, display_name, created_at FROM members WHERE member_id = ?1",
        )?;
        let row = stmt
            .query_row([member_id.as_str()], |row| {
                Ok(MemberRecord {
                    member_id: MemberId::from_str(&row.get::<_, String>(0)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    display_name: row.get(1)?,
                    created_at: row.get(2)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    pub fn require(&self, member_id: &MemberId) -> OfficeResult<MemberRecord> {
        self.get(member_id)?.ok_or_else(|| OfficeError::NotFound {
            entity: "member",
            id: member_id.to_string(),
        })
    }

    pub fn list(&self) -> OfficeResult<Vec<MemberRecord>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT member_id, display_name, created_at FROM members ORDER BY created_at, member_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut members = Vec::new();
        for row in rows {
            let (id, display_name, created_at) = row?;
            members.push(MemberRecord {
                member_id: MemberId::from_str(&id).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                display_name,
                created_at,
            });
        }
        Ok(members)
    }

    /// Set (or replace) the role/model/tool configuration. The member id and
    /// every record referencing it stay untouched — bindings are
    /// configuration, identity is not.
    pub fn set_binding(&self, binding: &MemberBinding) -> OfficeResult<()> {
        if binding.role.trim().is_empty() {
            return Err(OfficeError::Validation(
                "member role must not be empty".into(),
            ));
        }
        self.require(&binding.member_id)?;
        let tools = serde_json::to_string(&binding.tools)?;
        let updated = utc_now();
        self.store.connection().execute(
            "INSERT INTO member_bindings(member_id, role, model_binding, tools_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(member_id) DO UPDATE SET
                role = excluded.role,
                model_binding = excluded.model_binding,
                tools_json = excluded.tools_json,
                updated_at = excluded.updated_at",
            rusqlite::params![
                binding.member_id.as_str(),
                binding.role,
                binding.model_binding,
                tools,
                updated,
            ],
        )?;
        Ok(())
    }

    pub fn binding(&self, member_id: &MemberId) -> OfficeResult<Option<MemberBinding>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT role, model_binding, tools_json, updated_at
             FROM member_bindings WHERE member_id = ?1",
        )?;
        let row = stmt
            .query_row([member_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .optional()?;
        let Some((role, model_binding, tools_json, updated_at)) = row else {
            return Ok(None);
        };
        let tools: Vec<String> = serde_json::from_str(&tools_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(Some(MemberBinding {
            member_id: member_id.clone(),
            role,
            model_binding,
            tools,
            updated_at,
        }))
    }

    /// Validate a binding against the office model policy before use.
    pub fn ensure_model_supported(
        &self,
        member_id: &MemberId,
        policy: &ModelPolicy,
    ) -> OfficeResult<()> {
        let binding = self
            .binding(member_id)?
            .ok_or_else(|| OfficeError::NotFound {
                entity: "member binding",
                id: member_id.to_string(),
            })?;
        policy.ensure_supported(&binding.model_binding)
    }

    /// Probe one tool by explicit argv with a timeout and bounded captured
    /// output. The result is cached under the exact argv; `refresh` re-runs
    /// the probe instead of trusting the cache. Missing tools produce an
    /// actionable `detail`, and the config string alone is never evidence.
    pub fn probe_tool(
        &self,
        argv: Vec<String>,
        timeout: Duration,
        max_output_bytes: usize,
        refresh: bool,
    ) -> OfficeResult<ToolProbeResult> {
        if argv.is_empty() || argv[0].trim().is_empty() {
            return Err(OfficeError::Validation(
                "tool probe argv must name a program (explicit argv, no shell string)".into(),
            ));
        }
        let argv_json = serde_json::to_string(&argv)?;
        if !refresh {
            let cached = self.cached_probe(&argv_json)?;
            if let Some(result) = cached {
                return Ok(result);
            }
        }
        let result = run_probe(&argv, timeout, max_output_bytes)?;
        self.store.connection().execute(
            "INSERT INTO tool_probe_cache(argv_json, available, detail, probed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(argv_json) DO UPDATE SET
                available = excluded.available,
                detail = excluded.detail,
                probed_at = excluded.probed_at",
            rusqlite::params![
                argv_json,
                if result.available { 1 } else { 0 },
                result.detail,
                result.probed_at,
            ],
        )?;
        Ok(result)
    }

    fn cached_probe(&self, argv_json: &str) -> OfficeResult<Option<ToolProbeResult>> {
        let mut stmt = self.store.connection().prepare(
            "SELECT argv_json, available, detail, probed_at
             FROM tool_probe_cache WHERE argv_json = ?1",
        )?;
        let row = stmt
            .query_row([argv_json], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .optional()?;
        let Some((argv_json, available, detail, probed_at)) = row else {
            return Ok(None);
        };
        Ok(Some(ToolProbeResult {
            argv: serde_json::from_str(&argv_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
            available: available != 0,
            detail,
            probed_at,
        }))
    }
}

/// Run one bounded probe: explicit argv, hard timeout, output capped. The
/// probe reports what actually happened; it never guesses availability from
/// the config string. A tool that cannot even launch is an unavailable
/// *result* (so the negative fact is cached), not an internal error.
fn run_probe(
    argv: &[String],
    timeout: Duration,
    max_output_bytes: usize,
) -> OfficeResult<ToolProbeResult> {
    let mut child = match std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return Ok(ToolProbeResult {
                argv: argv.to_vec(),
                available: false,
                detail: format!(
                    "tool `{}` cannot be launched ({}); install it or fix the binding argv",
                    argv[0], err
                ),
                probed_at: utc_now(),
            });
        }
    };
    let started = Instant::now();
    loop {
        match child.try_wait()? {
            Some(_status) => break,
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(ToolProbeResult {
                    argv: argv.to_vec(),
                    available: false,
                    detail: format!(
                        "tool `{}` did not answer within {}ms; treat as unavailable",
                        argv[0],
                        timeout.as_millis()
                    ),
                    probed_at: utc_now(),
                });
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    // Bounded memory: stderr is drained and discarded in a background
    // thread; stdout is read incrementally and only the first
    // `max_output_bytes` are retained — a loud probe never balloons the
    // office's memory (QA round small item).
    let stderr_pipe = child.stderr.take();
    let stderr_drain = std::thread::spawn(move || {
        if let Some(mut pipe) = stderr_pipe {
            use std::io::Read as _;
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
            }
        }
    });
    let mut kept_bytes: Vec<u8> = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        use std::io::Read as _;
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = max_output_bytes.saturating_sub(kept_bytes.len());
                    if room > 0 {
                        kept_bytes.extend_from_slice(&buf[..n.min(room)]);
                    }
                }
            }
        }
    }
    let status = child.wait()?;
    let _ = stderr_drain.join();
    let kept = String::from_utf8_lossy(&kept_bytes).into_owned();
    Ok(ToolProbeResult {
        argv: argv.to_vec(),
        available: status.success(),
        detail: if status.success() {
            format!("tool `{}` answered successfully", argv[0])
        } else {
            format!(
                "tool `{}` exited with {status}; output (bounded): {kept}",
                argv[0]
            )
        },
        probed_at: utc_now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::store::{DOMAIN_FOUNDATION, FOUNDATION_V1_SQL};

    fn store() -> Store {
        let frozen = MigrationRegistry::new()
            .register(DOMAIN_FOUNDATION, 1, "foundation v1", FOUNDATION_V1_SQL)
            .register(DOMAIN_MEMBERS, 1, "members v1", MEMBERS_V1_SQL)
            .freeze()
            .expect("registry");
        Store::open_in_memory(&frozen).expect("store")
    }

    #[test]
    fn member_id_and_history_survive_binding_changes() {
        let store = store();
        let registry = MemberRegistry::new(&store);
        let member = registry.register("Samuel").expect("register");
        registry
            .set_binding(&MemberBinding {
                member_id: member.member_id.clone(),
                role: "developer".into(),
                model_binding: "glm-5.3-flash".into(),
                tools: vec!["pi-cli".into()],
                updated_at: utc_now(),
            })
            .expect("binding");

        // Rebinding changes configuration only.
        registry
            .set_binding(&MemberBinding {
                member_id: member.member_id.clone(),
                role: "reviewer".into(),
                model_binding: "glm-5.3-air".into(),
                tools: vec![],
                updated_at: utc_now(),
            })
            .expect("rebinding");

        let reloaded = registry.require(&member.member_id).expect("member");
        assert_eq!(reloaded.member_id, member.member_id, "identity unchanged");
        assert_eq!(reloaded.created_at, member.created_at);
        let binding = registry
            .binding(&member.member_id)
            .expect("binding")
            .expect("set");
        assert_eq!(binding.role, "reviewer");
        assert!(binding.tools.is_empty());
    }

    #[test]
    fn multiple_members_coexist() {
        let store = store();
        let registry = MemberRegistry::new(&store);
        let a = registry.register("Samuel").expect("a");
        let b = registry.register("Rook").expect("b");
        assert_ne!(
            a.member_id, b.member_id,
            "names are config, ids are identity"
        );
        assert_eq!(registry.list().expect("list").len(), 2);
        assert!(registry.binding(&a.member_id).expect("binding").is_none());
    }

    #[test]
    fn empty_display_name_is_rejected() {
        let store = store();
        let registry = MemberRegistry::new(&store);
        assert!(registry.register("   ").is_err());
    }

    #[test]
    fn unsupported_model_fails_with_actionable_error() {
        let policy = ModelPolicy::new(vec!["glm-5.3-flash".into(), "glm-5.3-air".into()]);
        let err = policy
            .ensure_supported("gpt-4o")
            .expect_err("unsupported model must fail");
        let text = err.to_string();
        assert!(text.contains("not supported"), "got: {text}");
        assert!(
            text.contains("glm-5.3-flash"),
            "must name the supported set: {text}"
        );
        policy.ensure_supported("glm-5.3-flash").expect("supported");
    }

    #[test]
    fn tool_probe_reports_missing_tool_actionably_and_caches() {
        let store = store();
        let registry = MemberRegistry::new(&store);

        let result = registry
            .probe_tool(
                vec!["definitely-not-a-real-tool-42".into()],
                Duration::from_secs(2),
                1024,
                false,
            )
            .expect("probe returns a result, not a panic");
        assert!(!result.available);
        assert!(
            result.detail.contains("cannot be launched"),
            "detail: {}",
            result.detail
        );

        // The negative result is cached under the exact argv.
        let again = registry
            .probe_tool(
                vec!["definitely-not-a-real-tool-42".into()],
                Duration::from_secs(2),
                1024,
                false,
            )
            .expect("cached");
        assert_eq!(again, result);
    }

    #[test]
    fn tool_probe_runs_real_explicit_argv() {
        let store = store();
        let registry = MemberRegistry::new(&store);
        let result = registry
            .probe_tool(
                vec!["/bin/echo".into(), "viva-probe".into()],
                Duration::from_secs(5),
                1024,
                false,
            )
            .expect("probe");
        assert!(result.available, "detail: {}", result.detail);

        // A probe of a real tool that exits non-zero still records a real,
        // bounded outcome (sh is present on every dev/CI machine).
        let failing = registry
            .probe_tool(
                vec!["/bin/sh".into(), "-c".into(), "exit 3".into()],
                Duration::from_secs(5),
                1024,
                false,
            )
            .expect("probe");
        assert!(!failing.available, "detail: {}", failing.detail);
        assert!(failing.detail.contains("exit 3") || failing.detail.contains("exited"));
    }

    #[test]
    fn probe_rejects_shell_strings_and_empty_argv() {
        let store = store();
        let registry = MemberRegistry::new(&store);
        assert!(
            registry
                .probe_tool(vec![], Duration::from_secs(1), 16, false)
                .is_err()
        );
        assert!(
            registry
                .probe_tool(vec!["  ".into()], Duration::from_secs(1), 16, false)
                .is_err()
        );
    }

    #[test]
    fn missing_member_is_a_not_found_error() {
        let store = store();
        let registry = MemberRegistry::new(&store);
        let err = registry
            .require(&MemberId::new())
            .expect_err("missing member");
        assert!(matches!(
            err,
            OfficeError::NotFound {
                entity: "member",
                ..
            }
        ));
    }
}
