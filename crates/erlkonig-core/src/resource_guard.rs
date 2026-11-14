//! The app's bounded service envelope, sampled by its existing controller.
//! No external watchdog, subprocess or owner is started.
use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure {
    FootprintLimit,
    HeadroomLimit,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(target_os = "macos")]
            Self::FootprintLimit => "service resource guard: process physical footprint reached 48 GiB; services stopped. After recovery, use Retry services.",
            #[cfg(not(target_os = "macos"))]
            Self::FootprintLimit => "service resource guard: process memory footprint reached 48 GiB (anonymous, shared and swapped pages); services stopped. After recovery, use Retry services.",
            Self::HeadroomLimit => "service resource guard: kernel memory headroom below 20%; services stopped. After recovery, use Retry services.",
        })
    }
}
impl std::error::Error for Failure {}

// The platform sampler supplies the process's own footprint: Darwin's
// physical footprint, or anonymous, shared and swapped pages on Linux. Clean
// file-backed pages (a memory-mapped pile) are reclaimable and count on neither.
pub fn check(process_memory: u64, headroom: u32) -> Result<()> {
    if process_memory >= 48 * 1024 * 1024 * 1024 {
        return Err(Failure::FootprintLimit.into());
    }
    if headroom > 100 {
        bail!("service resource guard: kernel memory headroom is outside its expected 0–100 range");
    }
    if headroom < 20 {
        return Err(Failure::HeadroomLimit.into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn process_usage(pid: i32) -> Result<libc::rusage_info_v0> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v0>::zeroed();
    // SAFETY: RUSAGE_INFO_V0 writes the SDK-defined rusage_info_v0 buffer.
    // libproc's pointer typedef is historical: this is the buffer itself,
    // not a pointer that libproc dereferences. Read it only on 0 (success).
    let result = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V0,
            usage.as_mut_ptr().cast(),
        )
    };
    if result != 0 {
        bail!("service resource guard cannot sample process physical footprint");
    }
    Ok(unsafe { usage.assume_init() })
}

#[cfg(target_os = "macos")]
fn check_usage(usage: &libc::rusage_info_v0, headroom: u32) -> Result<()> {
    // A live app cannot have no physical footprint. Never substitute RSS or
    // accept an unavailable observation as a successful guard sample.
    if usage.ri_phys_footprint == 0 {
        bail!("service resource guard obtained an invalid process physical footprint");
    }
    check(usage.ri_phys_footprint, headroom)
}

#[cfg(target_os = "macos")]
pub fn sample() -> Result<()> {
    let usage = process_usage(std::process::id() as i32)?;
    let mut level = 0_u32;
    let mut len = std::mem::size_of::<u32>();
    // SAFETY: sysctl receives a correctly sized writable u32 buffer.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_level".as_ptr(),
            (&mut level as *mut u32).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || len != std::mem::size_of::<u32>() {
        bail!("service resource guard cannot sample kernel headroom");
    }
    check_usage(&usage, level)
}

/// The process's own footprint from one `/proc/self/status` text, in bytes:
/// `RssAnon + RssShmem + VmSwap`. `VmRSS` also counts `RssFile`, the clean
/// file-backed pages of a memory-mapped pile, which the kernel reclaims
/// under pressure; Darwin's physical footprint excludes them likewise.
/// All three fields must be present (Linux 4.5 and later).
#[cfg(not(target_os = "macos"))]
fn process_footprint(status: &str) -> Option<u64> {
    let field = |key: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|line| line.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
    };
    Some((field("RssAnon:")? + field("RssShmem:")? + field("VmSwap:")?) * 1024)
}

#[cfg(not(target_os = "macos"))]
pub fn sample() -> Result<()> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let footprint = process_footprint(&status)
        .ok_or_else(|| anyhow::anyhow!("resource guard cannot sample process memory footprint"))?;
    let mem = std::fs::read_to_string("/proc/meminfo")?;
    let value = |key: &str| {
        mem.lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|line| line.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let (Some(total), Some(available)) = (value("MemTotal:"), value("MemAvailable:")) else {
        bail!("resource guard cannot sample host headroom");
    };
    check(
        footprint,
        (available.saturating_mul(100) / total.max(1)) as u32,
    )
}

/// Thread-only background scheduling leaves the native UI alone. On Darwin
/// this is the thread background policy. On Linux the nice value is per
/// thread and `who = 0` with `PRIO_PROCESS` names the calling thread
/// (setpriority(2), BUGS), so nice 10 reaches only the worker that calls it.
pub fn background() {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::setpriority(libc::PRIO_DARWIN_THREAD, 0, libc::PRIO_DARWIN_BG);
    }
    #[cfg(target_os = "linux")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn thresholds_fail_closed_without_latching_success() {
        assert!(super::check(47 * 1024 * 1024 * 1024, 20).is_ok());
        assert!(super::check(48 * 1024 * 1024 * 1024, 80).is_err());
        assert!(super::check(1024, 19).is_err());
        assert!(super::check(1024, 101).is_err());
        assert!(super::check(1024, u32::MAX).is_err());
    }
    #[test]
    fn native_sampler_is_available() {
        super::sample().unwrap();
        #[cfg(target_os = "macos")]
        println!("rusage_v0_abi size={} align={} resident-offset={} footprint-offset={}",
            std::mem::size_of::<libc::rusage_info_v0>(),
            std::mem::align_of::<libc::rusage_info_v0>(),
            std::mem::offset_of!(libc::rusage_info_v0, ri_resident_size),
            std::mem::offset_of!(libc::rusage_info_v0, ri_phys_footprint));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_footprint_sums_anonymous_shared_and_swapped_pages_only() {
        // A node with a 60 GiB memory-mapped pile: the clean file-backed pages
        // are reclaimable cache, not the process's own footprint.
        let status = "Name:\terlkonig\nVmPeak:\t70000000 kB\nVmSize:\t69000000 kB\n\
            VmHWM:\t66000000 kB\nVmRSS:\t65000000 kB\nRssAnon:\t 600000 kB\n\
            RssFile:\t63000000 kB\nRssShmem:\t 300000 kB\nVmData:\t 900000 kB\n\
            VmSwap:\t 100000 kB\nThreads:\t12\n";
        assert_eq!(
            super::process_footprint(status),
            Some((600_000 + 300_000 + 100_000) * 1024)
        );
        assert_eq!(
            super::process_footprint("Name:\terlkonig\nVmRSS:\t1 kB\n"),
            None
        );
        assert_eq!(
            super::process_footprint("RssAnon:\t1 kB\nRssShmem:\t2 kB\nVmSwap:\t3 kB\n"),
            Some(6 * 1024)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn background_lowers_only_the_calling_thread() {
        // SAFETY: getpriority has no preconditions; errno is cleared first so a
        // legitimate -1 nice value is distinguishable from failure.
        let nice = || unsafe {
            *libc::__errno_location() = 0;
            let value = libc::getpriority(libc::PRIO_PROCESS, 0);
            assert_eq!(*libc::__errno_location(), 0);
            value
        };
        let before = nice();
        let worker = std::thread::spawn(move || {
            assert_eq!(nice(), before);
            super::background();
            nice()
        })
        .join()
        .unwrap();
        assert_eq!(worker, before.max(10));
        assert_eq!(nice(), before);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn clean_resident_memory_is_not_the_footprint_limit() {
        let mut usage: libc::rusage_info_v0 = unsafe { std::mem::zeroed() };
        usage.ri_resident_size = 64 * 1024 * 1024 * 1024;
        usage.ri_phys_footprint = 587 * 1024 * 1024;
        assert!(super::check_usage(&usage, 20).is_ok());
        usage.ri_phys_footprint = 47 * 1024 * 1024 * 1024;
        assert!(super::check_usage(&usage, 20).is_ok());
        usage.ri_phys_footprint = 48 * 1024 * 1024 * 1024;
        assert!(super::check_usage(&usage, 20).is_err());
        usage.ri_phys_footprint = 587 * 1024 * 1024;
        assert!(super::check_usage(&usage, 19).is_err());
        assert!(super::check_usage(&usage, 101).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn invalid_footprint_observations_fail_closed() {
        assert!(super::process_usage(i32::MAX).is_err());
        let usage: libc::rusage_info_v0 = unsafe { std::mem::zeroed() };
        assert!(super::check_usage(&usage, 100).is_err());
    }
}
