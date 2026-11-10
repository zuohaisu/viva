use super::*;
fn fixture() -> (tempfile::TempDir, ClientAuthority, String, TransportPeer) {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("runtime/workbench")).unwrap();
    let mut authority = ClientAuthority::new(home.path());
    authority.activate("native-unit-fixture").unwrap();
    let credential = client_credential(home.path()).unwrap();
    let peer = TransportPeer {
        pid: Some(std::process::id()),
        uid: Some(unsafe { libc::geteuid() }),
    };
    (home, authority, credential, peer)
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
        clients: Mutex::new(ClientAuthority::new(home.path())),
        channels: Mutex::new(crate::foundation::envelope::ChannelRegistry::new()),
        executions: Mutex::new(super::super::NativeExecutionState::default()),
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
    let (_home, mut authority, credential, peer) = fixture();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    assert!(
        authority
            .authenticate(&credential, peer, None, |_, _| panic!(
                "missing lifetime admitted"
            ))
            .is_err()
    );
    assert!(
        authority
            .authenticate(&credential, peer, Some("wrong-lifetime"), |_, _| panic!(
                "wrong lifetime admitted"
            ))
            .is_err()
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
        channel_id, credential,
        "an identifier must not be the credential"
    );
    assert!(
        authority
            .authenticate(&channel_id, peer, Some(&lifetime), |_, _| panic!(
                "identifier admitted as a secret"
            ))
            .is_err()
    );
    assert!(
        authority
            .authenticate("unknown", peer, Some(&lifetime), |_, _| panic!(
                "bad proof reached audit"
            ))
            .is_err()
    );
    assert!(
        authority
            .authenticate(
                &credential,
                TransportPeer { pid: None, ..peer },
                Some(&lifetime),
                |_, _| panic!("missing PID admitted")
            )
            .is_err()
    );
    assert!(
        authority
            .authenticate(
                &credential,
                TransportPeer { uid: None, ..peer },
                Some(&lifetime),
                |_, _| panic!("missing UID admitted")
            )
            .is_err()
    );
    authority
        .authenticate(&credential, peer, Some(&lifetime), |_, _| Ok(()))
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
fn failed_admission_audit_leaves_no_usable_client_channel() {
    let (_home, mut authority, credential, peer) = fixture();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    assert!(
        authority
            .authenticate(&credential, peer, Some(&lifetime), |_, _| Err(
                io::Error::other("fixture audit failure")
            ))
            .is_err()
    );
    assert!(authority.validate(peer).is_err());
    assert!(authority.clients.is_empty());
}
#[test]
fn stale_capability_rotates_but_unknown_file_and_symlink_are_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let (home, mut authority, credential, peer) = fixture();
    let lifetime = peer_lifetime_marker(peer).unwrap();
    assert_eq!(
        std::fs::metadata(&authority.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(authority.activate("native-double").is_err());
    let mut next = ClientAuthority::new(home.path());
    next.activate("native-next-fixture").unwrap();
    assert_ne!(credential, client_credential(home.path()).unwrap());
    assert!(
        next.authenticate(&credential, peer, Some(&lifetime), |_, _| panic!(
            "stale credential admitted"
        ))
        .is_err()
    );
    std::fs::write(&next.path, b"unrelated private fixture").unwrap();
    let mut unknown = ClientAuthority::new(home.path());
    assert!(unknown.activate("native-unknown").is_err());
    assert_eq!(
        std::fs::read(&next.path).unwrap(),
        b"unrelated private fixture"
    );
    let sentinel = home.path().join("sentinel");
    std::fs::write(&sentinel, b"unchanged").unwrap();
    std::fs::remove_file(&next.path).unwrap();
    std::os::unix::fs::symlink(&sentinel, &next.path).unwrap();
    assert!(unknown.activate("native-symlink").is_err());
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
    std::fs::remove_file(&next.path).unwrap();
    std::os::unix::fs::symlink(home.path().join("missing"), &next.path).unwrap();
    assert!(unknown.activate("native-dangling-symlink").is_err());
    assert!(
        std::fs::symlink_metadata(&next.path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
