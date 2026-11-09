//! Development-only native composition (#191). Herdr owns the PTYs and renderer;
//! Viva owns member/task facts and refusal auditing. This is deliberately not
//! wired to the installed default until authority and cross-backend import pass.
use crate::{
    foundation::{
        error::{Error, Result},
        events::{self, NewEvent},
        paths,
        store::Store,
    },
    members::MemberRegistry,
    tasks::TaskRegistry,
};
use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct DomainBridge {
    home: PathBuf,
    host_id: String,
    store: Mutex<Option<Store>>,
}

impl DomainBridge {
    fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> io::Result<T> {
        let mut slot = self
            .store
            .lock()
            .map_err(|_| io::Error::other("native domain lock poisoned"))?;
        if slot.is_none() {
            paths::ensure_private_dir(&self.home)?;
            let path = paths::database_path(&self.home);
            // Existing data is never migrated/reconciled by this preview. Reuse
            // exact-schema attach; normal crash reconciliation would incorrectly
            // stop executions whose live ownership has not been integrated yet.
            *slot = Some(
                if path.exists() {
                    Store::attach_for_handoff(&path, crate::server::migrations())
                } else {
                    Store::open(&path, crate::server::migrations())
                }
                .map_err(io::Error::other)?,
            );
        }
        f(slot.as_ref().expect("store initialized")).map_err(io::Error::other)
    }
}

impl herdr::embedding::DomainServices for DomainBridge {
    fn server_started(&self) -> io::Result<()> {
        self.with_store(|store| {
            events::append(store, NewEvent {
                domain: crate::server::DOMAIN_HOST,
                kind: "native_domain_attached".into(),
                subject_type: "runtime_process".into(),
                subject_id: self.host_id.clone(),
                origin: "native_host".into(),
                payload: serde_json::json!({"pid": std::process::id(), "backend": "herdr-source-library", "state": "domain_attached_not_listener_ready"}),
            })?;
            Ok(())
        })
    }
    fn snapshot(&self) -> io::Result<serde_json::Value> {
        self.with_store(|store| {
            Ok(serde_json::json!({
                "schema": 1,
                "backend": "herdr-source-library",
                "runtime_pid": std::process::id(),
                "runtime_host_id": self.host_id,
                "execution_attribution": "not_integrated",
                "integration_state": "development-read-only",
                "member_dispatch": "not_integrated",
                "members": MemberRegistry::new(store).list()?,
                "tasks": TaskRegistry::new(store).list_tasks(0, 100)?,
                "tasks_limit": 100,
            }))
        })
    }
    fn refused(
        &self,
        method: &'static str,
        peer: Option<herdr::embedding::TransportPeer>,
    ) -> io::Result<()> {
        self.with_store(|store| {
            events::append(store, NewEvent {
                domain: crate::server::DOMAIN_HOST,
                kind: "native_automation_refused".into(),
                subject_type: "native_ingress".into(),
                subject_id: uuid::Uuid::new_v4().to_string(),
                origin: match method {
                    "native_cli.unsupported" => "native_cli:unbound",
                    "plugin.command" => "native_plugin:unbound",
                    _ => "native_api:unbound",
                }.into(),
                payload: serde_json::json!({"method": method, "actor": "unbound", "reason": "authority_not_integrated", "transport_peer": peer.map(|peer| serde_json::json!({"pid": peer.pid, "uid": peer.uid}))}),
            })?;
            Ok(())
        })
    }
}

pub fn run(args: &[String]) -> Result<()> {
    // No accidental installation/user-home trial before the default cutover gate.
    if std::env::var(paths::VIVA_HOME_ENV)
        .ok()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(Error::Validation("native development preview requires an explicit isolated VIVA_HOME; it is not an installation/upgrade acceptance build".into()));
    }
    if crate::machines::invocation_target().is_some() {
        return Err(Error::Validation(
            "native preview remote machine ingress is not integrated; refusing local fallback"
                .into(),
        ));
    }
    let home = paths::viva_home(None);
    paths::ensure_private_dir(&home)?;
    // Temporary development admission gate, not the final startup behavior.
    // Never launch a second execution host beside a legacy Viva host. The
    // production workbench still uses automatic same-backend takeover (#196).
    // Cross-backend inheritance remains required BEFORE default cutover.
    if crate::server::Client::connect(&home).is_ok() {
        return Err(Error::Validation("native development preview cannot yet inherit this live legacy host; it has been left untouched. This preview is not the #191 default-startup implementation.".into()));
    }
    eprintln!(
        "native development preview: use isolated HOME/VIVA_HOME only; member execution and cross-backend inheritance are not integrated; not an installation acceptance build"
    );
    let mut arguments = vec!["viva-runtime".to_owned()];
    arguments.extend_from_slice(args);
    herdr::embedding::run(herdr::embedding::Context {
        directory: home.join("runtime").join("workbench"),
        arguments,
        child_prefix: vec!["runtime".into()],
        domain: Arc::new(DomainBridge {
            home,
            host_id: format!("native-{}", uuid::Uuid::new_v4().simple()),
            store: Mutex::new(None),
        }),
    })?;
    Ok(())
}
