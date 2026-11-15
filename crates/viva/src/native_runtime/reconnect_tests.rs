//! Deterministic snapshot/file rollover at the real CLI authentication seam.
use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
};

#[test]
fn cli_recovers_when_capability_rotates_after_snapshot() {
    let home = tempfile::Builder::new().prefix("rc").tempdir().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    owner_anchor::prepare_home(home.path()).unwrap();
    let (_, anchor_a) = owner_anchor::generate();
    let (host_b, anchor_b) = owner_anchor::generate();
    owner_anchor::store_runtime(home.path(), &anchor_a).unwrap();
    let host_a = owner_anchor::host_id(&anchor_a).unwrap().to_owned();
    let socket = home.path().join("c.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let root = home.path().to_owned();
    let server = std::thread::spawn(move || {
        let mut snapshots = 0;
        let mut methods = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        for _ in 0..3 {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "fixture request deadline"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            };
            // macOS inherits O_NONBLOCK from the listener. The accept deadline
            // needs nonblocking polling, but read_line must wait for request bytes.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let method = request["method"].as_str().unwrap();
            methods.push(method.to_owned());
            let response = match method {
                "viva.snapshot" => {
                    snapshots += 1;
                    // The snapshot was taken by A, but B has atomically
                    // published its complete capability before the client reads.
                    owner_anchor::store_runtime(&root, &anchor_b).unwrap();
                    serde_json::json!({
                        "id": request["id"],
                        "result": {"snapshot": {"runtime_host_id":
                            if snapshots == 1 { &host_a } else { &host_b }}},
                    })
                }
                "viva.client.authenticate" => {
                    let valid = request["params"]["credential"].as_str() == Some(&anchor_b);
                    if valid {
                        serde_json::json!({"id": request["id"], "result": {"type": "ok"}})
                    } else {
                        serde_json::json!({"id": request["id"], "error": {
                            "code": "owner_anchor_missing", "message": "no current proof"}})
                    }
                }
                _ => panic!("unexpected method"),
            };
            writeln!(stream, "{response}").unwrap();
            if method == "viva.client.authenticate" {
                return methods;
            }
        }
        panic!("fixture expected authentication within three requests");
    });
    let result = authenticate_owner(&socket, home.path());
    let methods = server.join().unwrap();
    assert!(
        result.is_ok(),
        "a confirmed live incarnation change must recover: {result:?}"
    );
    assert_eq!(
        methods,
        ["viva.snapshot", "viva.snapshot", "viva.client.authenticate"]
    );
}
