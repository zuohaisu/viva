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
