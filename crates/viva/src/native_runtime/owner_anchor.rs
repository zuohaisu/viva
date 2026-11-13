//! Per-host local runtime capability. This is NOT proof of human presence.
//!
//! Reuse the native host-incarnation anchor and inherited spawn descriptor.
//! Fresh processes resolve the live host's anchor in a private runtime file.
//! The local account (including its explicitly credential-bearing controllers)
//! is trusted; this is not an OS sandbox against arbitrary same-UID programs.
use std::{
    ffi::CString,
    fs::{DirBuilder, File},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::DirBuilderExt,
    },
    path::Path,
};

pub(super) const FD_ENV: &str = "VIVA_OWNER_ANCHOR_FD";
const PREFIX: &str = "native-anchor:";
const MAX_ANCHOR_BYTES: usize = 512;
const DIRECTORY: &str = "runtime-capability";
const FILE: &str = "owner-anchor";

#[cfg(test)]
pub(super) fn generate() -> (String, String) {
    let host_id = format!("native-{}", uuid::Uuid::new_v4().simple());
    let anchor = with_host(&host_id);
    (host_id, anchor)
}

pub(super) fn with_host(host_id: &str) -> String {
    format!("{PREFIX}{host_id}:{}", uuid::Uuid::new_v4().simple())
}

pub(super) fn host_id(anchor: &str) -> Option<&str> {
    let rest = anchor.strip_prefix(PREFIX)?;
    let (host, secret) = rest.split_once(':')?;
    let safe = |value: &str| {
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    };
    (anchor.len() <= MAX_ANCHOR_BYTES
        && host.starts_with("native-")
        && host.len() > 7
        && safe(host)
        && !secret.is_empty()
        && safe(secret))
    .then_some(host)
}

/// A set but unreadable descriptor is an error, never a reason to fall back.
pub(super) fn take_inherited() -> io::Result<Option<String>> {
    let Some(raw) = std::env::var_os(FD_ENV) else {
        return Ok(None);
    };
    // SAFETY: single-threaded process startup; panes must not inherit this fd.
    unsafe { std::env::remove_var(FD_ENV) };
    let fd: libc::c_int = raw
        .to_str()
        .and_then(|value| value.parse().ok())
        .filter(|fd| *fd >= 0)
        .ok_or_else(|| invalid("owner anchor descriptor is not a number"))?;
    // SAFETY: fcntl checks a caller-owned descriptor and only changes CLOEXEC.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(invalid("owner anchor descriptor is not open"));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this process consumes the inherited descriptor exactly once.
    let file = unsafe { File::from_raw_fd(fd) };
    read_anchor(file).map(Some)
}

fn read_anchor(file: File) -> io::Result<String> {
    let mut bytes = Vec::new();
    file.take(MAX_ANCHOR_BYTES as u64 + 2)
        .read_to_end(&mut bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("runtime capability is not UTF-8"))?;
    let value = text.strip_suffix('\n').unwrap_or(&text);
    if value.len() > MAX_ANCHOR_BYTES || host_id(value).is_none() {
        return Err(invalid(
            "runtime capability has an invalid native anchor format",
        ));
    }
    Ok(value.to_owned())
}

/// Do not chmod/follow an existing insecure or symlinked state root.
pub(super) fn prepare_home(home: &Path) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(home)?;
    open_home(home).map(|_| ())
}

fn open_home(home: &Path) -> io::Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(home.as_os_str().as_bytes())
        .map_err(|_| invalid("state root contains NUL"))?;
    // SAFETY: a valid C path; the returned fd is owned below.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    checked_file(fd, true)
}

fn checked_file(fd: libc::c_int, directory: bool) -> io::Result<File> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a newly opened fd transferred exactly once to File.
    let file = unsafe { File::from_raw_fd(fd) };
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut stat) } < 0 {
        return Err(io::Error::last_os_error());
    }
    validate_stat(&stat, directory)?;
    Ok(file)
}

fn validate_stat(stat: &libc::stat, directory: bool) -> io::Result<()> {
    let (kind, mode) = if directory {
        (libc::S_IFDIR, 0o700)
    } else {
        (libc::S_IFREG, 0o600)
    };
    if stat.st_uid != unsafe { libc::geteuid() }
        || stat.st_mode & libc::S_IFMT != kind
        || stat.st_mode & 0o7777 != mode
        || (!directory && stat.st_nlink != 1)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "runtime capability requires owned 0700 directories and a single-link 0600 regular file",
        ));
    }
    Ok(())
}

fn open_at(
    directory: &File,
    name: &str,
    flags: libc::c_int,
    is_directory: bool,
) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| invalid("runtime capability path contains NUL"))?;
    // SAFETY: valid dirfd/name; O_NONBLOCK prevents a substituted FIFO hanging.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    checked_file(fd, is_directory)
}

fn directory(home: &Path, create: bool) -> io::Result<File> {
    let home = open_home(home)?;
    if create {
        let name = CString::new(DIRECTORY).expect("static path");
        // SAFETY: valid dirfd/name; an existing entry is checked by openat.
        if unsafe { libc::mkdirat(home.as_raw_fd(), name.as_ptr(), 0o700) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
    }
    open_at(&home, DIRECTORY, libc::O_RDONLY | libc::O_DIRECTORY, true)
}

/// Publish only after claiming both host endpoints. Rename makes readers see
/// either a complete previous incarnation or a complete new one, never a tear.
pub(super) fn store_runtime(home: &Path, anchor: &str) -> io::Result<()> {
    host_id(anchor).ok_or_else(|| invalid("cannot store a malformed runtime capability"))?;
    let directory = directory(home, true)?;
    match open_at(&directory, FILE, libc::O_RDONLY, false) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let temporary = format!(".anchor-{}", uuid::Uuid::new_v4().simple());
    let mut file = open_at(
        &directory,
        &temporary,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        false,
    )?;
    let temporary = CString::new(temporary).expect("generated path");
    let target = CString::new(FILE).expect("static path");
    let result = (|| {
        writeln!(file, "{anchor}")?;
        file.sync_all()?;
        // SAFETY: both names are relative to the pinned, private directory.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                target.as_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        directory.sync_all()
    })();
    if result.is_err() {
        // Only our unique temporary entry, never unrelated or old credentials.
        unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
    }
    result
}

pub(super) fn load_runtime(home: &Path, expected_host: &str) -> io::Result<String> {
    let directory = directory(home, false)?;
    let anchor = read_anchor(open_at(&directory, FILE, libc::O_RDONLY, false)?)?;
    if host_id(&anchor) != Some(expected_host) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "stale_owner_anchor: runtime capability belongs to another host incarnation",
        ));
    }
    Ok(anchor)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    fn fixture() -> tempfile::TempDir {
        let home = tempfile::Builder::new().prefix("ac").tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        home
    }

    #[test]
    fn private_store_roundtrip_rotates_incarnation_atomically() {
        let home = fixture();
        let (host, anchor) = generate();
        store_runtime(home.path(), &anchor).unwrap();
        let dir = home.path().join(DIRECTORY);
        let path = dir.join(FILE);
        assert_eq!(dir.metadata().unwrap().mode() & 0o7777, 0o700);
        assert_eq!(path.metadata().unwrap().mode() & 0o7777, 0o600);
        assert_eq!(load_runtime(home.path(), &host).unwrap(), anchor);
        let previous = File::open(&path).unwrap();
        let (next_host, next) = generate();
        store_runtime(home.path(), &next).unwrap();
        assert_eq!(
            read_anchor(previous).unwrap(),
            anchor,
            "atomic replacement, not in-place truncation"
        );
        assert_eq!(load_runtime(home.path(), &next_host).unwrap(), next);
        assert_eq!(
            load_runtime(home.path(), &host).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
    }

    #[test]
    fn maximum_size_anchor_roundtrips_with_its_newline() {
        let home = fixture();
        let mut anchor = String::from("native-anchor:native-boundary:");
        anchor.push_str(&"a".repeat(MAX_ANCHOR_BYTES - anchor.len()));
        store_runtime(home.path(), &anchor).unwrap();
        assert_eq!(
            load_runtime(home.path(), "native-boundary").unwrap(),
            anchor
        );
    }

    #[test]
    fn malformed_truncated_oversized_and_non_utf8_files_are_refused() {
        let home = fixture();
        let (host, anchor) = generate();
        store_runtime(home.path(), &anchor).unwrap();
        let path = home.path().join(DIRECTORY).join(FILE);
        for bytes in [
            vec![],
            b"native-anchor:native-x:".to_vec(),
            b"native-anchor:native-x:a:b".to_vec(),
            b"native-anchor:../x:secret".to_vec(),
            b"native-anchor:native-x:secret\nextra".to_vec(),
            vec![b'a'; MAX_ANCHOR_BYTES + 2],
            vec![0xff],
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(
                load_runtime(home.path(), &host).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        assert!(store_runtime(home.path(), "native-anchor:native-x:a:b").is_err());
    }

    #[test]
    fn unsafe_file_modes_and_links_are_refused_on_both_read_and_write() {
        let home = fixture();
        let (host, anchor) = generate();
        store_runtime(home.path(), &anchor).unwrap();
        let path = home.path().join(DIRECTORY).join(FILE);
        for mode in [0o644, 0o666, 0o400, 0o1600] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(load_runtime(home.path(), &host).is_err());
            assert!(store_runtime(home.path(), &anchor).is_err());
            assert_eq!(path.metadata().unwrap().mode() & 0o7777, mode);
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let other = home.path().join("sentinel");
        std::fs::rename(&path, &other).unwrap();
        symlink(&other, &path).unwrap();
        assert!(load_runtime(home.path(), &host).is_err());
        assert!(store_runtime(home.path(), &anchor).is_err());
        assert_eq!(
            std::fs::read_to_string(&other).unwrap(),
            format!("{anchor}\n")
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&other, &path).unwrap();
        assert!(load_runtime(home.path(), &host).is_err());
        assert!(store_runtime(home.path(), &anchor).is_err());
    }

    #[test]
    fn unsafe_directory_mode_and_symlink_never_get_repaired_or_followed() {
        let home = fixture();
        let (host, anchor) = generate();
        store_runtime(home.path(), &anchor).unwrap();
        let dir = home.path().join(DIRECTORY);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load_runtime(home.path(), &host).is_err());
        assert!(store_runtime(home.path(), &anchor).is_err());
        assert_eq!(dir.metadata().unwrap().mode() & 0o7777, 0o755);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let other = home.path().join("other");
        std::fs::rename(&dir, &other).unwrap();
        symlink(&other, &dir).unwrap();
        assert!(load_runtime(home.path(), &host).is_err());
        assert!(store_runtime(home.path(), &anchor).is_err());
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_home(home.path()).is_err());
        assert_eq!(home.path().metadata().unwrap().mode() & 0o7777, 0o755);
    }

    #[test]
    fn wrong_owner_metadata_is_refused() {
        // Constructed metadata isolates the ownership rule without requiring
        // root/chown on the real machine. Other tests open actual filesystem fds.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        stat.st_uid = unsafe { libc::geteuid() };
        stat.st_mode = libc::S_IFREG | 0o600;
        stat.st_nlink = 1;
        validate_stat(&stat, false).unwrap();
        stat.st_uid = stat.st_uid.wrapping_add(1);
        assert_eq!(
            validate_stat(&stat, false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        stat.st_mode = libc::S_IFDIR | 0o700;
        assert!(validate_stat(&stat, true).is_err());
    }
}
