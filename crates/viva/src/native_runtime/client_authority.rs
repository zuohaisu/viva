//! Owner-frontend capability for the gated native development runtime.
//! Reuses Core channels. This is not MemberExecution issuance and not a same-UID
//! sandbox. The parent bridge refuses this owner-only phase if any execution is
//! requested/running or a worker credential is active; real member/agent process
//! and descriptor binding must replace that guard
//! BEFORE native member dispatch is enabled.
use crate::foundation::envelope::{CallerIdentity, CallerRole, ChannelRegistry};
use herdr::embedding::{TransportPeer, peer_lifetime_marker};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

const FILE_NAME: &str = "owner-channel.json";
#[derive(Serialize, Deserialize)]
struct OwnerFile {
    schema: u32,
    host_id: String,
    credential: String,
}
struct Bootstrap {
    channel: CallerIdentity,
    credential: String,
}
struct BoundClient {
    peer: TransportPeer,
    lifetime: String,
    channel: CallerIdentity,
}
pub(super) struct ClientAuthority {
    channels: ChannelRegistry,
    bootstrap: Option<Bootstrap>,
    clients: HashMap<u32, BoundClient>,
    path: PathBuf,
}
fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn private_file(path: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(denied("native client capability is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: argument-free OS query, no mutable memory.
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(denied(
                "native client capability is not private to this account",
            ));
        }
    }
    Ok(())
}
pub(super) fn client_credential(home: &Path) -> io::Result<String> {
    let path = home.join("runtime/workbench").join(FILE_NAME);
    private_file(&path)?;
    let file: OwnerFile = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| denied("invalid native client capability file"))?;
    if file.schema != 1 || !file.host_id.starts_with("native-") {
        return Err(denied("unknown native client capability format"));
    }
    Ok(file.credential)
}
impl ClientAuthority {
    pub(super) fn new(home: &Path) -> Self {
        Self {
            channels: ChannelRegistry::new(),
            bootstrap: None,
            clients: HashMap::new(),
            path: home.join("runtime/workbench").join(FILE_NAME),
        }
    }
    /// Called only after both native endpoints have been claimed. A failed
    /// competing startup never rotates an already-live host's capability.
    pub(super) fn activate(&mut self, host_id: &str) -> io::Result<()> {
        if self.bootstrap.is_some() {
            return Err(denied("native client authority already activated"));
        }
        match std::fs::symlink_metadata(&self.path) {
            Ok(_) => {
                // Unlike try_exists, this also rejects a dangling symlink.
                private_file(&self.path)?;
                let old: OwnerFile = serde_json::from_slice(&std::fs::read(&self.path)?)
                    .map_err(|_| denied("refusing to replace an unknown capability file"))?;
                if old.schema != 1 || !old.host_id.starts_with("native-") {
                    return Err(denied("refusing to replace an unknown capability file"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let caller = self.channels.issue(CallerRole::User);
        // Channel IDs are identifiers, not bearer credentials. Keep a separate
        // random secret, using the same proven UUID source as Core credentials.
        let credential = format!("native-cap-{}", uuid::Uuid::new_v4().simple());
        let record = OwnerFile {
            schema: 1,
            host_id: host_id.into(),
            credential: credential.clone(),
        };
        let mut temp =
            tempfile::NamedTempFile::new_in(self.path.parent().expect("runtime directory"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        use std::io::Write;
        temp.write_all(&serde_json::to_vec(&record).map_err(io::Error::other)?)?;
        temp.as_file().sync_all()?;
        temp.persist(&self.path).map_err(io::Error::other)?;
        self.bootstrap = Some(Bootstrap {
            channel: caller,
            credential,
        });
        Ok(())
    }
    pub(super) fn authenticate(
        &mut self,
        credential: &str,
        peer: TransportPeer,
        expected_lifetime: Option<&str>,
        admitted: impl FnOnce(&CallerIdentity, &str) -> io::Result<()>,
    ) -> io::Result<()> {
        let bootstrap = self.bootstrap.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "native client authority not ready",
            )
        })?;
        if bootstrap.credential != credential
            || self.channels.validate(&bootstrap.channel) != Ok(CallerRole::User)
        {
            return Err(denied("untrusted native client capability"));
        }
        let pid = peer
            .pid
            .ok_or_else(|| denied("native client PID unavailable"))?;
        #[cfg(unix)]
        {
            if peer.uid != Some(unsafe { libc::geteuid() }) {
                return Err(denied("native client account mismatch"));
            }
        }
        let lifetime = peer_lifetime_marker(peer)?;
        // Bind the credential-bearing message to its sender's own lifetime.
        // A delayed request from a dead PID must not bind a reused PID instead.
        // The claim is only compared to the OS fact; it never overrides it.
        if expected_lifetime != Some(lifetime.as_str()) {
            return Err(denied(
                "native client lifetime proof does not match the OS process",
            ));
        }
        let stale: Vec<_> = self
            .clients
            .iter()
            .filter_map(|(pid, client)| {
                (peer_lifetime_marker(client.peer).ok().as_ref() != Some(&client.lifetime))
                    .then_some(*pid)
            })
            .collect();
        for pid in stale {
            if let Some(client) = self.clients.remove(&pid) {
                let _ = self.channels.revoke(&client.channel.channel_id);
            }
        }
        if let Some(old) = self.clients.remove(&pid) {
            let _ = self.channels.revoke(&old.channel.channel_id);
        }
        if self.clients.len() >= 128 {
            return Err(denied("native client binding limit reached"));
        }
        // Audit BEFORE installing a usable binding. Channel IDs are safe
        // correlation facts; unlike the bootstrap credential they are not secrets.
        let channel = self.channels.issue(CallerRole::User);
        if let Err(error) = admitted(&channel, &lifetime) {
            let _ = self.channels.revoke(&channel.channel_id);
            return Err(error);
        }
        self.clients.insert(
            pid,
            BoundClient {
                peer,
                lifetime,
                channel,
            },
        );
        Ok(())
    }
    pub(super) fn validate(&self, peer: TransportPeer) -> io::Result<()> {
        let pid = peer
            .pid
            .ok_or_else(|| denied("native client PID unavailable"))?;
        let client = self
            .clients
            .get(&pid)
            .ok_or_else(|| denied("native raw client has no host-issued binding"))?;
        if client.peer != peer
            || self.channels.validate(&client.channel) != Ok(CallerRole::User)
            || peer_lifetime_marker(peer)? != client.lifetime
        {
            return Err(denied("native client process binding is no longer live"));
        }
        Ok(())
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
