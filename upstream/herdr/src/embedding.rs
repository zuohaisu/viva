// Viva #191 integration glue. Standalone Herdr keeps its original paths and
// behavior; embedding is explicit and does not rewrite HOME/XDG/HERDR_*.
use std::{
    io,
    path::PathBuf,
    process::Command,
    sync::{Arc, OnceLock},
};

/// Narrow dependency inversion: the runtime does not own Viva domain records.
/// No dispatch/stop capability is exposed until ingress authority is integrated.
pub trait DomainServices: Send + Sync {
    fn server_started(&self) -> io::Result<()>;
    fn server_ready(&self) -> io::Result<()>;
    fn client_credential(&self) -> io::Result<String>;
    fn authenticate_client(
        &self,
        credential: &str,
        expected_lifetime: Option<&str>,
        peer: TransportPeer,
    ) -> io::Result<()>;
    fn validate_client(&self, peer: TransportPeer) -> io::Result<()>;
    fn snapshot(&self) -> io::Result<serde_json::Value>;
    fn refused(&self, method: &'static str, peer: Option<TransportPeer>) -> io::Result<()>;
}

/// OS-reported transport facts, NOT a member identity or owner credential.
/// PID reuse/inherited descriptors require lifecycle validation before authority.
/// Deliberately not Deserialize: request JSON cannot construct this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportPeer {
    pub pid: Option<u32>,
    pub uid: Option<u32>,
}

/// Revalidate this at admission: a PID/UID alone is not a live process binding.
pub fn peer_lifetime_marker(peer: TransportPeer) -> io::Result<String> {
    let pid = peer.pid.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "connection PID unavailable",
        )
    })?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        crate::platform::process_lifetime_marker(pid).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "connection process lifetime unavailable",
            )
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native process binding unavailable",
        ))
    }
}

pub(crate) fn server_ready() -> io::Result<()> {
    if let Some(ctx) = context() {
        ctx.domain.server_ready()?;
    }
    Ok(())
}

pub(crate) fn authenticate_client(
    credential: &str,
    expected_lifetime: Option<&str>,
    stream: &crate::ipc::LocalStream,
) -> io::Result<()> {
    let ctx = context().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Viva embedding unavailable",
        )
    })?;
    ctx.domain.authenticate_client(
        credential,
        expected_lifetime,
        crate::ipc::transport_peer(stream)?,
    )
}

pub(crate) fn validate_client(stream: &crate::ipc::LocalStream) -> io::Result<()> {
    if let Some(ctx) = context() {
        ctx.domain
            .validate_client(crate::ipc::transport_peer(stream)?)?;
    }
    Ok(())
}

pub(crate) fn authenticate_local_client() -> io::Result<()> {
    let Some(ctx) = context() else {
        return Ok(());
    };
    // The client socket can appear just before the ready capability is published.
    // Retry only readiness or a visible private-file credential rotation,
    // never an unchanged bad credential and never an execution effect.
    use crate::api::client::{parse_response_value, ApiClient, ApiClientError};
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let credential = match ctx.domain.client_credential() {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => return Err(error),
        };
        let request = crate::api::schema::Request {
            id: "viva.client-authenticate".into(),
            method: crate::api::schema::Method::VivaClientAuthenticate(
                crate::api::schema::VivaClientAuthenticateParams {
                    credential: credential.clone(),
                    process_lifetime: Some(peer_lifetime_marker(TransportPeer {
                        pid: Some(std::process::id()),
                        uid: None,
                    })?),
                },
            ),
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "native client authentication readiness timed out",
            ));
        }
        match ApiClient::local()
            .request_value_with_timeout(&request, remaining)
            .and_then(parse_response_value)
        {
            Ok(_) => return Ok(()),
            Err(ApiClientError::ErrorResponse(error))
                if error.error.code == "client_not_ready" && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(ApiClientError::ErrorResponse(error))
                if error.error.code == "untrusted_client"
                    && Instant::now() < deadline
                    && ctx
                        .domain
                        .client_credential()
                        .is_ok_and(|latest| latest != credential) =>
            {
                // A stale file can be read just before the ready host atomically
                // replaces it. Reload the changed proof; do not retry a bad key.
                continue;
            }
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

pub struct Context {
    pub directory: PathBuf,
    pub arguments: Vec<String>,
    pub child_prefix: Vec<String>,
    pub domain: Arc<dyn DomainServices>,
}

static CONTEXT: OnceLock<Context> = OnceLock::new();

pub fn context() -> Option<&'static Context> {
    CONTEXT.get()
}
pub fn enabled() -> bool {
    context().is_some()
}

/// Development composition entry, not the installed default or a release gate.
/// A second call in one process is rejected rather than changing live authority.
pub fn run(context: Context) -> io::Result<()> {
    if let Err(error) = validate_arguments(&context.arguments) {
        context.domain.refused("native_cli.unsupported", None)?;
        return Err(error);
    }
    std::fs::create_dir_all(&context.directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&context.directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let args = context.arguments.clone();
    CONTEXT
        .set(context)
        .map_err(|_| io::Error::other("native runtime context already installed"))?;
    crate::run_args(args)
}

fn validate_arguments(args: &[String]) -> io::Result<()> {
    // These operations must be integrated with Viva first. In particular, never
    // let Herdr's updater replace a Viva executable or use an external bridge.
    let supported_namespace = matches!(
        args.get(1).map(String::as_str),
        None | Some(
            "server"
                | "pane"
                | "tab"
                | "workspace"
                | "api"
                | "status"
                | "--version"
                | "-V"
                | "version"
                | "--help"
                | "-h"
                | "help"
        )
    );
    if args.iter().skip(1).any(|arg| {
        arg == "--session"
            || arg.starts_with("--session=")
            || arg == "--remote"
            || arg.starts_with("--remote=")
            || arg == "--handoff-import"
    }) || !supported_namespace
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "native preview operation is not integrated with Viva authority; no legacy UI fallback",
        ));
    }
    Ok(())
}

pub(crate) fn arguments() -> Vec<String> {
    context()
        .map(|ctx| ctx.arguments.clone())
        .unwrap_or_else(|| std::env::args().collect())
}

pub(crate) fn child_command(executable: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(executable);
    if let Some(ctx) = context() {
        command.args(&ctx.child_prefix);
    }
    command
}

pub(crate) fn server_started() -> io::Result<()> {
    if let Some(ctx) = context() {
        ctx.domain.server_started()?;
    }
    Ok(())
}

pub(crate) fn snapshot() -> io::Result<serde_json::Value> {
    context()
        .ok_or_else(|| io::Error::other("Viva domain services are not embedded"))?
        .domain
        .snapshot()
}

pub(crate) fn constrain_config(config: &mut crate::config::Config) {
    if enabled() {
        config.update.version_check = false;
        config.update.manifest_check = false;
        config.onboarding = Some(false);
        // Native output is not yet integrated with Viva persistence redaction.
        config.experimental.pane_history = false;
    }
}

pub(crate) fn admit_from_socket(
    request: &crate::api::schema::Request,
    stream: &crate::ipc::LocalStream,
) -> io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    // Captured from the accepted connection, never a request/pane/env claim.
    let peer = crate::ipc::transport_peer(stream).ok();
    admit_with_peer(request, peer)
}

pub(crate) fn admit(request: &crate::api::schema::Request) -> io::Result<()> {
    admit_with_peer(request, None)
}

fn admit_with_peer(
    request: &crate::api::schema::Request,
    peer: Option<TransportPeer>,
) -> io::Result<()> {
    let Some(ctx) = context() else {
        return Ok(());
    };
    let method = crate::api::api_method_name(&request.method);
    if matches!(
        method,
        "ping"
            | "viva.snapshot"
            | "viva.client.authenticate"
            | "client_shell.surface.set"
            | "client.window_title.set"
            | "client.window_title.clear"
    ) {
        return Ok(());
    }
    // Never accept caller/member/grant claims from pane metadata or JSON params.
    // Unknown automated ingress is unbound, NOT the owner. Log only the schema's
    // fixed method name, never untrusted request IDs, tokens or command arguments.
    ctx.domain.refused(method, peer)?;
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "native automation ingress is not yet bound to Viva authority; operation refused",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_entry_refuses_unintegrated_namespaces() {
        for args in [
            vec!["viva", "update"],
            vec!["viva", "--session=x"],
            vec!["viva", "--remote", "host"],
            vec!["viva", "plugin", "run"],
        ] {
            assert_eq!(
                validate_arguments(&args.into_iter().map(str::to_owned).collect::<Vec<_>>())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        assert!(validate_arguments(&["viva".into(), "server".into()]).is_ok());
    }
}
