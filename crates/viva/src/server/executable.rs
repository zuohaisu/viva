//! Pin the executing image rather than trusting the current pathname (#201).
//! Linux exposes its inode directly. Darwin's libproc exposes the vnode for
//! dyld's mapped main image; match it against the SAME descriptor we hash.
use std::{fs::File, io};

pub(super) fn open_running() -> io::Result<File> {
    #[cfg(target_os = "linux")]
    let file = File::open("/proc/self/exe")?;
    #[cfg(not(target_os = "linux"))]
    let file = File::open(std::env::current_exe()?)?;
    #[cfg(target_os = "macos")]
    verify_running_inode(&file)?;
    Ok(file)
}

// Stable libproc ABI from sys/proc_info.h. libc already supplies the vnode
// structure; only proc_regioninfo's prefix is missing from its bindings.
// The real dyld startup regression compiles SDK sizeof/offsetof assertions.
#[cfg(target_os = "macos")]
#[repr(C)]
struct RegionInfo {
    _protection: [u32; 4],
    _offset: u64,
    _counters: [u32; 14],
    address: u64,
    size: u64,
}
#[cfg(target_os = "macos")]
#[repr(C)]
struct RegionWithPath {
    region: RegionInfo,
    vnode: libc::vnode_info_path,
}

#[cfg(target_os = "macos")]
fn verify_running_inode(file: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    // Stable dyld pointer ABI; no dependency on libc's retired Mach structs.
    unsafe extern "C" {
        fn _dyld_get_image_header(image_index: u32) -> *const u8;
    }
    // SAFETY: dyld owns image zero for the lifetime of this process. We only
    // use its address to ask the kernel about our OWN mapped executable.
    let loaded = unsafe { _dyld_get_image_header(0) };
    if loaded.is_null() {
        return Err(io::Error::other("loaded executable address unavailable"));
    }
    let address = loaded as u64;
    // SAFETY: these repr(C) structs contain only integers/arrays; libproc fills
    // this sized output buffer. The SDK regression verifies the missing prefix.
    let mut info: RegionWithPath = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<RegionWithPath>() as libc::c_int;
    let written = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            8, // PROC_PIDREGIONPATHINFO
            address,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if written != size {
        return Err(io::Error::other(format!(
            "cannot identify loaded executable vnode: {}",
            io::Error::last_os_error()
        )));
    }
    let metadata = file.metadata()?;
    let stat = &info.vnode.vip_vi.vi_stat;
    if address < info.region.address
        || address - info.region.address >= info.region.size
        || metadata.ino() != stat.vst_ino
        || metadata.dev() as u32 != stat.vst_dev as u32
    {
        return Err(io::Error::other(
            "executable file does not match running image; retry the installed binary",
        ));
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn same_build_copy_is_not_the_executing_inode() {
        let file = tempfile::NamedTempFile::new().unwrap();
        // Even byte-identical copies preserve the Mach-O UUID but have a
        // different vnode; a build-ID-only check would wrongly accept them.
        std::fs::copy(std::env::current_exe().unwrap(), file.path()).unwrap();
        assert!(super::verify_running_inode(file.as_file()).is_err());
    }
    #[test]
    fn current_loaded_image_matches_its_open_descriptor() {
        assert_eq!(std::mem::size_of::<super::RegionInfo>(), 96);
        assert_eq!(std::mem::offset_of!(super::RegionWithPath, vnode), 96);
        assert!(super::open_running().is_ok());
    }
}
