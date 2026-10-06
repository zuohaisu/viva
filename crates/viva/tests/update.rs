//! Update CLI boundaries; all mutation cases use offline release/npm
//! fixtures in update::tests. The real subprocesses here must not create a
//! Viva home or start a resident server just to parse/help/reject an update;
//! the one handover test below starts a server deliberately, because that is
//! the behavior under test.
use std::process::Command;
use tempfile::TempDir;

fn viva(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_viva"))
        .args(args)
        .env("VIVA_HOME", home)
        .env_remove("VIVA_NPM_PACKAGE_ROOT")
        .env_remove("VIVA_OFFICE_MEMBER_ID")
        .env_remove("VIVA_OFFICE_GRANT_ID")
        .output()
        .unwrap()
}

#[test]
fn update_help_and_invalid_arguments_never_initialize_home() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("unused-home");
    let help = viva(&home, &["help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("viva update [--check]"));
    let help = viva(&home, &["update", "--help"]);
    assert!(help.status.success());
    let usage = String::from_utf8_lossy(&help.stdout);
    assert!(usage.contains("VIVA_HOME data is untouched"), "{usage}");
    assert!(usage.contains("--no-restart"), "{usage}");
    for args in [
        vec!["update", "--force"],
        vec!["update", "--check", "--force"],
        vec!["update", "v0.1.0"],
    ] {
        let result = viva(&home, &args);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("USAGE: viva update"));
    }
    assert!(!home.exists());
}

#[test]
fn member_context_cannot_upgrade_the_installed_host() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("unused-home");
    let output = Command::new(env!("CARGO_BIN_EXE_viva"))
        .arg("update")
        .env("VIVA_HOME", &home)
        .env("VIVA_OFFICE_MEMBER_ID", "member-test")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("user-only installation operation"));
    assert!(!home.exists());
}

#[cfg(unix)]
#[test]
fn real_cli_uses_npm_channel_and_original_global_prefix_without_opening_home() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("npm prefix");
    let wrapper = prefix.join("lib/node_modules/@zuohaisu/viva");
    let platform = wrapper.join("node_modules/@zuohaisu/viva-darwin-arm64");
    std::fs::create_dir_all(wrapper.join("bin")).unwrap();
    std::fs::create_dir_all(platform.join("bin")).unwrap();
    std::fs::write(wrapper.join("package.json"), r#"{"name":"@zuohaisu/viva"}"#).unwrap();
    std::fs::write(
        platform.join("package.json"),
        r#"{"name":"@zuohaisu/viva-darwin-arm64"}"#,
    )
    .unwrap();
    let native = platform.join("bin/viva");
    std::fs::copy(env!("CARGO_BIN_EXE_viva"), &native).unwrap();
    let mut next = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    next.patch += 1;
    let next_binary = dir.path().join("next-binary");
    std::fs::write(&next_binary, format!("#!/bin/sh\necho 'viva {next}'\n")).unwrap();
    std::fs::set_permissions(&next_binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let log = dir.path().join("npm.log");
    let npm = tools.join("npm");
    std::fs::write(
        &npm,
        format!(
            "#!/bin/sh\nset -e\nprintf '%s\\n' \"$@\" >> '{}'\n\
         if [ \"$1\" = view ]; then echo '\"{next}\"'; exit 0; fi\n\
         cp '{}' '{}.next'\nmv '{}.next' '{}'\n\
         cp '{}' '{}/bin/viva.js'\n",
            log.display(),
            next_binary.display(),
            native.display(),
            native.display(),
            native.display(),
            next_binary.display(),
            wrapper.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = dir.path().join("unused-home");
    let path = std::env::join_paths(
        std::iter::once(tools).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(&native)
            .args(args)
            .env("PATH", &path)
            .env("VIVA_HOME", &home)
            .env("VIVA_NPM_PACKAGE_ROOT", &wrapper)
            .env_remove("VIVA_OFFICE_MEMBER_ID")
            .env_remove("VIVA_OFFICE_GRANT_ID")
            .output()
            .unwrap()
    };
    let check = run(&["update", "--check"]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(String::from_utf8_lossy(&check.stdout).contains("(npm)"));
    assert!(!std::fs::read_to_string(&log).unwrap().contains("install"));
    let install = run(&["update"]);
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    // No server was running: the restart step says so instead of spawning.
    assert!(String::from_utf8_lossy(&install.stdout).contains("No running resident server"));
    let actual_prefix = std::fs::canonicalize(&prefix).unwrap();
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains(&format!(
        "install\n--global\n--prefix\n{}\n@zuohaisu/viva@{next}\n",
        actual_prefix.display()
    )));
    assert!(String::from_utf8_lossy(&run(&["--version"]).stdout).contains(&format!("viva {next}")));
    assert!(!home.exists());
}

#[test]
fn build_output_is_not_overwritten_by_update() {
    let dir = TempDir::new().unwrap();
    let home = dir.path().join("unused-home");
    let output = viva(&home, &["update"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("refusing to overwrite a Cargo build output")
            || error.contains("native update supports macOS"),
        "{error}"
    );
    assert!(!home.exists());
}

/// The upgrade completes itself: `viva update` hands a RUNNING resident
/// server to the freshly installed entry point through the live handoff.
/// The whole path runs through the real binary with a stub npm — the
/// resumed host is the real binary exec'd by the installed "next" wrapper,
/// exactly what a production npm upgrade leaves on disk.
#[cfg(unix)]
#[test]
fn update_restarts_a_running_server_through_live_handover() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    use viva::office::{OFFICE_SOCKET_NAME, OfficeClient, OfficeHost, OfficeRequestKind};

    let dir = TempDir::new().unwrap();
    // The global npm layout the updater validates, with a stub npm that
    // "installs" the next version by copying the next entry over the
    // platform binary and the wrapper's viva.js.
    let prefix = dir.path().join("npm prefix");
    let wrapper = prefix.join("lib/node_modules/@zuohaisu/viva");
    let platform = wrapper.join("node_modules/@zuohaisu/viva-darwin-arm64");
    std::fs::create_dir_all(wrapper.join("bin")).unwrap();
    std::fs::create_dir_all(platform.join("bin")).unwrap();
    std::fs::write(wrapper.join("package.json"), r#"{"name":"@zuohaisu/viva"}"#).unwrap();
    std::fs::write(
        platform.join("package.json"),
        r#"{"name":"@zuohaisu/viva-darwin-arm64"}"#,
    )
    .unwrap();
    let native = platform.join("bin/viva");
    std::fs::copy(env!("CARGO_BIN_EXE_viva"), &native).unwrap();
    let mut next = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    next.patch += 1;
    // The installed "next" entry answers --version honestly and execs the
    // real binary for `server --resume` — the handover target must be a
    // genuine resident server, not a version-echoing stub.
    let next_entry = dir.path().join("next-entry");
    std::fs::write(
        &next_entry,
        format!(
            "#!/bin/sh\nif [ \"$1\" = server ]; then exec '{}' \"$@\"; fi\necho 'viva {next}'\n",
            env!("CARGO_BIN_EXE_viva")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&next_entry, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let log = dir.path().join("npm.log");
    let npm = tools.join("npm");
    std::fs::write(
        &npm,
        format!(
            "#!/bin/sh\nset -e\nprintf '%s\\n' \"$@\" >> '{}'\n\
         if [ \"$1\" = view ]; then echo '\"{next}\"'; exit 0; fi\n\
         cp '{}' '{}.next'\nmv '{}.next' '{}'\n\
         cp '{}' '{}/bin/viva.js'\n",
            log.display(),
            next_entry.display(),
            native.display(),
            native.display(),
            native.display(),
            next_entry.display(),
            wrapper.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();

    // A live host (the old generation) with one real terminal.
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let host = OfficeHost::open(&home).expect("host claims the slot");
    let old_pid = std::process::id();
    let server = host.serve_background();
    let socket = home.join(OFFICE_SOCKET_NAME);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut client = OfficeClient::connect(&home).expect("client");
    let created = client
        .call(OfficeRequestKind::TerminalCreate {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo update-e2e; sleep 60".into(),
            ],
            cwd: home.display().to_string(),
            env: vec![],
            cols: 80,
            rows: 24,
            purpose: "pre-update session".into(),
            worktree_id: None,
            owner: "user_shell".into(),
        })
        .expect("create");
    let terminal_id = created
        .get("terminal_id")
        .and_then(|v| v.as_str())
        .expect("terminal id")
        .to_string();
    drop(client);

    // The real `viva update` through the stub npm.
    let path = std::env::join_paths(
        std::iter::once(&tools)
            .map(|p| p.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = Command::new(&native)
        .arg("update")
        .env("PATH", &path)
        .env("VIVA_HOME", &home)
        .env("VIVA_NPM_PACKAGE_ROOT", &wrapper)
        .env_remove("VIVA_OFFICE_MEMBER_ID")
        .env_remove("VIVA_OFFICE_GRANT_ID")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("npm installed Viva"), "{stdout}");
    assert!(stdout.contains("Resident server restarted"), "{stdout}");

    // A NEW process serves, and the live terminal survived the upgrade.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        if let Ok(client) = OfficeClient::connect(&home) {
            break client;
        }
        assert!(Instant::now() < deadline, "the resumed host never answered");
        std::thread::sleep(Duration::from_millis(100));
    };
    let status = client.call(OfficeRequestKind::Ping).expect("ping");
    let new_pid = status.get("pid").and_then(|p| p.as_u64()).expect("pid");
    assert_ne!(new_pid, u64::from(old_pid), "a new process must serve");
    let list = client.call(OfficeRequestKind::TerminalList).expect("list");
    assert!(
        format!("{list}").contains(&terminal_id),
        "the terminal survived the update restart: {list}"
    );

    // Clean teardown of the resumed (detached) server.
    client
        .call(OfficeRequestKind::Shutdown { close_policy: None })
        .expect("shutdown");
    drop(client);
    let deadline = Instant::now() + Duration::from_secs(15);
    while socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    server.join().expect("old host joins");
}

/// A socket nobody answers is left alone with an honest note: the update
/// never claims it (that is the next start's recorded recovery), and the
/// install still succeeds.
#[cfg(unix)]
#[test]
fn update_with_a_dead_socket_installs_and_does_not_restart() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("prefix");
    let wrapper = prefix.join("lib/node_modules/@zuohaisu/viva");
    let platform = wrapper.join("node_modules/@zuohaisu/viva-darwin-arm64");
    std::fs::create_dir_all(wrapper.join("bin")).unwrap();
    std::fs::create_dir_all(platform.join("bin")).unwrap();
    std::fs::write(wrapper.join("package.json"), r#"{"name":"@zuohaisu/viva"}"#).unwrap();
    std::fs::write(
        platform.join("package.json"),
        r#"{"name":"@zuohaisu/viva-darwin-arm64"}"#,
    )
    .unwrap();
    let native = platform.join("bin/viva");
    std::fs::copy(env!("CARGO_BIN_EXE_viva"), &native).unwrap();
    let mut next = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    next.patch += 1;
    // The probe target the updater verifies after the install step.
    let viva_js = wrapper.join("bin/viva.js");
    std::fs::write(&viva_js, format!("#!/bin/sh\necho 'viva {next}'\n")).unwrap();
    std::fs::set_permissions(&viva_js, std::fs::Permissions::from_mode(0o755)).unwrap();
    let tools = dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let npm = tools.join("npm");
    std::fs::write(
        &npm,
        format!(
            "#!/bin/sh\nif [ \"$1\" = view ]; then echo '\"{next}\"'; exit 0; fi\n\
             echo 'viva {next}'\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The wedge: a socket path with no listener behind it.
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let socket = home.join("office.sock");
    std::fs::write(&socket, b"").unwrap();

    let path = std::env::join_paths(
        std::iter::once(tools).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = Command::new(&native)
        .arg("update")
        .env("PATH", &path)
        .env("VIVA_HOME", &home)
        .env("VIVA_NPM_PACKAGE_ROOT", &wrapper)
        .env_remove("VIVA_OFFICE_MEMBER_ID")
        .env_remove("VIVA_OFFICE_GRANT_ID")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("npm installed Viva"), "{stdout}");
    assert!(!stdout.contains("Resident server restarted"), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("answered no healthy host"), "{stderr}");
    // The wedge was left for the next start's recorded claim.
    assert!(socket.exists());
}
