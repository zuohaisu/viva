//! Explicit, user-invoked CLI upgrades, separate from the resident runtime.
//!
//! Reuses GitHub Releases + the shipped SHA-256 files, system curl (TLS),
//! npm's package manager, and tar/flate2/tempfile for bounded staging and
//! atomic replacement. Never opens VIVA_HOME data or modifies
//! member/task/history records. After a successful install a RUNNING
//! resident server is restarted through the S3 live handoff — the freshly
//! installed entry point takes over and its terminals survive — unless
//! `--no-restart` is given; nothing is spawned when no server runs.
//! Checksums detect corruption; they are not an independent release
//! signature.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};

use crate::foundation::{OfficeError, OfficeResult};

const LATEST_RELEASE: &str = "https://api.github.com/repos/zuohaisu/viva/releases/latest";
const NPM_PACKAGE: &str = "@zuohaisu/viva";
const MAX_METADATA: u64 = 2 * 1024 * 1024;
const MAX_ARCHIVE: u64 = 100 * 1024 * 1024;
const MAX_UNPACKED: u64 = 512 * 1024 * 1024;
const MAX_CHECKSUM: u64 = 4096;
const USAGE: &str = "USAGE: viva update [--check] [--no-restart]\n\
    Upgrade to the latest stable version in the installation's channel.\n\
    --check only checks; it does not replace files or install packages.\n\
    --no-restart skips restarting a running resident server after installing.\n\
    Native macOS: verified GitHub Release binary, atomically replaced.\n\
    Global npm: npm installs @zuohaisu/viva@latest at the same prefix.\n\
    After installing, a running resident server is restarted through the\n\
    live handover (its terminals survive). VIVA_HOME data is untouched.";

fn invalid(message: impl Into<String>) -> OfficeError {
    OfficeError::Validation(message.into())
}

#[derive(Debug, PartialEq)]
enum Mode {
    Install { restart: bool },
    Check,
    Help,
}

fn parse_args(args: &[String]) -> OfficeResult<Mode> {
    match args {
        [] => Ok(Mode::Install { restart: true }),
        [arg] if arg == "--check" => Ok(Mode::Check),
        [arg] if arg == "--no-restart" => Ok(Mode::Install { restart: false }),
        [arg] if matches!(arg.as_str(), "--help" | "-h") => Ok(Mode::Help),
        _ => Err(invalid(USAGE)),
    }
}

/// No automatic/background updates and no runtime/store side effects.
pub fn run(args: &[String]) -> OfficeResult<()> {
    let mode = parse_args(args)?;
    if mode == Mode::Help {
        println!("{USAGE}");
        return Ok(());
    }
    // A maintenance operation on the installed host is not a task grant
    // action. Reject known member contexts rather than treating their
    // request as the owner's. Like other CLI gates, this is NOT an OS sandbox.
    if matches!(mode, Mode::Install { .. })
        && ["VIVA_OFFICE_MEMBER_ID", "VIVA_OFFICE_GRANT_ID"]
            .iter()
            .any(|key| std::env::var_os(key).is_some())
    {
        return Err(invalid(
            "update is a user-only installation operation; run it outside the member/worker context",
        ));
    }
    let current = version(env!("CARGO_PKG_VERSION"))?;
    let target = fs::canonicalize(std::env::current_exe()?)?;
    let installed: Option<PathBuf> = if let Some(root) = std::env::var_os("VIVA_NPM_PACKAGE_ROOT") {
        let prefix = npm_prefix(&target, Path::new(&root))?;
        if update_npm(&current, prefix.as_deref(), mode == Mode::Check, "npm")? {
            Some(npm_wrapper_entry(
                prefix.as_deref().expect("global prefix checked"),
            ))
        } else {
            None
        }
    } else {
        // Never replace a package manager's binary behind its back, even
        // when someone invoked the platform executable without its wrapper.
        if target
            .components()
            .any(|part| matches!(part.as_os_str().to_str(), Some("node_modules" | "Cellar")))
        {
            return Err(invalid(
                "package-managed binary: use the npm `viva` wrapper or your package manager to update",
            ));
        }
        let platform = platform(std::env::consts::OS, std::env::consts::ARCH)?;
        if matches!(mode, Mode::Install { .. }) && is_build_output(&target) {
            return Err(invalid(
                "refusing to overwrite a Cargo build output; rebuild from source or install Viva before updating",
            ));
        }
        if update_native(&current, &target, platform, mode == Mode::Check, &Curl)? {
            Some(target)
        } else {
            None
        }
    };
    match (installed, &mode) {
        // The freshly installed entry takes over the running server: the
        // handover IS the upgrade (S3). Nothing to spawn when none runs.
        (Some(entry), Mode::Install { restart: true }) => restart_running_server(
            &crate::foundation::paths::viva_home(None),
            &entry,
        ),
        (Some(_), Mode::Install { restart: false }) => print_restart_skipped(),
        _ => {}
    }
    Ok(())
}

fn version(raw: &str) -> OfficeResult<Version> {
    Version::parse(raw.strip_prefix('v').unwrap_or(raw))
        .map_err(|err| invalid(format!("invalid release version `{raw}`: {err}")))
}

fn newer(current: &Version, latest: &Version) -> bool {
    // Build metadata is not precedence; never install an equal or older version.
    latest.cmp_precedence(current).is_gt()
}

fn is_build_output(path: &Path) -> bool {
    let parts: Vec<_> = path.components().collect();
    parts.iter().enumerate().any(|(i, part)| {
        part.as_os_str() == "target"
            && parts[i + 1..]
                .iter()
                .any(|p| matches!(p.as_os_str().to_str(), Some("debug" | "release")))
    })
}

fn platform(os: &str, arch: &str) -> OfficeResult<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-intel"),
        _ => Err(invalid(format!(
            "no Viva release binary for {os}/{arch}; native update supports macOS Apple Silicon and Intel"
        ))),
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

impl Release {
    fn stable_version(&self) -> OfficeResult<Version> {
        let version = version(&self.tag_name)?;
        if self.draft || self.prerelease || !version.pre.is_empty() {
            return Err(invalid("latest release is not a published stable version"));
        }
        Ok(version)
    }

    fn asset_url(&self, name: &str, max_size: u64) -> OfficeResult<&str> {
        let matches: Vec<_> = self.assets.iter().filter(|a| a.name == name).collect();
        let [asset] = matches.as_slice() else {
            return Err(invalid(format!(
                "release {} must contain exactly one `{name}` asset",
                self.tag_name
            )));
        };
        let expected = format!(
            "https://github.com/zuohaisu/viva/releases/download/{}/{name}",
            self.tag_name
        );
        if asset.browser_download_url != expected || asset.size == 0 || asset.size > max_size {
            return Err(invalid(format!(
                "invalid URL or size for release asset `{name}`"
            )));
        }
        Ok(&asset.browser_download_url)
    }
}

trait Download {
    fn fetch(&self, url: &str, destination: &Path, max_bytes: u64) -> OfficeResult<()>;
}

struct Curl;
impl Download for Curl {
    fn fetch(&self, url: &str, destination: &Path, max_bytes: u64) -> OfficeResult<()> {
        // Use macOS's existing TLS client, not a shell or remotely fetched
        // installer. Ignore curlrc; require HTTPS including every redirect.
        let mut command = Command::new("/usr/bin/curl");
        command
            .args([
                "--disable",
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--connect-timeout",
                "10",
                "--max-time",
                "120",
                "--user-agent",
                concat!("viva/", env!("CARGO_PKG_VERSION")),
                "--max-filesize",
                &max_bytes.to_string(),
                "--output",
            ])
            .arg(destination)
            .arg("--url")
            .arg(url);
        capture(&mut command, Duration::from_secs(130))?;
        if fs::metadata(destination)?.len() > max_bytes {
            return Err(invalid("release download exceeds the size limit"));
        }
        Ok(())
    }
}

fn update_native(
    current: &Version,
    target: &Path,
    platform: &str,
    check: bool,
    download: &impl Download,
) -> OfficeResult<bool> {
    let metadata_dir = TempDir::new()?;
    let metadata = metadata_dir.path().join("release.json");
    download.fetch(LATEST_RELEASE, &metadata, MAX_METADATA)?;
    let release: Release = serde_json::from_slice(&read_bounded(&metadata, MAX_METADATA)?)?;
    let latest = release.stable_version()?;
    println!("Current: {current}; latest: {latest} (GitHub Releases)");
    if !newer(current, &latest) {
        println!("Already up to date; no downgrade performed.");
        return Ok(false);
    }
    let archive_name = format!("viva-{platform}.tar.gz");
    let archive_url = release.asset_url(&archive_name, MAX_ARCHIVE)?;
    let checksum_url = release.asset_url(&format!("{archive_name}.sha256"), MAX_CHECKSUM)?;
    if check {
        println!("Update available. Run `viva update` to install {latest}.");
        return Ok(false);
    }

    // Stage on the target filesystem; a read-only installation fails here
    // without ever truncating the running executable. A persistent advisory
    // lock serializes competing upgrades; unlinking it would introduce races.
    let parent = target
        .parent()
        .ok_or_else(|| invalid("binary has no parent directory"))?;
    let _lock = lock_installation(target)?;
    let mut installed_probe = Command::new(target);
    installed_probe.arg("--version");
    if capture(&mut installed_probe, Duration::from_secs(10))?.trim() != format!("viva {current}") {
        return Err(invalid(
            "installed binary changed since this process started; run `viva update` again",
        ));
    }
    let stage = tempfile::Builder::new()
        .prefix(".viva-update-")
        .tempdir_in(parent)?;
    let archive = stage.path().join(&archive_name);
    let checksum = stage.path().join("checksum");
    println!("Downloading Viva {latest} for {platform}…");
    download.fetch(checksum_url, &checksum, MAX_CHECKSUM)?;
    download.fetch(archive_url, &archive, MAX_ARCHIVE)?;
    verify_checksum(&archive, &checksum, &archive_name)?;
    let mut candidate = NamedTempFile::new_in(parent)?;
    extract_binary(
        &archive,
        &format!("viva-{platform}/viva"),
        candidate.as_file_mut(),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(target)?.permissions().mode() & 0o777;
        candidate
            .as_file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
    }
    candidate.as_file().sync_all()?;
    // Close the writable handle before executing (required on Linux, and
    // harmless on macOS). TempPath still cleans up every failed staging run.
    let candidate = candidate.into_temp_path();
    let mut probe = Command::new(&candidate);
    probe.arg("--version");
    let reported = capture(&mut probe, Duration::from_secs(10))?;
    if reported.trim() != format!("viva {latest}") {
        return Err(invalid(format!(
            "downloaded binary version mismatch: expected viva {latest}"
        )));
    }
    // tempfile::persist uses rename: replacement is atomic on supported
    // Unix filesystems, even while the old inode is executing.
    candidate
        .persist(target)
        .map_err(|err| OfficeError::Io(err.error))?;
    if let Err(err) = File::open(parent).and_then(|dir| dir.sync_all()) {
        eprintln!("viva: binary replaced, but syncing its directory failed: {err}");
    }
    println!("Updated Viva {current} → {latest} at {}.", target.display());
    Ok(true)
}

fn read_bounded(path: &Path, limit: u64) -> OfficeResult<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("update input exceeds the size limit"));
    }
    Ok(bytes)
}

fn verify_checksum(archive: &Path, checksum: &Path, archive_name: &str) -> OfficeResult<()> {
    let text = String::from_utf8(read_bounded(checksum, MAX_CHECKSUM)?)
        .map_err(|_| invalid("checksum is not UTF-8"))?;
    let fields: Vec<_> = text.split_whitespace().collect();
    if fields.len() != 2
        || fields[0].len() != 64
        || !fields[0].bytes().all(|b| b.is_ascii_hexdigit())
        || fields[1].trim_start_matches('*') != archive_name
    {
        return Err(invalid("invalid SHA-256 checksum file or archive filename"));
    }
    let mut file = File::open(archive)?;
    if file.metadata()?.len() > MAX_ARCHIVE {
        return Err(invalid("release archive exceeds the size limit"));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if format!("{:x}", hasher.finalize()) != fields[0].to_ascii_lowercase() {
        return Err(invalid(
            "SHA-256 checksum mismatch; installed binary was not changed",
        ));
    }
    Ok(())
}

fn extract_binary(archive: &Path, expected: &str, output: &mut File) -> OfficeResult<()> {
    // Never unpack paths onto the filesystem. Only one exact regular-file
    // entry is copied to our already-open staging file; links/traversal are
    // not followed. Bound expanded bytes as well as the compressed download.
    let decoder = GzDecoder::new(File::open(archive)?).take(MAX_UNPACKED);
    let mut archive = tar::Archive::new(decoder);
    let mut found = false;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new(expected) {
            continue;
        }
        if found || !entry.header().entry_type().is_file() || entry.size() > MAX_ARCHIVE {
            return Err(invalid(
                "release binary must be one bounded regular file, not a link",
            ));
        }
        let size = entry.size();
        if size == 0 || std::io::copy(&mut entry, output)? != size {
            return Err(invalid("release binary is empty or truncated"));
        }
        found = true;
    }
    if !found {
        return Err(invalid(
            "release archive does not contain the platform binary",
        ));
    }
    output.flush()?;
    Ok(())
}

fn lock_installation(target: &Path) -> OfficeResult<File> {
    let name = target
        .file_name()
        .ok_or_else(|| invalid("binary has no filename"))?;
    let lock_path = target.with_file_name(format!(".{}-update.lock", name.to_string_lossy()));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(lock_path)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(invalid(
                "another update is in progress (installation lock unavailable)",
            ));
        }
    }
    Ok(file)
}

/// The npm wrapper supplies its own root. Validate both package identities
/// and placement; don't let direct execution silently mutate node_modules.
/// A local install is checkable, but mutation stays with the project's npm.
fn npm_prefix(target: &Path, wrapper: &Path) -> OfficeResult<Option<PathBuf>> {
    let target = fs::canonicalize(target)?;
    let wrapper = fs::canonicalize(wrapper)?;
    let package_name = |root: &Path| -> OfficeResult<String> {
        let value: serde_json::Value =
            serde_json::from_slice(&read_bounded(&root.join("package.json"), MAX_METADATA)?)?;
        value["name"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid("npm package has no name"))
    };
    let native_root = target
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("invalid npm binary path"))?;
    let native_name = package_name(native_root)?;
    if package_name(&wrapper)? != NPM_PACKAGE
        || !matches!(
            native_name.as_str(),
            "@zuohaisu/viva-darwin-arm64" | "@zuohaisu/viva-darwin-x64"
        )
    {
        return Err(invalid("npm installation package identity mismatch"));
    }
    let modules = wrapper
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("invalid npm wrapper path"))?;
    if wrapper.file_name().and_then(|s| s.to_str()) != Some("viva")
        || wrapper
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            != Some("@zuohaisu")
        || modules.file_name().and_then(|s| s.to_str()) != Some("node_modules")
        || !(native_root == modules.join("@zuohaisu").join("viva-darwin-arm64")
            || native_root == modules.join("@zuohaisu").join("viva-darwin-x64")
            || native_root == wrapper.join("node_modules/@zuohaisu/viva-darwin-arm64")
            || native_root == wrapper.join("node_modules/@zuohaisu/viva-darwin-x64"))
    {
        return Err(invalid("npm wrapper does not own this platform binary"));
    }
    let parent = modules
        .parent()
        .ok_or_else(|| invalid("invalid npm prefix"))?;
    if parent.file_name().and_then(|s| s.to_str()) == Some("lib") {
        return Ok(parent.parent().map(Path::to_path_buf));
    }
    Ok(None)
}

fn update_npm(
    current: &Version,
    prefix: Option<&Path>,
    check: bool,
    npm: &str,
) -> OfficeResult<bool> {
    if !check && prefix.is_none() {
        return Err(invalid(
            "local npm installation: update from your project with `npm install @zuohaisu/viva@latest`; viva update upgrades global installations only",
        ));
    }
    let mut query = Command::new(npm);
    query.args(["view", "@zuohaisu/viva@latest", "version", "--json"]);
    let raw = capture(&mut query, Duration::from_secs(120))?;
    let latest = version(&serde_json::from_str::<String>(&raw)?)?;
    if !latest.pre.is_empty() {
        return Err(invalid("npm latest is not a stable version"));
    }
    println!("Current: {current}; latest: {latest} (npm)");
    if !newer(current, &latest) {
        println!("Already up to date; no downgrade performed.");
        return Ok(false);
    }
    if check {
        println!(
            "Update available. Run `viva update` for a global install, or `npm install @zuohaisu/viva@latest` for a local install."
        );
        return Ok(false);
    }
    println!("Updating @zuohaisu/viva through npm…");
    let mut install = Command::new(npm);
    // Pin the version we just checked: a changing dist-tag cannot introduce
    // an unexamined prerelease/downgrade. Keep npm's dependency integrity
    // verification and update the wrapper + platform package together.
    install
        .args(["install", "--global", "--prefix"])
        .arg(prefix.expect("global prefix checked"))
        .arg(format!("{NPM_PACKAGE}@{latest}"))
        .args(["--no-audit", "--no-fund", "--ignore-scripts"]);
    capture(&mut install, Duration::from_secs(300))?;
    let wrapper = npm_wrapper_entry(prefix.unwrap());
    let mut probe = Command::new(&wrapper);
    probe.arg("--version");
    if capture(&mut probe, Duration::from_secs(10))?.trim() != format!("viva {latest}") {
        return Err(invalid(
            "npm exited successfully but the installed Viva version could not be verified; inspect the installation before retrying",
        ));
    }
    println!(
        "npm installed Viva {latest} at {}.",
        prefix.unwrap().display()
    );
    Ok(true)
}

/// The verified global-install entry point: the wrapper npm put on PATH,
/// which resolves and execs the platform binary of its own channel.
fn npm_wrapper_entry(prefix: &Path) -> PathBuf {
    prefix.join("lib/node_modules/@zuohaisu/viva/bin/viva.js")
}

/// Post-install activation (default): hand a RUNNING resident server to the
/// freshly installed entry point through the S3 live handoff — the handover
/// IS the upgrade, and its terminals survive. No server, or a socket with no
/// healthy host, is left alone with an honest note; a failed handover keeps
/// the old generation serving and says so instead of failing the install,
/// which already succeeded.
fn restart_running_server(home: &Path, entry: &Path) {
    let socket = home.join(crate::office::OFFICE_SOCKET_NAME);
    if !socket.exists() {
        println!("No running resident server; the next `viva` start will use the new version.");
        return;
    }
    // Only a healthy host can hand over. A socket nobody answers is left
    // for the next start to claim (`OfficeHost::open` records that).
    if crate::office::OfficeClient::connect(home).is_err() {
        eprintln!(
            "viva: socket {} answered no healthy host; nothing to restart — \
             the next `viva` start will claim it if it is stale",
            socket.display()
        );
        return;
    }
    match crate::office::restart_server_with(home, entry) {
        Ok(new_pid) => println!(
            "Resident server restarted on the new version (pid {new_pid}); \
             its terminals carried over — reconnect the TUI."
        ),
        Err(err) => eprintln!(
            "viva: update installed, but the server restart failed: {err}\n\
             The old server keeps serving; run `viva server-restart` when ready."
        ),
    }
}

fn print_restart_skipped() {
    println!(
        "Server restart skipped (--no-restart). A running server keeps the old version until `viva server-restart`; reconnect the TUI afterwards."
    );
}

/// Bounded capture + deadline for the existing curl/npm and version probe.
/// Files avoid pipe deadlocks; no shell interpretation, stdin prompts, or
/// unbounded in-memory subprocess output. Failures retain their real cause.
fn capture(command: &mut Command, timeout: Duration) -> OfficeResult<String> {
    let stdout = NamedTempFile::new()?;
    let stderr = NamedTempFile::new()?;
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(stdout.reopen()?)
        .stderr(stderr.reopen()?);
    let mut child = command
        .spawn()
        .map_err(|err| invalid(format!("could not start `{program}`: {err}")))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid(format!(
                "`{program}` timed out after {}s",
                timeout.as_secs()
            )));
        }
        if stdout.as_file().metadata()?.len() > MAX_METADATA
            || stderr.as_file().metadata()?.len() > MAX_METADATA
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid(format!(
                "`{program}` output exceeds the size limit"
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if !status.success() {
        let mut details = Vec::new();
        File::open(stderr.path())?
            .take(4096)
            .read_to_end(&mut details)?;
        return Err(invalid(format!(
            "`{program}` failed ({status}): {}",
            String::from_utf8_lossy(&details).trim()
        )));
    }
    String::from_utf8(read_bounded(stdout.path(), MAX_METADATA)?)
        .map_err(|_| invalid(format!("`{program}` output is not UTF-8")))
}

#[cfg(all(test, unix))]
mod tests;
