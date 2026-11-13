//! Local runtime-controller capability, reusing Core channels and native
//! host-incarnation anchors. Inherited spawn descriptors and the private runtime
//! store carry the same proof. Possession is not human presence or a User role.
//! The bridge separately rejects known execution/host descendants and unknown
//! live work; member ingress retains its real execution, channel and live grant.
//! This is not an OS sandbox against arbitrary same-UID programs.
use super::owner_anchor;
use crate::foundation::envelope::{CallerIdentity, CallerRole, ChannelRegistry};
use herdr::embedding::{TransportPeer, peer_lifetime_marker};
use std::{collections::HashMap, io};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthRefusal {
    NotReady,
    Unavailable,
    Missing,
    Stale,
    Mismatch,
    ClientLimit,
}
#[derive(Debug)]
pub(super) enum AuthOutcome {
    Refused(AuthRefusal),
    Internal(io::Error),
}

impl AuthRefusal {
    pub(super) fn reason(self) -> &'static str {
        match self {
            Self::NotReady => "client_not_ready",
            Self::Unavailable => "owner_anchor_unavailable",
            Self::Missing => "owner_anchor_missing",
            Self::Stale => "stale_owner_anchor",
            Self::Mismatch => "owner_anchor_mismatch",
            Self::ClientLimit => "owner_client_limit",
        }
    }
    pub(super) fn into_error(self, detail: &str) -> io::Error {
        let kind = match self {
            Self::NotReady => io::ErrorKind::WouldBlock,
            Self::Unavailable => io::ErrorKind::Unsupported,
            Self::Missing | Self::Stale | Self::Mismatch | Self::ClientLimit => {
                io::ErrorKind::PermissionDenied
            }
        };
        io::Error::new(kind, format!("{}: {detail}", self.reason()))
    }
}

struct Bootstrap {
    channel: CallerIdentity,
    anchor: String,
    host_id: String,
}
struct BoundClient {
    peer: TransportPeer,
    lifetime: String,
    channel: CallerIdentity,
}
pub(super) struct ClientAuthority {
    channels: ChannelRegistry,
    bootstrap: Option<Bootstrap>,
    activated: bool,
    clients: HashMap<u32, BoundClient>,
}
fn denied(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
impl ClientAuthority {
    pub(super) fn new() -> Self {
        Self {
            channels: ChannelRegistry::new(),
            bootstrap: None,
            activated: false,
            clients: HashMap::new(),
        }
    }
    /// Called only after both native endpoints have been claimed. `anchor` is
    /// the credential accepted from the spawning TUI or private runtime file;
    /// `None` leaves the host running with controller admission refused.
    /// A failed competing startup must not activate a second authority.
    pub(super) fn activate(&mut self, anchor: Option<String>) -> io::Result<()> {
        if self.activated {
            return Err(denied("native client authority already activated".into()));
        }
        self.activated = true;
        let Some(anchor) = anchor else {
            return Ok(());
        };
        let host_id = owner_anchor::host_id(&anchor)
            .ok_or_else(|| denied("native owner anchor has an unknown format".into()))?
            .to_string();
        let caller = self.channels.issue(CallerRole::AgentCli);
        self.bootstrap = Some(Bootstrap {
            channel: caller,
            anchor,
            host_id,
        });
        Ok(())
    }
    pub(super) fn authenticate(
        &mut self,
        credential: &str,
        peer: TransportPeer,
        expected_lifetime: Option<&str>,
        admitted: impl FnOnce(&CallerIdentity, &str) -> io::Result<()>,
    ) -> Result<(), AuthOutcome> {
        if !self.activated {
            return Err(AuthOutcome::Refused(AuthRefusal::NotReady));
        }
        let bootstrap = self
            .bootstrap
            .as_ref()
            .ok_or(AuthOutcome::Refused(AuthRefusal::Unavailable))?;
        if credential.is_empty() {
            return Err(AuthOutcome::Refused(AuthRefusal::Missing));
        }
        if credential != bootstrap.anchor {
            let presented = owner_anchor::host_id(credential)
                .ok_or(AuthOutcome::Refused(AuthRefusal::Mismatch))?;
            let refusal = if presented != bootstrap.host_id {
                AuthRefusal::Stale
            } else {
                AuthRefusal::Mismatch
            };
            return Err(AuthOutcome::Refused(refusal));
        }
        if self.channels.validate(&bootstrap.channel) != Ok(CallerRole::AgentCli) {
            return Err(AuthOutcome::Refused(AuthRefusal::Mismatch));
        }
        let pid = peer.pid.ok_or(AuthOutcome::Refused(AuthRefusal::Missing))?;
        #[cfg(unix)]
        {
            if peer.uid != Some(unsafe { libc::geteuid() }) {
                return Err(AuthOutcome::Refused(AuthRefusal::Mismatch));
            }
        }
        let lifetime =
            peer_lifetime_marker(peer).map_err(|_| AuthOutcome::Refused(AuthRefusal::Missing))?;
        // Bind the credential-bearing message to its sender's own lifetime.
        // A delayed request from a dead PID must not bind a reused PID instead.
        // The claim is only compared to the OS fact; it never overrides it.
        if expected_lifetime != Some(lifetime.as_str()) {
            return Err(AuthOutcome::Refused(AuthRefusal::Missing));
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
            return Err(AuthOutcome::Refused(AuthRefusal::ClientLimit));
        }
        // Audit BEFORE installing a usable binding. Channel IDs are safe
        // correlation facts; unlike the anchor they are not secrets.
        let channel = self.channels.issue(CallerRole::AgentCli);
        if let Err(error) = admitted(&channel, &lifetime) {
            let _ = self.channels.revoke(&channel.channel_id);
            return Err(AuthOutcome::Internal(error));
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
            .ok_or_else(|| denied("native client PID unavailable".into()))?;
        let client = self
            .clients
            .get(&pid)
            .ok_or_else(|| denied("native raw client has no host-issued binding".into()))?;
        if client.peer != peer
            || self.channels.validate(&client.channel) != Ok(CallerRole::AgentCli)
            || peer_lifetime_marker(peer)? != client.lifetime
        {
            return Err(denied(
                "native client process binding is no longer live".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests;
