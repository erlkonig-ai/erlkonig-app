//! The app's bounded service envelope, sampled by its existing controller.
//! No external watchdog, subprocess or owner is started.
use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Failure { RssLimit, HeadroomLimit }

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RssLimit => "service resource guard: process RSS reached 48 GiB; services stopped. After recovery, use Retry services.",
            Self::HeadroomLimit => "service resource guard: kernel memory headroom below 20%; services stopped. After recovery, use Retry services.",
        })
    }
}
impl std::error::Error for Failure {}

pub fn check(rss: u64, headroom: u32) -> Result<()> {
    if rss >= 48 * 1024 * 1024 * 1024 {
        return Err(Failure::RssLimit.into());
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
pub fn sample() -> Result<()> {
    let mut task = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let len = std::mem::size_of::<libc::proc_taskinfo>();
    // SAFETY: the kernel receives correctly sized writable buffers; the
    // initialized values are read only after successful complete calls.
    let read = unsafe {
        libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDTASKINFO,
            0,
            task.as_mut_ptr().cast(),
            len as i32,
        )
    };
    if read != len as i32 {
        bail!("service resource guard cannot sample process memory");
    }
    let rss = unsafe { task.assume_init() }.pti_resident_size;
    let mut level = 0_u32;
    let mut len = std::mem::size_of::<u32>();
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
    check(rss, level)
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
    }
    #[test]
    fn native_sampler_is_available() {
        super::sample().unwrap();
    }
}
