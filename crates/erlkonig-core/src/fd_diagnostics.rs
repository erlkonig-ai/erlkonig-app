//! Bounded, best-effort evidence at the app's maintenance error handler.
//!
//! This is NOT an allocation/syscall hook: the upstream error is already a
//! String and temporary descriptors may have been released before this call.
//! Capture uses fixed storage, no file opens, formatting, node locks or callbacks.
//! Publication is exclusively the controller's deferred, ordinary metrics work.

use std::sync::{
    Mutex, MutexGuard, TryLockError,
    atomic::{AtomicU64, Ordering},
};
use triblespace::prelude::{Id, genid};

pub(crate) const CAPACITY: usize = 16;
pub(crate) const FD_CAPACITY: usize = 8192;
const SAMPLE_CAPACITY: usize = 60;
const SAMPLE_WINDOW_NS: u128 = 60_000_000_000;
pub(crate) const TYPE_NAMES: [&str; 12] = [
    "atalk",
    "vnode",
    "socket",
    "pshm",
    "psem",
    "kqueue",
    "pipe",
    "fsevents",
    "netpolicy",
    "channel",
    "nexus",
    "unknown",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Exhaustion {
    Process,
    System,
}

impl Exhaustion {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Process => "fd-limit-process",
            Self::System => "fd-limit-system",
        }
    }
}

// tempfile may wrap io::Error so raw_os_error() is None. Maintenance has
// already formatted the source chain. Recognize only fixed errno/messages;
// never retain the text. Inspect at most 8 KiB, including the causal suffix.
fn exhaustion(error: &str) -> Option<Exhaustion> {
    let bytes = error.as_bytes();
    let prefix = &bytes[..bytes.len().min(4096)];
    let suffix = &bytes[bytes.len().saturating_sub(4096)..];
    let has = |needle: &[u8]| {
        [prefix, suffix]
            .iter()
            .any(|part| part.windows(needle.len()).any(|window| window == needle))
    };
    if has(b"(os error 23)") || has(b"Too many open files in system") {
        Some(Exhaustion::System)
    } else if has(b"(os error 24)") || has(b"Too many open files") {
        Some(Exhaustion::Process)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Census {
    /// Returned entries only. At capacity this is a lower bound, not a total.
    pub count: u64,
    pub types: [u64; TYPE_NAMES.len()],
    pub complete: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub wall_unix_ns: Option<i128>,
    pub monotonic_ns: Option<u128>,
    pub limits: Option<(u64, u64)>,
    /// None means unavailable, not an empty table.
    pub census: Option<Census>,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FdInfo {
    fd: i32,
    kind: u32,
}

fn census_from_result(entries: &[FdInfo], returned_bytes: i32) -> Option<Census> {
    let width = std::mem::size_of::<FdInfo>();
    if returned_bytes <= 0
        || returned_bytes as usize % width != 0
        || returned_bytes as usize > std::mem::size_of_val(entries)
    {
        return None;
    }
    let count = returned_bytes as usize / width;
    let mut census = Census {
        count: count as u64,
        complete: count < entries.len(),
        ..Census::default()
    };
    for entry in &entries[..count] {
        let bucket = match entry.kind {
            0..=7 => entry.kind as usize,
            9..=11 => entry.kind as usize - 1,
            _ => 11,
        };
        census.types[bucket] += 1;
    }
    Some(census)
}

fn clock_ns(clock: libc::clockid_t) -> Option<i128> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid writable timespec; no descriptors or heap allocations.
    (unsafe { libc::clock_gettime(clock, &mut time) } == 0)
        .then_some(time.tv_sec as i128 * 1_000_000_000 + time.tv_nsec as i128)
}

impl Snapshot {
    pub(crate) fn capture() -> Self {
        let wall_unix_ns = clock_ns(libc::CLOCK_REALTIME);
        let monotonic_ns = clock_ns(libc::CLOCK_MONOTONIC).and_then(|n| n.try_into().ok());
        let mut limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit only writes this initialized stack object.
        let limits = (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } == 0)
            .then_some((limits.rlim_cur as u64, limits.rlim_max as u64));
        Self {
            wall_unix_ns,
            monotonic_ns,
            limits,
            census: self_census(),
        }
    }
}

#[cfg(target_os = "macos")]
fn self_census() -> Option<Census> {
    #[link(name = "proc")]
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    // proc_fdinfo and PROC_PIDLISTFDS from the macOS SDK sys/proc_info.h.
    // A single list call avoids opening /dev/fd and never requests paths.
    let mut entries = [FdInfo::default(); FD_CAPACITY];
    // SAFETY: C-compatible entries, correct byte capacity, own process only.
    let bytes = unsafe {
        proc_pidinfo(
            libc::getpid(),
            1,
            0,
            entries.as_mut_ptr().cast(),
            std::mem::size_of_val(&entries) as libc::c_int,
        )
    };
    census_from_result(&entries, bytes)
}

#[cfg(not(target_os = "macos"))]
fn self_census() -> Option<Census> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Occurrence {
    pub session: Id,
    pub sequence: u64,
    pub code: Exhaustion,
    pub handler: Snapshot,
    pub sampled_high_water: Option<Snapshot>,
    pub samples: u64,
    pub dropped: u64,
}

struct Queue {
    slots: [Option<Occurrence>; CAPACITY],
    len: usize,
    sequence: u64,
    samples: [Option<Snapshot>; SAMPLE_CAPACITY],
    next_sample: usize,
}

/// Immutable publication copy. Capture can continue while the writer uses it.
#[derive(Clone, Debug)]
pub(crate) struct Batch {
    pub session: Id,
    pub events: [Option<Occurrence>; CAPACITY],
    pub dropped: u64,
}

pub(crate) struct Diagnostics {
    session: Id,
    queue: Mutex<Queue>,
    dropped: AtomicU64,
    published_dropped: AtomicU64,
}

impl Diagnostics {
    /// Called during ordinary controller setup, before any capture.
    pub(crate) fn new() -> Self {
        Self {
            session: genid().id,
            queue: Mutex::new(Queue {
                slots: [None; CAPACITY],
                len: 0,
                sequence: 0,
                samples: [None; SAMPLE_CAPACITY],
                next_sample: 0,
            }),
            dropped: AtomicU64::new(0),
            published_dropped: AtomicU64::new(0),
        }
    }

    fn lose(&self) {
        let _ = self
            .dropped
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_add(1))
            });
    }

    fn try_queue(&self) -> Option<MutexGuard<'_, Queue>> {
        match self.queue.try_lock() {
            Ok(queue) => Some(queue),
            Err(TryLockError::WouldBlock) => None,
            // Every mutation below preserves the queue invariant without a
            // fallible call between fields. A prior unwinding reader must not
            // permanently conceal queued evidence or subsequent loss counts.
            Err(TryLockError::Poisoned(error)) => {
                self.queue.clear_poison();
                Some(error.into_inner())
            }
        }
    }

    pub(crate) fn maintenance_failure(&self, session: Id, error: &str) {
        let Some(code) = exhaustion(error) else {
            return;
        };
        self.capture_with(session, code, Snapshot::capture);
    }

    fn capture_with(&self, session: Id, code: Exhaustion, probe: impl FnOnce() -> Snapshot) {
        let Some(mut queue) = self.try_queue() else {
            self.lose();
            return;
        };
        if queue.len == CAPACITY || queue.sequence == u64::MAX {
            self.lose();
            return;
        }
        // Never call a publisher from here. The production probe only reads
        // raw process counters into stack memory and cannot reenter this queue.
        let handler = probe();
        let mut sampled_high_water: Option<Snapshot> = None;
        let mut samples = 0;
        for sample in queue.samples.iter().flatten() {
            let Some(age) = handler
                .monotonic_ns
                .zip(sample.monotonic_ns)
                .and_then(|(now, then)| now.checked_sub(then))
            else {
                continue;
            };
            let Some(census) = sample.census else {
                continue;
            };
            if age > SAMPLE_WINDOW_NS || !census.complete {
                continue;
            }
            samples += 1;
            if sampled_high_water
                .and_then(|s| s.census)
                .is_none_or(|old| census.count > old.count)
            {
                sampled_high_water = Some(*sample);
            }
        }
        queue.sequence += 1;
        let event = Occurrence {
            session,
            sequence: queue.sequence,
            code,
            handler,
            sampled_high_water,
            samples,
            dropped: self.dropped.load(Ordering::Relaxed),
        };
        let index = queue.len;
        queue.slots[index] = Some(event);
        queue.len += 1;
    }

    /// Best-effort controller sampling, nominally once per second. Store work
    /// can delay it arbitrarily. No durable record is generated by sampling.
    pub(crate) fn sample(&self) {
        self.sample_with(Snapshot::capture());
    }

    fn sample_with(&self, snapshot: Snapshot) {
        let Some(mut queue) = self.try_queue() else {
            return;
        };
        let index = queue.next_sample;
        queue.samples[index] = Some(snapshot);
        queue.next_sample = (index + 1) % SAMPLE_CAPACITY;
    }

    /// Called only by the single controller writer, outside the node lock.
    /// On error neither slots nor the loss acknowledgement advance. The same
    /// frozen occurrences (including clocks) will be offered on the next try.
    pub(crate) fn publish_pending<E>(
        &self,
        publish: impl FnOnce(&Batch) -> Result<(), E>,
    ) -> Result<(), E> {
        let batch = {
            let Some(queue) = self.try_queue() else {
                return Ok(());
            };
            let dropped = self.dropped.load(Ordering::Relaxed);
            if queue.len == 0 && dropped == self.published_dropped.load(Ordering::Relaxed) {
                return Ok(());
            }
            Batch {
                session: self.session,
                events: queue.slots,
                dropped,
            }
        };
        publish(&batch)?;
        // No node lock is held here. A contended acknowledgement is safe:
        // retry reasserts the same occurrence identities, it cannot lose them.
        if let Some(mut queue) = self.try_queue() {
            let count = batch.events.iter().flatten().count();
            queue.slots.copy_within(count.., 0);
            queue.slots[CAPACITY - count..].fill(None);
            queue.len -= count;
            self.published_dropped
                .store(batch.dropped, Ordering::Relaxed);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
