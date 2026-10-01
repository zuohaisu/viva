//! Update CLI boundaries; all mutation cases use offline release/npm
//! fixtures in update::tests. These real subprocesses must not create a
//! Viva home or start a resident server just to parse/help/reject an update.
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
    assert!(String::from_utf8_lossy(&help.stdout).contains("VIVA_HOME are untouched"));
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
