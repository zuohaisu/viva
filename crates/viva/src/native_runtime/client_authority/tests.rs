use super::*;
use crate::native_runtime::owner_anchor;

#[test]
fn runtime_capability_never_claims_human_user_role() {
    let (mut authority, anchor, peer) = fixture();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    authority
        .authenticate(&anchor, peer, Some(&lifetime), |channel, _| {
            assert_eq!(channel.claimed_role, CallerRole::AgentCli);
            Ok(())
        })
        .unwrap();
}

fn fixture() -> (ClientAuthority, String, TransportPeer) {
    let (_, anchor) = owner_anchor::generate();
    let mut authority = ClientAuthority::new();
    authority.activate(Some(anchor.clone())).unwrap();
    let peer = TransportPeer {
        pid: Some(std::process::id()),
        uid: Some(unsafe { libc::geteuid() }),
    };
    (authority, anchor, peer)
}

fn refused(outcome: Result<(), AuthOutcome>) -> AuthRefusal {
    match outcome {
        Err(AuthOutcome::Refused(reason)) => reason,
        Err(AuthOutcome::Internal(error)) => panic!("unexpected internal error: {error}"),
        Ok(()) => panic!("authentication unexpectedly admitted"),
    }
}

#[test]
fn active_worker_credential_without_execution_blocks_owner_only_phase() {
    use crate::{authority::AuthorityEngine, foundation::store::Store, members::MemberRegistry};
    use std::sync::Mutex;
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory(crate::server::migrations()).unwrap();
    let member = MemberRegistry::new(&store)
        .register("active agent CLI fixture")
        .unwrap();
    let mut channels = ChannelRegistry::new();
    let caller = channels.issue(CallerRole::AgentCli);
    AuthorityEngine::new(&store)
        .issue_worker_credential(&caller.channel_id, None, member.member_id)
        .unwrap();
    let bridge = super::super::DomainBridge {
        home: home.path().into(),
        host_id: "native-guard-unit".into(),
        store: Mutex::new(Some(store)),
        clients: Mutex::new(ClientAuthority::new()),
        owner_anchor: Mutex::new(None),
        pre_provisioned_anchor: None,
        channels: Mutex::new(crate::foundation::envelope::ChannelRegistry::new()),
        executions: Mutex::new(super::super::NativeExecutionState::default()),
        legacy_handoff: None,
    };
    assert!(bridge.require_owner_only_phase().is_err());
    assert!(
        !home
            .path()
            .join("runtime/workbench/owner-channel.json")
            .exists()
    );
}

// Constructed peer facts: channel/lifetime fixtures, not socket authentication
// or Native member-process evidence. Real bridge/probe tests cover transport.
#[test]
fn owner_capability_needs_proof_and_live_process_not_uid_alone() {
    let (mut authority, anchor, peer) = fixture();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    assert_eq!(
        refused(authority.authenticate(&anchor, peer, None, |_, _| panic!(
            "missing lifetime admitted"
        ))),
        AuthRefusal::Missing
    );
    assert_eq!(
        refused(
            authority.authenticate(&anchor, peer, Some("wrong-lifetime"), |_, _| panic!(
                "wrong lifetime admitted"
            ))
        ),
        AuthRefusal::Missing
    );
    assert!(authority.validate(peer).is_err());
    let channel_id = authority
        .bootstrap
        .as_ref()
        .unwrap()
        .channel
        .channel_id
        .to_string();
    assert_ne!(
        channel_id, anchor,
        "an identifier must not be the credential"
    );
    assert_eq!(
        refused(
            authority.authenticate(&channel_id, peer, Some(&lifetime), |_, _| panic!(
                "identifier admitted as a secret"
            ))
        ),
        AuthRefusal::Mismatch
    );
    assert_eq!(
        refused(
            authority.authenticate("unknown", peer, Some(&lifetime), |_, _| panic!(
                "bad proof reached audit"
            ))
        ),
        AuthRefusal::Mismatch
    );
    assert_eq!(
        refused(authority.authenticate(
            &anchor,
            TransportPeer { pid: None, ..peer },
            Some(&lifetime),
            |_, _| panic!("missing PID admitted")
        )),
        AuthRefusal::Missing
    );
    assert_eq!(
        refused(authority.authenticate(
            &anchor,
            TransportPeer { uid: None, ..peer },
            Some(&lifetime),
            |_, _| panic!("missing UID admitted")
        )),
        AuthRefusal::Mismatch
    );
    authority
        .authenticate(&anchor, peer, Some(&lifetime), |_, _| Ok(()))
        .unwrap();
    authority.validate(peer).unwrap();
    authority
        .clients
        .get_mut(&std::process::id())
        .unwrap()
        .lifetime = "not-the-kernel-lifetime".into();
    assert!(
        authority.validate(peer).is_err(),
        "mismatched lifetime must not retain authority"
    );
}

#[test]
fn an_anchor_from_a_dead_incarnation_is_refused_as_stale() {
    let (mut authority, _anchor, peer) = fixture();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    let stale = owner_anchor::with_host("native-dead-incarnation");
    assert_eq!(
        refused(
            authority.authenticate(&stale, peer, Some(&lifetime), |_, _| panic!(
                "stale anchor admitted"
            ))
        ),
        AuthRefusal::Stale
    );
    assert_eq!(AuthRefusal::Stale.reason(), "stale_owner_anchor");
    assert!(authority.clients.is_empty());
}

#[test]
fn a_foreign_anchor_for_the_live_host_is_refused_as_mismatch() {
    let (mut authority, anchor, peer) = fixture();
    let host = owner_anchor::host_id(&anchor).unwrap();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    let forged = owner_anchor::with_host(host);
    assert_eq!(
        refused(
            authority.authenticate(&forged, peer, Some(&lifetime), |_, _| panic!(
                "forged anchor admitted"
            ))
        ),
        AuthRefusal::Mismatch
    );
}

#[test]
fn activation_without_an_anchor_refuses_every_authentication() {
    let mut authority = ClientAuthority::new();
    authority.activate(None).unwrap();
    let peer = TransportPeer {
        pid: Some(std::process::id()),
        uid: Some(unsafe { libc::geteuid() }),
    };
    let (_, anchor) = owner_anchor::generate();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    assert_eq!(
        refused(
            authority.authenticate(&anchor, peer, Some(&lifetime), |_, _| panic!(
                "anchor admitted without activation anchor"
            ))
        ),
        AuthRefusal::Unavailable
    );
    assert_eq!(
        AuthRefusal::Unavailable.reason(),
        "owner_anchor_unavailable"
    );
}

#[test]
fn a_second_activation_is_rejected() {
    let (mut authority, anchor, _peer) = fixture();
    assert!(authority.activate(Some(anchor)).is_err());
}

#[test]
fn failed_admission_audit_leaves_no_usable_client_channel() {
    let (mut authority, anchor, peer) = fixture();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    match authority.authenticate(&anchor, peer, Some(&lifetime), |_, _| {
        Err(io::Error::other("fixture audit failure"))
    }) {
        Err(AuthOutcome::Internal(_)) => {}
        _ => panic!("audit failure must be internal, not admitted"),
    }
    assert!(authority.validate(peer).is_err());
    assert!(authority.clients.is_empty());
}
