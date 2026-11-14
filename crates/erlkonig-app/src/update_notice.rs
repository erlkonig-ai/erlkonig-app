//! One optional advisory check per day while the native app is running.
//! CLI/MCP transports never construct this worker. It has no persistent state.

use erlkonig_core::update_catalog;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) struct UpdateNotice {
    next_check: Instant,
    worker: Option<JoinHandle<Option<String>>>,
    cancel: Arc<AtomicBool>,
    pub(crate) offered: Option<String>,
}

impl UpdateNotice {
    pub(crate) fn new() -> Self {
        Self {
            next_check: Instant::now() + FIRST_CHECK_DELAY,
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            offered: None,
        }
    }

    pub(crate) fn poll(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            self.offered = self
                .worker
                .take()
                .and_then(|worker| worker.join().ok())
                .flatten();
        }
        if self.cancel.load(Ordering::Relaxed)
            || self.worker.is_some()
            || Instant::now() < self.next_check
        {
            return;
        }
        self.next_check = Instant::now() + CHECK_INTERVAL;
        let Some(target) = update_catalog::target() else {
            return;
        };
        if update_catalog::stable_version(env!("CARGO_PKG_VERSION")).is_none() {
            return;
        }
        let cancel = self.cancel.clone();
        self.worker = std::thread::Builder::new()
            .name("erlkonig-update-check".into())
            .spawn(move || {
                let bytes = update_catalog::fetch(cancel)?;
                update_catalog::offered_version(&bytes, env!("CARGO_PKG_VERSION"), target)
            })
            .ok();
    }

    pub(crate) fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(|worker| worker.is_finished())
    }
}

impl Drop for UpdateNotice {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_is_delayed_and_cancellation_never_starts_a_worker() {
        let mut notice = UpdateNotice::new();
        notice.poll();
        assert!(notice.worker.is_none());
        assert!(notice.next_check > Instant::now());
        notice.cancel();
        notice.next_check = Instant::now();
        notice.poll();
        assert!(notice.is_finished());
        assert!(notice.offered.is_none());
    }

    #[test]
    fn finished_worker_updates_the_offer_without_another_request() {
        let mut notice = UpdateNotice::new();
        notice.worker = Some(std::thread::spawn(|| Some("0.2.0".to_owned())));
        while !notice.is_finished() {
            std::thread::yield_now();
        }
        notice.poll();
        assert_eq!(notice.offered.as_deref(), Some("0.2.0"));
        assert!(notice.worker.is_none());
        notice.poll();
        assert!(notice.worker.is_none());
    }
}
