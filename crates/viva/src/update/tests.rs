use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    dir: TempDir,
    target: PathBuf,
    original: Vec<u8>,
    download: FakeDownload,
}

struct FakeDownload {
    files: HashMap<String, Vec<u8>>,
    calls: RefCell<Vec<String>>,
}
impl Download for FakeDownload {
    fn fetch(&self, url: &str, destination: &Path, max_bytes: u64) -> OfficeResult<()> {
        self.calls.borrow_mut().push(url.into());
        let bytes = self
            .files
            .get(url)
            .ok_or_else(|| invalid("fixture download failed"))?;
        if bytes.len() as u64 > max_bytes {
            return Err(invalid("too large"));
        }
        fs::write(destination, bytes)?;
        Ok(())
    }
}

fn script_version(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho 'viva {version}'\n").into_bytes()
}

fn executable(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn archive(entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
    let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    for (path, bytes, kind) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(bytes.len() as u64);
        header.set_entry_type(*kind);
        if kind.is_symlink() {
            header.set_link_name("/outside/viva").unwrap();
        }
        header.set_cksum();
        tar.append_data(&mut header, path, *bytes).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

const ARCHIVE_NAME: &str = "viva-macos-arm64.tar.gz";
fn url(name: &str) -> String {
    format!("https://github.com/zuohaisu/viva/releases/download/v0.3.0/{name}")
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("viva");
        let original = script_version("0.2.0");
        executable(&target, &original);
        // Unrelated assets next to the installation must remain untouched.
        fs::write(dir.path().join("member-history"), "persistent history").unwrap();
        let mut fixture = Self {
            dir,
            target,
            original,
            download: FakeDownload {
                files: HashMap::new(),
                calls: RefCell::new(Vec::new()),
            },
        };
        fixture.set_archive(archive(&[(
            "viva-macos-arm64/viva",
            &script_version("0.3.0"),
            tar::EntryType::Regular,
        )]));
        fixture
    }

    fn set_archive(&mut self, bytes: Vec<u8>) {
        let checksum = format!("{:x}  {ARCHIVE_NAME}\n", Sha256::digest(&bytes));
        let metadata = serde_json::json!({
            "tag_name": "v0.3.0", "draft": false, "prerelease": false,
            "assets": [
                {"name": ARCHIVE_NAME, "size": bytes.len(), "browser_download_url": url(ARCHIVE_NAME)},
                {"name": format!("{ARCHIVE_NAME}.sha256"), "size": checksum.len(), "browser_download_url": url(&format!("{ARCHIVE_NAME}.sha256"))}
            ]
        });
        self.download.files.insert(
            LATEST_RELEASE.into(),
            serde_json::to_vec(&metadata).unwrap(),
        );
        self.download.files.insert(url(ARCHIVE_NAME), bytes);
        self.download.files.insert(
            url(&format!("{ARCHIVE_NAME}.sha256")),
            checksum.into_bytes(),
        );
    }

    fn run(&self, check: bool) -> OfficeResult<bool> {
        update_native(
            &version("0.2.0").unwrap(),
            &self.target,
            "macos-arm64",
            check,
            &self.download,
        )
    }

    fn unchanged(&self) {
        assert_eq!(fs::read(&self.target).unwrap(), self.original);
        assert_eq!(
            fs::read_to_string(self.dir.path().join("member-history")).unwrap(),
            "persistent history"
        );
        for entry in fs::read_dir(self.dir.path()).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(
                matches!(
                    name.to_str(),
                    Some("viva" | "member-history" | ".viva-update.lock")
                ),
                "staging left over: {name:?}"
            );
        }
    }
}

#[test]
fn strict_arguments_and_semver_precedence() {
    assert_eq!(parse_args(&[]).unwrap(), Mode::Install);
    assert_eq!(parse_args(&["--check".into()]).unwrap(), Mode::Check);
    assert_eq!(parse_args(&["--help".into()]).unwrap(), Mode::Help);
    for args in [
        vec!["--force".into()],
        vec!["--check".into(), "--check".into()],
    ] {
        assert!(parse_args(&args).is_err());
    }
    for (current, latest, expected) in [
        ("0.2.0", "v0.10.0", true),
        ("0.2.0", "0.2.0", false),
        ("1.0.0", "0.9.0", false),
        ("0.3.0-rc.1", "0.3.0", true),
        ("0.3.0+source", "0.3.0+release", false),
        ("0.4.0-dev", "0.3.0", false),
    ] {
        assert_eq!(
            newer(&version(current).unwrap(), &version(latest).unwrap()),
            expected
        );
    }
    assert!(version("latest").is_err());
}

#[test]
fn platform_selection_and_build_output_safety() {
    assert_eq!(platform("macos", "aarch64").unwrap(), "macos-arm64");
    assert_eq!(platform("macos", "x86_64").unwrap(), "macos-intel");
    assert!(platform("linux", "x86_64").is_err());
    assert!(platform("macos", "riscv64").is_err());
    assert!(is_build_output(Path::new("/repo/target/debug/viva")));
    assert!(is_build_output(Path::new(
        "/repo/target/aarch64-apple-darwin/release/viva"
    )));
    assert!(!is_build_output(Path::new("/Users/u/.cargo/bin/viva")));
}

#[test]
fn native_upgrade_verifies_version_and_preserves_unrelated_assets() {
    let fixture = Fixture::new();
    assert!(fixture.run(false).unwrap());
    assert_eq!(fs::read(&fixture.target).unwrap(), script_version("0.3.0"));
    assert_eq!(
        fs::metadata(&fixture.target).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_to_string(fixture.dir.path().join("member-history")).unwrap(),
        "persistent history"
    );
    assert_eq!(fixture.download.calls.borrow().len(), 3);
    assert_eq!(fs::read_dir(fixture.dir.path()).unwrap().count(), 3); // binary, history, persistent lock
}

#[test]
fn check_and_already_current_download_only_metadata_and_write_nothing() {
    let fixture = Fixture::new();
    assert!(!fixture.run(true).unwrap());
    fixture.unchanged();
    assert!(!fixture.dir.path().join(".viva-update.lock").exists());
    assert_eq!(*fixture.download.calls.borrow(), vec![LATEST_RELEASE]);
    for current in ["0.3.0", "0.4.0", "0.3.0+build"] {
        assert!(
            !update_native(
                &version(current).unwrap(),
                &fixture.target,
                "macos-arm64",
                false,
                &fixture.download
            )
            .unwrap()
        );
        fixture.unchanged();
    }
    assert!(
        fixture
            .download
            .calls
            .borrow()
            .iter()
            .all(|url| url == LATEST_RELEASE)
    );
}

#[test]
fn network_failure_and_corrupt_checksum_leave_old_binary_intact() {
    let mut fixture = Fixture::new();
    let bytes = fixture.download.files.remove(&url(ARCHIVE_NAME)).unwrap();
    assert!(
        fixture
            .run(false)
            .unwrap_err()
            .to_string()
            .contains("download failed")
    );
    fixture.unchanged();
    fixture.download.files.insert(url(ARCHIVE_NAME), bytes);
    fixture.download.files.insert(
        url(&format!("{ARCHIVE_NAME}.sha256")),
        format!("{}  {ARCHIVE_NAME}\n", "0".repeat(64)).into_bytes(),
    );
    assert!(
        fixture
            .run(false)
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch")
    );
    fixture.unchanged();
}

#[test]
fn malformed_checksum_wrong_filename_and_oversized_inputs_are_rejected() {
    let mut fixture = Fixture::new();
    for text in [
        "bad hash",
        "",
        &format!("{}  other.tar.gz", "0".repeat(64)),
        &format!("{}  {ARCHIVE_NAME}\nextra", "0".repeat(64)),
    ] {
        fixture.download.files.insert(
            url(&format!("{ARCHIVE_NAME}.sha256")),
            text.as_bytes().to_vec(),
        );
        assert!(fixture.run(false).is_err());
        fixture.unchanged();
    }
    let file = fixture.dir.path().join("input");
    fs::write(&file, "oversized").unwrap();
    assert!(read_bounded(&file, 3).is_err());
}

#[test]
fn missing_binary_link_duplicate_and_version_mismatch_never_replace_target() {
    let mut fixture = Fixture::new();
    for entries in [
        vec![(
            "viva-macos-intel/viva",
            script_version("0.3.0"),
            tar::EntryType::Regular,
        )],
        vec![("viva-macos-arm64/viva", Vec::new(), tar::EntryType::Symlink)],
        vec![
            (
                "viva-macos-arm64/viva",
                script_version("0.3.0"),
                tar::EntryType::Regular
            );
            2
        ],
        vec![(
            "viva-macos-arm64/viva",
            script_version("0.2.0"),
            tar::EntryType::Regular,
        )],
        vec![("viva-macos-arm64/viva", Vec::new(), tar::EntryType::Regular)],
    ] {
        let entries: Vec<_> = entries
            .iter()
            .map(|(p, b, k)| (*p, b.as_slice(), *k))
            .collect();
        fixture.set_archive(archive(&entries));
        assert!(fixture.run(false).is_err());
        fixture.unchanged();
    }
}

#[test]
fn unrelated_archive_entries_are_never_unpacked() {
    let mut fixture = Fixture::new();
    fixture.set_archive(archive(&[
        (
            "viva-macos-arm64/extensions/pi/viva-office.ts",
            b"not installed",
            tar::EntryType::Regular,
        ),
        ("member-history", b"overwritten", tar::EntryType::Regular),
        (
            "viva-macos-arm64/viva",
            &script_version("0.3.0"),
            tar::EntryType::Regular,
        ),
    ]));
    assert!(fixture.run(false).unwrap());
    assert!(!fixture.dir.path().join("viva-macos-arm64").exists());
    assert_eq!(
        fs::read_to_string(fixture.dir.path().join("member-history")).unwrap(),
        "persistent history"
    );
}

#[test]
fn release_metadata_rejects_unstable_missing_or_foreign_assets() {
    let fixture = Fixture::new();
    let original: serde_json::Value =
        serde_json::from_slice(fixture.download.files.get(LATEST_RELEASE).unwrap()).unwrap();
    for (field, value) in [
        ("draft", serde_json::json!(true)),
        ("prerelease", serde_json::json!(true)),
        ("tag_name", serde_json::json!("v0.3.0-rc.1")),
        ("tag_name", serde_json::json!("not-semver")),
    ] {
        let mut metadata = original.clone();
        metadata[field] = value;
        assert!(
            serde_json::from_value::<Release>(metadata)
                .unwrap()
                .stable_version()
                .is_err()
        );
    }
    let mut metadata = original.clone();
    metadata["assets"] = serde_json::json!([]);
    assert!(
        serde_json::from_value::<Release>(metadata)
            .unwrap()
            .asset_url(ARCHIVE_NAME, MAX_ARCHIVE)
            .is_err()
    );
    for bad in [
        serde_json::json!(
            "http://github.com/zuohaisu/viva/releases/download/v0.3.0/viva-macos-arm64.tar.gz"
        ),
        serde_json::json!("https://attacker.example/payload"),
    ] {
        let mut metadata = original.clone();
        metadata["assets"][0]["browser_download_url"] = bad;
        assert!(
            serde_json::from_value::<Release>(metadata)
                .unwrap()
                .asset_url(ARCHIVE_NAME, MAX_ARCHIVE)
                .is_err()
        );
    }
    for size in [0, MAX_ARCHIVE + 1] {
        let mut metadata = original.clone();
        metadata["assets"][0]["size"] = serde_json::json!(size);
        assert!(
            serde_json::from_value::<Release>(metadata)
                .unwrap()
                .asset_url(ARCHIVE_NAME, MAX_ARCHIVE)
                .is_err()
        );
    }
    let mut metadata = original;
    let duplicate = metadata["assets"][0].clone();
    metadata["assets"].as_array_mut().unwrap().push(duplicate);
    assert!(
        serde_json::from_value::<Release>(metadata)
            .unwrap()
            .asset_url(ARCHIVE_NAME, MAX_ARCHIVE)
            .is_err()
    );
}

#[test]
fn concurrent_updates_are_locked_and_changed_installation_is_not_downgraded() {
    let fixture = Fixture::new();
    let first = lock_installation(&fixture.target).unwrap();
    assert!(lock_installation(&fixture.target).is_err());
    assert!(fixture.run(false).is_err());
    fixture.unchanged();
    drop(first);
    executable(&fixture.target, &script_version("0.4.0"));
    assert!(
        fixture
            .run(false)
            .unwrap_err()
            .to_string()
            .contains("changed since")
    );
    assert_eq!(fs::read(&fixture.target).unwrap(), script_version("0.4.0"));
    assert!(
        fixture
            .download
            .calls
            .borrow()
            .iter()
            .all(|url| url == LATEST_RELEASE)
    );
}

fn npm_layout(base: &Path, nested: bool) -> (PathBuf, PathBuf) {
    let wrapper = base.join("node_modules/@zuohaisu/viva");
    fs::create_dir_all(&wrapper).unwrap();
    fs::write(wrapper.join("package.json"), r#"{"name":"@zuohaisu/viva"}"#).unwrap();
    let native_root = if nested {
        wrapper.join("node_modules/@zuohaisu/viva-darwin-arm64")
    } else {
        base.join("node_modules/@zuohaisu/viva-darwin-arm64")
    };
    let native = native_root.join("bin/viva");
    executable(&native, &script_version("0.2.0"));
    fs::write(
        native_root.join("package.json"),
        r#"{"name":"@zuohaisu/viva-darwin-arm64"}"#,
    )
    .unwrap();
    (native, wrapper)
}

#[test]
fn npm_installation_prefix_detection_handles_nested_hoisted_and_local_packages() {
    let dir = TempDir::new().unwrap();
    for nested in [true, false] {
        let prefix = dir.path().join(if nested {
            "global nested"
        } else {
            "global hoisted"
        });
        let (native, wrapper) = npm_layout(&prefix.join("lib"), nested);
        assert_eq!(
            npm_prefix(&native, &wrapper).unwrap(),
            Some(fs::canonicalize(prefix).unwrap())
        );
        let (native, wrapper) = npm_layout(
            &dir.path().join(if nested {
                "local nested"
            } else {
                "local hoisted"
            }),
            nested,
        );
        assert_eq!(npm_prefix(&native, &wrapper).unwrap(), None);
    }
    let (native, wrapper) = npm_layout(&dir.path().join("forged/lib"), true);
    fs::write(
        wrapper.join("package.json"),
        r#"{"name":"foreign-package"}"#,
    )
    .unwrap();
    assert!(npm_prefix(&native, &wrapper).is_err());
}

#[test]
fn npm_checks_and_upgrades_exact_version_at_original_prefix() {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("prefix with spaces");
    let log = dir.path().join("npm.log");
    let npm = dir.path().join("npm");
    executable(&npm, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nif [ \"$1\" = view ]; then echo '\"0.3.0\"'; fi\n", log.display()).as_bytes());
    let wrapper = prefix.join("lib/node_modules/@zuohaisu/viva/bin/viva.js");
    executable(&wrapper, &script_version("0.3.0"));
    let current = version("0.2.0").unwrap();
    assert!(!update_npm(&current, Some(&prefix), true, npm.to_str().unwrap()).unwrap());
    assert!(!fs::read_to_string(&log).unwrap().contains("install"));
    assert!(update_npm(&current, Some(&prefix), false, npm.to_str().unwrap()).unwrap());
    let calls = fs::read_to_string(&log).unwrap();
    assert!(calls.contains(&format!("install\n--global\n--prefix\n{}\n@zuohaisu/viva@0.3.0\n--no-audit\n--no-fund\n--ignore-scripts\n", prefix.display())));
    assert!(
        !update_npm(
            &version("0.4.0").unwrap(),
            Some(&prefix),
            false,
            npm.to_str().unwrap()
        )
        .unwrap()
    );
    assert!(
        update_npm(&current, None, false, npm.to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("local npm installation")
    );
}

#[test]
fn npm_failure_and_missing_platform_do_not_claim_success() {
    let dir = TempDir::new().unwrap();
    let npm = dir.path().join("npm");
    let prefix = dir.path().join("prefix");
    let current = version("0.2.0").unwrap();
    executable(&npm, b"#!/bin/sh\nif [ \"$1\" = view ]; then echo '\"0.3.0\"'; else echo 'permission denied' >&2; exit 1; fi\n");
    assert!(
        update_npm(&current, Some(&prefix), false, npm.to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("permission denied")
    );
    executable(
        &npm,
        b"#!/bin/sh\nif [ \"$1\" = view ]; then echo '\"0.3.0\"'; fi\n",
    );
    assert!(update_npm(&current, Some(&prefix), false, npm.to_str().unwrap()).is_err());
    executable(
        &prefix.join("lib/node_modules/@zuohaisu/viva/bin/viva.js"),
        &script_version("0.2.0"),
    );
    assert!(
        update_npm(&current, Some(&prefix), false, npm.to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("could not be verified")
    );
    executable(&npm, b"#!/bin/sh\necho '\"0.3.0-rc.1\"'\n");
    assert!(
        update_npm(&current, Some(&prefix), false, npm.to_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("not a stable version")
    );
}

/// Opt-in real-network smoke; only a temporary installation is modified.
#[test]
#[ignore = "downloads a real public release; run explicitly, not in offline CI"]
fn live_github_release_updates_only_sandbox_binary() {
    let dir = TempDir::new().unwrap();
    let target = dir.path().join("viva");
    executable(&target, &script_version("0.0.0"));
    let platform = platform(std::env::consts::OS, std::env::consts::ARCH).unwrap();
    assert!(update_native(&version("0.0.0").unwrap(), &target, platform, false, &Curl).unwrap());
    let mut probe = Command::new(&target);
    probe.arg("--version");
    let reported = capture(&mut probe, Duration::from_secs(10)).unwrap();
    assert!(newer(
        &version("0.0.0").unwrap(),
        &version(reported.trim().strip_prefix("viva ").unwrap()).unwrap()
    ));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2); // binary + lock only
}

#[test]
fn subprocess_timeout_and_exit_errors_are_honest() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exec sleep 10"]);
    assert!(
        capture(&mut command, Duration::from_millis(100))
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "echo real-failure >&2; exit 9"]);
    assert!(
        capture(&mut command, Duration::from_secs(2))
            .unwrap_err()
            .to_string()
            .contains("real-failure")
    );
}
