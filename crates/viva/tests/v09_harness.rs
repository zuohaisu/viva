//! V09 acceptance tests (issue #18): the Pi harness launch combination and
//! the extension-facing office endpoints. The TS extension package has its
//! own independent typecheck/test suite under `extensions/pi/`; these tests
//! cover the Rust side of the contract (launch context, honest
//! availability, handoff facts that never complete a task).

use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use viva::authority::{AuthorityEngine, GrantMode};
use viva::foundation::ids::MemberId;
use viva::harness::HarnessSpec;
use viva::harness::pi::{
    Availability, ENV_GRANT_ID, ENV_MEMBER_ID, ENV_MEMBER_NAME, ENV_TASK_ID, PiHarness,
};
use viva::members::{MemberBinding, MemberRegistry};
use viva::office::{self, OfficeHost, OfficeRequestKind};
use viva::tasks::TaskRegistry;

fn wait_until(what: &str, timeout: Duration, mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if probe() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {what}");
}

fn socket_ready(home: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(home.join(office::OFFICE_SOCKET_NAME)).is_ok()
}

fn send(home: &std::path::Path, kind: OfficeRequestKind) -> Result<Value, String> {
    let response =
        office::send_request(home, office::new_request(kind)).map_err(|e| e.to_string())?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(response.error.unwrap_or_else(|| "rejected".into()))
    }
}

struct Seed {
    member_id: MemberId,
    task_id: String,
    grant_id: String,
}

fn seed(home: &std::path::Path, goal: &str) -> Seed {
    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(home),
        office::office_migrations(),
    )
    .expect("seed store");
    let members = MemberRegistry::new(&store);
    let member = members.register("Samuel").expect("member");
    members
        .set_binding(&MemberBinding {
            member_id: member.member_id.clone(),
            role: "coordinator".into(),
            model_binding: "glm-5.3-flash".into(),
            tools: vec!["pi-cli".into()],
            updated_at: viva::foundation::ids::utc_now(),
        })
        .expect("binding");
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .create_task(goal, vec![], Some(member.member_id.clone()), None, None)
        .expect("task");
    let grant = AuthorityEngine::new(&store)
        .issue_root_grant(
            Some(member.member_id.clone()),
            Some(task.task_id.clone()),
            vec!["dispatch_delegated".into()],
            GrantMode::ActAutonomously,
            None,
        )
        .expect("grant");
    Seed {
        member_id: member.member_id,
        task_id: task.task_id.to_string(),
        grant_id: grant.grant_id.to_string(),
    }
}

fn spawn_host(home: std::path::PathBuf) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        OfficeHost::open(&home)
            .expect("host opens")
            .serve()
            .expect("serve");
    })
}

#[test]
fn harness_spec_rejects_shell_string_shapes() {
    // The generic launch entry (also used by V14 for other CLIs) keeps the
    // no-joined-shell rule.
    assert!(HarnessSpec::new("pi", vec![], std::env::temp_dir()).is_err());
    assert!(HarnessSpec::new("pi", vec!["pi".into()], std::path::Path::new("relative")).is_err());
}

#[test]
fn pi_launch_combination_injects_identity_and_task_context() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().to_path_buf();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let seed = seed(&home, "prepare the release");

    // The launch context is built from real office records.
    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(&home),
        office::office_migrations(),
    )
    .expect("store");
    let members = MemberRegistry::new(&store);
    let member = members.require(&seed.member_id).expect("member");
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .require_task(&viva::foundation::ids::TaskId::from_str(&seed.task_id).expect("task id"))
        .expect("task");
    drop(store);

    let harness = PiHarness::for_repo(&repo);
    let spec = harness
        .launch(
            &member,
            None,
            Some(&task),
            &repo,
            Some(&viva::foundation::ids::GrantId::from_str(&seed.grant_id).expect("grant id")),
        )
        .expect("launch");

    // Identity is the member id; the display name is launch-time
    // configuration. No hard-coded member name anywhere in the argv.
    let env = |key: &str| {
        spec.env
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(env(ENV_MEMBER_ID).as_deref(), Some(seed.member_id.as_str()));
    assert_eq!(env(ENV_MEMBER_NAME).as_deref(), Some("Samuel"));
    assert_eq!(env(ENV_TASK_ID).as_deref(), Some(seed.task_id.as_str()));
    assert_eq!(env(ENV_GRANT_ID).as_deref(), Some(seed.grant_id.as_str()));
    assert_eq!(spec.argv[0], "pi", "the pi binary is explicit");
    assert!(
        spec.argv
            .iter()
            .any(|a| a.ends_with("extensions/pi/viva-office.ts")),
        "the shipped extension is loaded explicitly: {:?}",
        spec.argv
    );

    // A plain-chat launch carries identity but no task and no grant:
    // chat never inherits office authority.
    let chat = harness
        .launch(&member, None, None, &repo, None)
        .expect("chat launch");
    assert!(chat.env.iter().any(|(k, _)| k == ENV_MEMBER_ID));
    assert!(!chat.env.iter().any(|(k, _)| k == ENV_TASK_ID));
    assert!(!chat.env.iter().any(|(k, _)| k == ENV_GRANT_ID));
}

#[test]
fn pi_availability_is_honest_when_missing() {
    let harness = PiHarness {
        pi_binary: "/nonexistent/viva-fake-pi".into(),
        extension_path: std::env::temp_dir().join("extensions/pi/viva-office.ts"),
    };
    match harness.availability() {
        Availability::Unavailable { reason } => {
            assert!(reason.contains("could not be started"), "got: {reason}");
        }
        Availability::Available => panic!("a missing binary must not be reported as available"),
    }
}

#[test]
fn handoff_records_member_report_and_never_completes_the_task() {
    let dir = TempDir::new().expect("dir");
    let home = dir.path().to_path_buf();
    let seed = seed(&home, "ship the loop");

    let host = spawn_host(home.clone());
    wait_until("socket", Duration::from_secs(10), || socket_ready(&home));

    let handoff = |summary: &str| {
        send(
            &home,
            OfficeRequestKind::Handoff {
                task_id: seed.task_id.clone(),
                member_id: seed.member_id.to_string(),
                summary: summary.into(),
            },
        )
    };

    // A member report is recorded as a member-reported fact.
    let first = handoff("draft done; open question: retry budget").expect("handoff recorded");
    assert_eq!(first["recorded"], true);

    // An identical repeat is a duplicate and changes nothing.
    let duplicate = handoff("draft done; open question: retry budget").expect("duplicate handled");
    assert_eq!(duplicate["recorded"], false);
    assert_eq!(duplicate["duplicate"], true);

    // The task is NOT completed by handoffs: status stays as created.
    let store = viva::foundation::store::Store::open(
        &viva::foundation::paths::database_path(&home),
        office::office_migrations(),
    )
    .expect("store");
    let tasks = TaskRegistry::new(&store);
    let task = tasks
        .require_task(&viva::foundation::ids::TaskId::from_str(&seed.task_id).expect("task id"))
        .expect("task");
    assert_eq!(
        task.status.as_str(),
        "open",
        "a member handoff must never complete the task"
    );
    // And the recorded results name their reporter.
    let results = tasks
        .results_for_task(&viva::foundation::ids::TaskId::from_str(&seed.task_id).expect("task id"))
        .expect("results");
    assert_eq!(results.len(), 1, "the duplicate was not recorded twice");
    assert_eq!(results[0].kind, "member_handoff");
    assert_eq!(
        results[0].payload["reporter_member_id"],
        Value::String(seed.member_id.to_string())
    );

    // An empty summary is rejected, not stored.
    assert!(handoff("   ").is_err());

    office::send_request(&home, office::new_request(OfficeRequestKind::Shutdown { close_policy: None })).ok();
    host.join().ok();
}

use std::str::FromStr as _;
