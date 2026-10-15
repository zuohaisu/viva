//! TFHS-74: one workbench aggregates this computer and saved SSH machines.
//! The fixture is a fake `ssh` plus two extra homes on this machine.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use viva::server::{Client, RequestKind, SOCKET_NAME};

fn viva_bin() -> &'static str {
    env!("CARGO_BIN_EXE_viva")
}

fn wait_socket(home: &Path) {
    let socket = home.join(SOCKET_NAME);
    let deadline = Instant::now() + Duration::from_secs(8);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "missing socket {}", socket.display());
}

struct Reap(Vec<std::process::Child>);

impl Drop for Reap {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn serve(home: &Path, ssh: Option<(&Path, &str)>) -> std::process::Child {
    let mut cmd = Command::new(viva_bin());
    cmd.arg("server")
        .env("VIVA_HOME", home)
        .env_remove("VIVA_MACHINE_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some((program, map)) = ssh {
        cmd.env("VIVA_SSH", program)
            .env("VIVA_FAKE_SSH_MAP", map)
            .env("VIVA_BRIDGE_BIN", viva_bin());
    } else {
        cmd.env_remove("VIVA_SSH")
            .env_remove("VIVA_FAKE_SSH_MAP")
            .env_remove("VIVA_BRIDGE_BIN");
    }
    cmd.spawn().expect("spawn viva server")
}

fn viva(home: &Path, ssh: &Path, map: &str, args: &[&str]) -> std::process::Output {
    Command::new(viva_bin())
        .args(args)
        .env("VIVA_HOME", home)
        .env("VIVA_SSH", ssh)
        .env("VIVA_FAKE_SSH_MAP", map)
        .env("VIVA_BRIDGE_BIN", viva_bin())
        .output()
        .expect("spawn viva")
}

fn write_fake_ssh(path: &Path) {
    let script = r#"#!/bin/sh
target=""
skip=0
for arg in "$@"; do
  if [ "$skip" = 1 ]; then
    skip=0
    continue
  fi
  case "$arg" in
    -o|-p|-i|-l|-F|-W|-J)
      skip=1
      ;;
    -*)
      ;;
    *)
      target=$arg
      break
      ;;
  esac
done
if [ "$target" = "missing-viva" ]; then
  echo "zsh: command not found: viva" >&2
  exit 127
fi
sock=""
rest=${VIVA_FAKE_SSH_MAP-}
while [ -n "$rest" ]; do
  pair=${rest%%,*}
  if [ "$rest" = "$pair" ]; then
    rest=""
  else
    rest=${rest#*,}
  fi
  key=${pair%%=*}
  val=${pair#*=}
  if [ "$key" = "$target" ]; then
    sock=$val
  fi
done
if [ -z "$sock" ] || [ ! -S "$sock" ]; then
  echo "no viva server for $target" >&2
  exit 1
fi
if [ -z "${VIVA_BRIDGE_BIN:-}" ]; then
  echo "fake ssh is missing VIVA_BRIDGE_BIN" >&2
  exit 127
fi
# The catalog session stays `default`. This fixture maps the SSH target
# onto another home on this computer and runs the real bridge there.
home=$(dirname "$sock")
exec "$VIVA_BRIDGE_BIN" machine bridge --session "$home"
"#;
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(script.as_bytes()).unwrap();
    drop(file);
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn shutdown(home: &Path) {
    if let Ok(mut client) = Client::connect(home) {
        let _ = client.call(RequestKind::Shutdown { close_policy: None });
    }
}

#[test]
fn fake_ssh_aggregates_two_homes_without_storing_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("home-local");
    let lab = dir.path().join("home-lab");
    let other = dir.path().join("home-other");
    for home in [&local, &lab, &other] {
        std::fs::create_dir_all(home).unwrap();
    }
    let script = dir.path().join("fake-ssh");
    write_fake_ssh(&script);
    let map = format!(
        "lab-host={},other-host={}",
        lab.join(SOCKET_NAME).display(),
        other.join(SOCKET_NAME).display()
    );

    let lab_server = serve(&lab, None);
    let other_server = serve(&other, None);
    let local_server = serve(&local, Some((&script, &map)));
    let lab_pid = lab_server.id();
    let other_pid = other_server.id();
    let local_pid = local_server.id();
    let reap = Reap(vec![lab_server, other_server, local_server]);
    wait_socket(&lab);
    wait_socket(&other);
    wait_socket(&local);

    Client::connect(&lab)
        .unwrap()
        .call(RequestKind::Workspace {
            action: "new".into(),
            value: Some("labws".into()),
            project_id: None,
        })
        .expect("lab workspace");
    Client::connect(&other)
        .unwrap()
        .call(RequestKind::Workspace {
            action: "new".into(),
            value: Some("otherws".into()),
            project_id: None,
        })
        .expect("other workspace");

    let added = viva(
        &local,
        &script,
        &map,
        &[
            "machine", "add", "--target", "lab-host", "--label", "Lab", "--id", "lab",
        ],
    );
    assert!(
        added.status.success(),
        "add lab: {}",
        String::from_utf8_lossy(&added.stderr)
    );
    let added = viva(
        &local,
        &script,
        &map,
        &[
            "machine",
            "add",
            "--target",
            "other-host",
            "--label",
            "Other",
            "--id",
            "other",
            "--session",
            "default",
        ],
    );
    assert!(
        added.status.success(),
        "add other: {}",
        String::from_utf8_lossy(&added.stderr)
    );

    let mut local_client = Client::connect(&local).unwrap();
    local_client
        .call(RequestKind::MachineRefresh)
        .expect("refresh both machines");
    let view = local_client.call(RequestKind::WorkbenchView).expect("view");
    let machines = view["machines"].as_array().expect("machines");
    assert_eq!(machines[0]["id"], "local");
    assert_eq!(machines[0]["connected"], true);
    let lab_view = machines.iter().find(|m| m["id"] == "lab").unwrap();
    let other_view = machines.iter().find(|m| m["id"] == "other").unwrap();
    assert_eq!(lab_view["connected"], true, "{lab_view}");
    assert_eq!(other_view["connected"], true, "{other_view}");
    assert!(
        lab_view["workspaces"].to_string().contains("labws"),
        "{lab_view}"
    );
    assert!(
        other_view["workspaces"].to_string().contains("otherws"),
        "{other_view}"
    );
    assert!(lab_view["agents"].is_array());
    assert!(other_view["agents"].is_array());

    shutdown(&lab);
    let lab_socket = lab.join(SOCKET_NAME);
    let deadline = Instant::now() + Duration::from_secs(5);
    while lab_socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    local_client
        .call(RequestKind::MachineRefresh)
        .expect("refresh after lab drops");
    let view = local_client.call(RequestKind::WorkbenchView).unwrap();
    let machines = view["machines"].as_array().unwrap();
    let lab_view = machines.iter().find(|m| m["id"] == "lab").unwrap();
    let other_view = machines.iter().find(|m| m["id"] == "other").unwrap();
    assert_eq!(lab_view["connected"], false, "{lab_view}");
    assert!(
        lab_view["workspaces"].to_string().contains("labws"),
        "{lab_view}"
    );
    assert_eq!(other_view["connected"], true, "{other_view}");
    assert_eq!(machines[0]["id"], "local");
    assert_eq!(machines[0]["connected"], true);

    let forwarded = local_client
        .call(RequestKind::MachineForward {
            id: "other".into(),
            kind: Box::new(RequestKind::Ping),
        })
        .expect("other still answers");
    assert_eq!(forwarded["pid"].as_u64().unwrap(), other_pid as u64);
    let refused = local_client
        .call(RequestKind::MachineForward {
            id: "lab".into(),
            kind: Box::new(RequestKind::Ping),
        })
        .unwrap_err()
        .to_string();
    assert!(refused.contains("input is refused"), "{refused}");
    let local_ping = local_client.call(RequestKind::Ping).unwrap();
    assert_eq!(local_ping["pid"].as_u64().unwrap(), local_pid as u64);
    assert_ne!(local_pid, other_pid);
    assert_ne!(local_pid, lab_pid);
    drop(local_client);

    let remote_status = viva(&local, &script, &map, &["--machine", "other", "status"]);
    assert!(
        remote_status.status.success(),
        "status other: {}",
        String::from_utf8_lossy(&remote_status.stderr)
    );
    let remote: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&remote_status.stdout)).unwrap();
    assert_eq!(remote["pid"].as_u64().unwrap(), other_pid as u64);
    assert_eq!(remote["home"].as_str().unwrap(), other.to_str().unwrap());

    let local_status = viva(&local, &script, &map, &["status"]);
    assert!(
        local_status.status.success(),
        "local status: {}",
        String::from_utf8_lossy(&local_status.stderr)
    );
    let local_body: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&local_status.stdout)).unwrap();
    assert_eq!(local_body["pid"].as_u64().unwrap(), local_pid as u64);
    assert_eq!(
        local_body["home"].as_str().unwrap(),
        local.to_str().unwrap()
    );
    assert!(local.join(SOCKET_NAME).exists());

    let missing = viva(
        &local,
        &script,
        &map,
        &[
            "machine",
            "add",
            "--target",
            "missing-viva",
            "--label",
            "Missing",
            "--id",
            "missing",
        ],
    );
    assert!(
        missing.status.success(),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    let failed = viva(&local, &script, &map, &["--machine", "missing", "status"]);
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("missing the viva command"), "{stderr}");

    let catalog = std::fs::read_to_string(local.join("machines.json")).unwrap();
    for forbidden in [
        "password",
        "PRIVATE",
        "ticket",
        "ssh-ed25519",
        "ssh-rsa",
        "BEGIN ",
        "IdentityFile",
    ] {
        assert!(!catalog.contains(forbidden), "{catalog}");
    }
    let value: serde_json::Value = serde_json::from_str(&catalog).unwrap();
    let mut keys = Vec::new();
    collect_keys(&value, &mut keys);
    for key in keys {
        assert!(
            [
                "version",
                "machines",
                "id",
                "label",
                "ssh_target",
                "remote_session",
                "enabled"
            ]
            .contains(&key.as_str()),
            "unexpected key {key} in {catalog}"
        );
    }

    let renamed = viva(
        &local,
        &script,
        &map,
        &["machine", "rename", "other", "Other Mac"],
    );
    assert!(
        renamed.status.success(),
        "{}",
        String::from_utf8_lossy(&renamed.stderr)
    );
    let disabled = viva(&local, &script, &map, &["machine", "disable", "missing"]);
    assert!(
        disabled.status.success(),
        "{}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    let enabled = viva(&local, &script, &map, &["machine", "enable", "missing"]);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    let removed = viva(&local, &script, &map, &["machine", "remove", "missing"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let listed = viva(&local, &script, &map, &["machine", "list"]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let list_text = String::from_utf8_lossy(&listed.stdout);
    assert!(list_text.contains("Other Mac"), "{list_text}");
    assert!(!list_text.contains("missing-viva"), "{list_text}");

    shutdown(&other);
    shutdown(&local);
    drop(reap);
}

fn collect_keys(value: &serde_json::Value, keys: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                keys.push(key.clone());
                collect_keys(child, keys);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_keys(child, keys);
            }
        }
        _ => {}
    }
}
