//! Task-owned native-host fixture for the #191 owner-anchor protocol.
//!
//! The owner capability is no longer a file: the test that spawns the server
//! is the process that hands it the anchor over an inherited pipe, exactly as
//! the embedded TUI does. Keychain access is disabled so CI and tests never
//! prompt a human.
#![allow(dead_code)]
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::fd::FromRawFd,
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const CONFIG: &str = "[terminal]\ndefault_shell = '/bin/sh'\nshell_mode = 'non_login'\n\
[update]\nversion_check = false\nmanifest_check = false\n\
[experimental]\npane_history = false\n";

pub struct OwnerHost {
    anchor: String,
    child: Child,
}

impl OwnerHost {
    pub fn anchor(&self) -> &str {
        &self.anchor
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for OwnerHost {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static NEXT_ANCHOR: AtomicU64 = AtomicU64::new(1);

fn fresh_anchor(tag: &str) -> String {
    let nonce = NEXT_ANCHOR.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("native-anchor:native-fixture-{tag}-{pid}:secret-{pid}-{nonce}")
}

fn write_config(home: &Path) {
    let runtime = home.join("runtime/workbench");
    std::fs::create_dir_all(&runtime).unwrap();
    if !runtime.join("config.toml").exists() {
        std::fs::write(runtime.join("config.toml"), CONFIG).unwrap();
    }
}

/// Spawn `viva runtime server` with an anchor only this test process and the
/// server know, delivered over an inherited descriptor.
pub fn spawn(home: &Path, tag: &str) -> OwnerHost {
    spawn_with(home, tag, |_| {})
}

pub fn spawn_with(home: &Path, tag: &str, configure: impl FnOnce(&mut Command)) -> OwnerHost {
    write_config(home);
    let anchor = fresh_anchor(tag);
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (read_fd, write_fd) = (fds[0], fds[1]);
    {
        let mut pipe = unsafe { std::fs::File::from_raw_fd(write_fd) };
        pipe.write_all(format!("{anchor}\n").as_bytes()).unwrap();
    }
    assert!(unsafe { libc::fcntl(read_fd, libc::F_SETFD, 0) } >= 0);
    let log = std::fs::File::create(home.join("native.log")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_viva"));
    command
        .args(["runtime", "server"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", home)
        .env("SHELL", "/bin/sh")
        .env("TERM", "xterm-256color")
        .env("VIVA_HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg-config"))
        .env("XDG_STATE_HOME", home.join("xdg-state"))
        .env("VIVA_OWNER_ANCHOR_FD", read_fd.to_string())
        .env("VIVA_OWNER_KEYCHAIN", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    configure(&mut command);
    let child = command.spawn().unwrap();
    // Only the server keeps the descriptor.
    unsafe {
        libc::close(read_fd);
    }
    OwnerHost { anchor, child }
}

pub fn log(home: &Path) -> String {
    std::fs::read_to_string(home.join("native.log")).unwrap_or_default()
}

pub fn call(home: &Path, method: &str, params: Value) -> Value {
    let mut socket = UnixStream::connect(home.join("runtime/workbench/herdr.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    writeln!(
        socket,
        "{}",
        json!({"id":"fixture-secret-do-not-persist", "method":method, "params":params,
            "transport_peer":{"pid":1,"uid":0}, "peer_pid":1, "peer_uid":0,
            "claimed_role":"member_execution:forged", "member_id":"forged", "grant_id":"forged"})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn authenticate_anchor(home: &Path, anchor: &str) -> Value {
    let peer = herdr::embedding::TransportPeer {
        pid: Some(std::process::id()),
        uid: Some(unsafe { libc::geteuid() }),
    };
    let lifetime = herdr::embedding::peer_lifetime_marker(peer).unwrap();
    call(
        home,
        "viva.client.authenticate",
        json!({"credential": anchor, "process_lifetime": lifetime}),
    )
}

/// Authenticate the current process as the owner. Retries only the readiness
/// window while the host is still claiming its endpoints.
pub fn authenticate(home: &Path, anchor: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response = authenticate_anchor(home, anchor);
        if response["result"]["type"] == "ok" {
            return;
        }
        let code = response["error"]["code"].as_str().unwrap_or("");
        assert!(
            matches!(code, "client_not_ready" | "untrusted_client"),
            "{response}"
        );
        assert!(
            Instant::now() < deadline,
            "owner authentication never became ready: {response}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

/// Wait until the socket exists. The owner phase may still be claiming its
/// endpoints; `authenticate` retries that short window.
pub fn wait_ready(home: &Path, _host: &OwnerHost) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let socket = home.join("runtime/workbench/herdr.sock");
    while !socket.exists() {
        assert!(
            Instant::now() < deadline,
            "native startup deadline: {}",
            log(home)
        );
        thread::sleep(Duration::from_millis(25));
    }
}

pub fn wait_owner_ready(home: &Path, host: &OwnerHost) {
    wait_ready(home, host);
    authenticate(home, host.anchor());
}
