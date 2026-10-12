//! The app's bounded service envelope, sampled by its existing controller.
//! No external watchdog, subprocess or owner is started.
use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure {
    #[cfg(not(target_os = "macos"))]
    RssLimit,
    #[cfg(target_os = "macos")]
    FootprintLimit,
    HeadroomLimit,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(not(target_os = "macos"))]
            Self::RssLimit => "service resource guard: process RSS reached 48 GiB; services stopped. After recovery, use Retry services.",
            #[cfg(target_os = "macos")]
            Self::FootprintLimit => "service resource guard: process physical footprint reached 48 GiB; services stopped. After recovery, use Retry services.",
            Self::HeadroomLimit => "service resource guard: kernel memory headroom below 20%; services stopped. After recovery, use Retry services.",
        })
    }
}
impl std::error::Error for Failure {}

// The platform sampler supplies physical footprint on Darwin and RSS elsewhere.
pub fn check(process_memory: u64, headroom: u32) -> Result<()> {
    if process_memory >= 48 * 1024 * 1024 * 1024 {
        #[cfg(not(target_os = "macos"))]
        return Err(Failure::RssLimit.into());
        #[cfg(target_os = "macos")]
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

#[cfg(not(target_os = "macos"))]
pub fn sample() -> Result<()> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let rss = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| anyhow::anyhow!("resource guard cannot sample process RSS"))?
        * 1024;
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
    check(rss, (available.saturating_mul(100) / total.max(1)) as u32)
}

/// Thread-only Darwin background scheduling leaves the native UI alone.
pub fn background() {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::setpriority(libc::PRIO_DARWIN_THREAD, 0, libc::PRIO_DARWIN_BG);
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
