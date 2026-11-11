//! Owner anchor for the native runtime.
//!
//! The owner credential is never a file. The spawning TUI generates it and
//! hands it to the server over an inherited pipe (`VIVA_OWNER_ANCHOR_FD`); a
//! reattaching TUI/control process obtains the same anchor from the login
//! keychain, which requires the user's presence. A plain same-UID process
//! holds neither and is refused by the host. Tests and the probe supply the
//! anchor through the same inherited-fd protocol.
use std::{
    io::{self, Read},
    path::Path,
};

pub(super) const FD_ENV: &str = "VIVA_OWNER_ANCHOR_FD";
pub(super) const KEYCHAIN_ENV: &str = "VIVA_OWNER_KEYCHAIN";
const PREFIX: &str = "native-anchor:";
const MAX_ANCHOR_BYTES: usize = 512;

/// A fresh host incarnation plus its anchor. The host id travels inside the
/// anchor so a stale credential can be named as stale.
#[cfg(test)]
pub(super) fn generate() -> (String, String) {
    let host_id = format!("native-{}", uuid::Uuid::new_v4().simple());
    let anchor = format!("{PREFIX}{host_id}:{}", uuid::Uuid::new_v4().simple());
    (host_id, anchor)
}

/// An anchor for a host id that was chosen before the anchor existed.
pub(super) fn with_host(host_id: &str) -> String {
    format!("{PREFIX}{host_id}:{}", uuid::Uuid::new_v4().simple())
}

pub(super) fn host_id(anchor: &str) -> Option<&str> {
    let rest = anchor.strip_prefix(PREFIX)?;
    let (host, secret) = rest.split_once(':')?;
    (host.starts_with("native-") && !host.contains(':') && !secret.is_empty()).then_some(host)
}

/// Reads the anchor from the inherited descriptor named by `FD_ENV`.
///
/// `Ok(None)` means the variable is absent. A set but unreadable descriptor is
/// an error: the caller must not silently fall back to a weaker anchor.
pub(super) fn take_inherited() -> io::Result<Option<String>> {
    let Some(raw) = std::env::var_os(FD_ENV) else {
        return Ok(None);
    };
    // The descriptor is inherited by contract; panes must not inherit it.
    // SAFETY: environment mutation is process-global and startup is still
    // single-threaded here.
    unsafe { std::env::remove_var(FD_ENV) };
    let fd: libc::c_int = raw
        .to_str()
        .and_then(|value| value.parse().ok())
        .filter(|fd| *fd >= 0)
        .ok_or_else(|| invalid("owner anchor descriptor is not a number"))?;
    // SAFETY: fcntl on a caller-owned descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(invalid("owner anchor descriptor is not open"));
    }
    // SAFETY: as above; only the close-on-exec flag changes.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    let mut value = Vec::new();
    file.take(MAX_ANCHOR_BYTES as u64 + 1)
        .read_to_end(&mut value)?;
    let value = String::from_utf8(value)
        .map_err(|_| invalid("owner anchor is not valid UTF-8"))?
        .trim()
        .to_string();
    if value.is_empty() {
        return Err(invalid("owner anchor is empty"));
    }
    if value.len() > MAX_ANCHOR_BYTES || host_id(&value).is_none() {
        return Err(invalid(
            "owner anchor does not have the native anchor format",
        ));
    }
    Ok(Some(value))
}

pub(super) fn keychain_enabled() -> bool {
    std::env::var(KEYCHAIN_ENV).map_or(true, |value| value != "0")
}

fn keychain_account(home: &Path, host_id: &str) -> String {
    format!("{}|{host_id}", home.display())
}

pub(super) fn store_keychain(home: &Path, anchor: &str) -> io::Result<()> {
    if !keychain_enabled() {
        return Err(unsupported("owner keychain anchor is disabled"));
    }
    let host_id =
        host_id(anchor).ok_or_else(|| invalid("cannot store a malformed owner anchor"))?;
    keychain::store(&keychain_account(home, host_id), anchor)
}

pub(super) fn load_keychain(home: &Path, host_id: &str) -> io::Result<String> {
    if !keychain_enabled() {
        return Err(unsupported("owner keychain anchor is disabled"));
    }
    keychain::load(&keychain_account(home, host_id))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(target_os = "macos")]
mod keychain {
    //! Login-keychain item with no trusted applications: every read is a
    //! user-presence gate (the user approves or enters the keychain password).
    //! The legacy API is used deliberately; the data-protection keychain needs
    //! an entitlement an unsigned CLI does not have.
    use super::{invalid, unsupported};
    use std::{
        ffi::{CString, c_char, c_void},
        io, ptr,
    };

    type CfRef = *const c_void;
    type OsStatus = i32;
    const SUCCESS: OsStatus = 0;
    const ITEM_NOT_FOUND: OsStatus = -25300;
    const DUPLICATE_ITEM: OsStatus = -25299;
    const INTERACTION_NOT_ALLOWED: OsStatus = -25308;
    const USER_CANCELED: OsStatus = -128;
    const AUTH_FAILED: OsStatus = -25293;
    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const SERVICE: &str = "viva-native-owner";

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: CfRef,
            value: *const c_char,
            encoding: u32,
        ) -> CfRef;
        fn CFRelease(value: CfRef);
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        fn SecAccessCreate(descriptor: CfRef, trusted: CfRef, access: *mut CfRef) -> OsStatus;
        fn SecKeychainAddGenericPassword(
            keychain: CfRef,
            service_len: u32,
            service: *const c_char,
            account_len: u32,
            account: *const c_char,
            password_len: u32,
            password: *const c_void,
            item: *mut CfRef,
        ) -> OsStatus;
        fn SecKeychainFindGenericPassword(
            keychain_or_array: CfRef,
            service_len: u32,
            service: *const c_char,
            account_len: u32,
            account: *const c_char,
            password_len: *mut u32,
            password_data: *mut *mut c_void,
            item: *mut CfRef,
        ) -> OsStatus;
        fn SecKeychainItemSetAccess(item: CfRef, access: CfRef) -> OsStatus;
        fn SecKeychainItemFreeContent(attributes: *mut c_void, data: *mut c_void) -> OsStatus;
    }

    struct Cf(CfRef);
    impl Drop for Cf {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: created by a CoreFoundation/Security function.
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn status_error(status: OsStatus, operation: &str) -> io::Error {
        let hint = match status {
            ITEM_NOT_FOUND => "no owner anchor exists for this host incarnation",
            INTERACTION_NOT_ALLOWED => "user presence is unavailable or was not allowed",
            USER_CANCELED => "the user canceled the owner anchor prompt",
            AUTH_FAILED => "owner anchor authentication failed",
            _ => "keychain reported a failure",
        };
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{operation} refused: {hint} ({status})"),
        )
    }

    fn cf_string(value: &str) -> io::Result<Cf> {
        let value = CString::new(value).map_err(|_| invalid("keychain text contains NUL"))?;
        // SAFETY: a NUL-terminated string and the documented UTF-8 encoding.
        let raw = unsafe {
            CFStringCreateWithCString(ptr::null(), value.as_ptr(), CF_STRING_ENCODING_UTF8)
        };
        (!raw.is_null())
            .then_some(Cf(raw))
            .ok_or_else(|| invalid("cannot create a keychain string"))
    }

    pub(super) fn store(account: &str, anchor: &str) -> io::Result<()> {
        let service = CString::new(SERVICE).expect("static service");
        let account =
            CString::new(account).map_err(|_| invalid("keychain account contains NUL"))?;
        // An empty trusted-application list makes every future read a prompt.
        let descriptor = cf_string("Viva native owner anchor")?;
        let mut access: CfRef = ptr::null();
        // SAFETY: a valid descriptor and a null trusted list.
        let status = unsafe { SecAccessCreate(descriptor.0, ptr::null(), &mut access) };
        if status != SUCCESS {
            return Err(status_error(status, "creating the owner keychain access"));
        }
        let access = Cf(access);
        let mut item: CfRef = ptr::null();
        // SAFETY: lengths match the pointers; the default keychain is null.
        let status = unsafe {
            SecKeychainAddGenericPassword(
                ptr::null(),
                service.as_bytes().len() as u32,
                service.as_ptr(),
                account.as_bytes().len() as u32,
                account.as_ptr(),
                anchor.len() as u32,
                anchor.as_ptr() as *const c_void,
                &mut item,
            )
        };
        if status == DUPLICATE_ITEM {
            return Err(unsupported(
                "an owner anchor already exists for this host incarnation",
            ));
        }
        if status != SUCCESS {
            return Err(status_error(status, "storing the owner anchor"));
        }
        let item = Cf(item);
        // SAFETY: both references are live for this call.
        let status = unsafe { SecKeychainItemSetAccess(item.0, access.0) };
        if status != SUCCESS {
            return Err(status_error(status, "protecting the owner anchor"));
        }
        Ok(())
    }

    pub(super) fn load(account: &str) -> io::Result<String> {
        let service = CString::new(SERVICE).expect("static service");
        let account =
            CString::new(account).map_err(|_| invalid("keychain account contains NUL"))?;
        let mut length = 0_u32;
        let mut data: *mut c_void = ptr::null_mut();
        let mut item: CfRef = ptr::null();
        // SAFETY: out-parameters are valid; a null keychain means the default
        // login keychain. The access object has no trusted apps, so this call
        // asks the user before returning the anchor.
        let status = unsafe {
            SecKeychainFindGenericPassword(
                ptr::null(),
                service.as_bytes().len() as u32,
                service.as_ptr(),
                account.as_bytes().len() as u32,
                account.as_ptr(),
                &mut length,
                &mut data,
                &mut item,
            )
        };
        if status != SUCCESS {
            return Err(status_error(status, "reading the owner anchor"));
        }
        let item = Cf(item);
        // SAFETY: the Security framework returned `length` readable bytes.
        let value = unsafe { std::slice::from_raw_parts(data as *const u8, length as usize) };
        let value = String::from_utf8(value.to_vec())
            .map_err(|_| invalid("owner anchor is not valid UTF-8"));
        // SAFETY: the buffer belongs to the item and is released exactly once.
        unsafe {
            SecKeychainItemFreeContent(ptr::null_mut(), data);
        }
        let _ = item;
        value
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
    use super::unsupported;
    use std::io;

    pub(super) fn store(_account: &str, _anchor: &str) -> io::Result<()> {
        Err(unsupported(
            "no user-presence owner anchor exists on this platform",
        ))
    }

    pub(super) fn load(_account: &str) -> io::Result<String> {
        Err(unsupported(
            "no user-presence owner anchor exists on this platform",
        ))
    }
}
