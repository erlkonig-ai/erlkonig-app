//! Process questions answered without pattern matching: is this pid alive,
//! and which executable is it running.

use std::path::PathBuf;

pub fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks for existence and permission.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The executable a live process runs, resolved by the kernel.
#[cfg(target_os = "macos")]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let pid = i32::try_from(pid).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: buf is valid for buf.len() bytes.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(&buf[..n as usize])))
}

#[cfg(not(target_os = "macos"))]
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Sends `sig` to the process group led by `pid` when `pid` leads one, else
/// to `pid` alone.
pub fn signal_group(pid: u32, sig: i32) {
    let Ok(pid) = i32::try_from(pid) else { return };
    if pid <= 0 {
        return;
    }
    // SAFETY: plain syscalls on a positive pid.
    unsafe {
        if libc::getpgid(pid) == pid {
            libc::killpg(pid, sig);
        } else {
            libc::kill(pid, sig);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_alive_and_has_an_exe() {
        assert!(alive(std::process::id()));
        let exe = exe_path(std::process::id()).unwrap();
        assert_eq!(exe.canonicalize().unwrap(), std::env::current_exe().unwrap().canonicalize().unwrap());
        assert!(!alive(0));
    }
}
