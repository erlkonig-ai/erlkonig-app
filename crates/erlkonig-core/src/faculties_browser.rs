//! The viewer borrows the application's already-loaded NodeStore.
//! No path-based open, signer reload, peer, or independent storage lifetime.
use anyhow::{Result, bail};
use std::sync::{Arc, Mutex, Weak, atomic::{AtomicBool, Ordering}, TryLockError};
use std::time::Duration;
use triblespace::core::repo::pile::{Pile, PileSnapshot};
use triblespace::core::repo::{SnapshotSource, StoreChanges, StoreSnapshot};
use triblespace::core::collection::observed_store::DependencyTracker;
use crate::{node_store::ConfigView, runtime::SharedNode, workspace::Workspace};

#[derive(Clone)]
pub struct ViewerNode {
    pub(crate) node: SharedNode,
    pub(crate) shutdown: tokio::sync::watch::Receiver<bool>,
    pub(crate) interests: ViewerInterests,
}


// One window may subscribe to the already-running owner's refresh cycle. The
// slot is weak: neither the controller nor a late notification keeps it alive.
pub(crate) type ViewerInterests = Arc<Mutex<Option<Weak<ViewerInterest>>>>;

pub struct ViewerInterest {
    stop: Arc<AtomicBool>,
    state: Mutex<Option<ReadInterest>>,
    notify: Box<dyn Fn(u64, u64) + Send + Sync>,
}

struct ReadInterest {
    generation: u64,
    observation: u64,
    baseline: Option<PileSnapshot>,
    config: Option<ConfigView>,
    dependencies: DependencyTracker,
    owner_unavailable: bool,
    queued: bool,
}

impl ViewerInterest {
    /// Called under the owner borrow. The callback may only queue/flag work;
    /// it must not acquire the owner again or invoke UI code.
    pub(crate) fn check(&self, snapshot: Option<&PileSnapshot>, unavailable: bool) {
        if self.stop.load(Ordering::Acquire) { return; }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = state.as_mut() else { return; };
        if state.queued { return; }
        let changed = state.owner_unavailable != unavailable || match (snapshot, &state.baseline) {
            (Some(now), Some(before)) => {
                let reads = state.dependencies.lock().unwrap_or_else(|e| e.into_inner());
                now.changes_for(before, &reads) != StoreChanges::NONE
                    || state.config.as_ref().is_some_and(|view| !view.is_current(now))
            }
            (None, None) => false,
            _ => true,
        };
        if changed {
            state.queued = true;
            (self.notify)(state.generation, state.observation);
        }
    }
}

impl ViewerNode {
    pub fn is_stopping(&self) -> bool { *self.shutdown.borrow() }

    fn with_node<T>(
        &self,
        cancelled: impl Fn() -> bool,
        operation: impl FnOnce(&mut crate::node_store::NodeStore) -> Result<T>,
    ) -> Result<T> {
        loop {
            if self.is_stopping() || cancelled() { bail!("The viewer request was cancelled."); }
            match self.node.try_lock() {
                Ok(mut guard) => {
                    if self.is_stopping() || cancelled() { bail!("The viewer request was cancelled."); }
                    let node = guard.as_mut().ok_or_else(|| anyhow::anyhow!("Erlkonig is unavailable."))?;
                    return operation(node);
                }
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
                Err(TryLockError::Poisoned(_)) => bail!("Erlkonig’s local data is unavailable."),
            }
        }
    }

    pub fn watch(
        &self,
        stop: Arc<AtomicBool>,
        notify: impl Fn(u64, u64) + Send + Sync + 'static,
    ) -> Result<Arc<ViewerInterest>> {
        let interest = Arc::new(ViewerInterest {
            stop, state: Mutex::new(None), notify: Box::new(notify),
        });
        self.with_node(|| interest.stop.load(Ordering::Acquire), |_| {
            *self.interests.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&interest));
            Ok(())
        })?;
        Ok(interest)
    }

    pub fn snapshot(&self, cancelled: impl Fn() -> bool) -> Result<PileSnapshot> {
        self.with_node(cancelled, |node| Ok(node.pile.snapshot()?))
    }

    pub fn arm(
        &self,
        interest: &ViewerInterest,
        generation: u64,
        observation: u64,
        baseline: Option<PileSnapshot>,
        config: Option<ConfigView>,
        dependencies: DependencyTracker,
        cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        // Failed Config construction has no interpreted view to retain. Native
        // root comparisons conservatively detect its recovery, without scans.
        if config.is_none() {
            let mut reads = dependencies.lock().unwrap_or_else(|e| e.into_inner());
            reads.all_records = true;
            reads.all_blobs = true;
            reads.capability_proofs = true;
        }
        self.with_node(cancelled, |_| {
            let owner_unavailable = baseline.is_none() || config.is_none();
            *interest.state.lock().unwrap_or_else(|e| e.into_inner()) = Some(ReadInterest {
                generation, observation, baseline, config, dependencies, owner_unavailable, queued: false,
            });
            Ok(())
        })
    }

    pub fn config(&self, cancelled: impl Fn() -> bool) -> Result<ConfigView> {
        self.with_node(cancelled, |node| node.config())
    }

    /// The full selected value, not just its id, must still be current while
    /// the owner's borrow is held. Never call an external opener in this scope.
    pub fn with_workspace<T>(
        &self,
        selected: &Workspace,
        cancelled: impl Fn() -> bool,
        operation: impl FnOnce(&mut Pile, &ed25519_dalek::SigningKey) -> Result<T>,
    ) -> Result<T> {
        self.with_node(&cancelled, |node| {
            let current = node.config()?.workspace(selected.id)?;
            if current.as_ref() != Some(selected) { bail!("Workspace changed; choose it again."); }
            if self.is_stopping() || cancelled() { bail!("The viewer request was cancelled."); }
            operation(&mut node.pile, &node.signer)
        })
    }

    pub fn revalidate(&self, selected: &Workspace, cancelled: impl Fn() -> bool) -> Result<()> {
        self.with_workspace(selected, cancelled, |_, _| Ok(()))
    }
}

/// Cancellation/completion only. Runtime owns and joins the actual worker
/// before closing NodeStore; dropping a viewer never closes that store.
#[derive(Clone)]
pub struct ViewerWorker {
    pub(crate) stop: Arc<AtomicBool>,
    pub(crate) done: Arc<AtomicBool>,
}
impl ViewerWorker {
    pub fn stop(&self) { self.stop.store(true, Ordering::Release); }
    pub fn is_finished(&self) -> bool { self.done.load(Ordering::Acquire) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use crate::{config::Bootstrap, init, node_store::NodeStore};

    #[test]
    fn tracked_lazy_read_after_arrival_queues_once_and_rearms_without_idle_work() {
        use triblespace::core::repo::{BlobStoreGet, BlobStorePut, ObservedStore};
        use triblespace::prelude::*;
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let node = NodeStore::open(&bootstrap.pile, signer).unwrap();
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        let reader = ViewerNode { node: Arc::new(Mutex::new(Some(node))), shutdown: receiver, interests: Default::default() };
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, notifications) = std::sync::mpsc::channel();
        let interest = reader.watch(stop.clone(), move |generation, serial| {
            sender.send((generation, serial)).unwrap();
        }).unwrap();
        let baseline = reader.snapshot(|| false).unwrap();
        let tracker = DependencyTracker::default();
        let lazy = ObservedStore::with_tracker(baseline.clone(), tracker.clone());
        let config = reader.config(|| false).unwrap();
        reader.arm(&interest, 3, 7, Some(baseline), Some(config), tracker.clone(), || false).unwrap();
        let body: Blob<blobencodings::UTF8String> = "late widget body".to_blob();
        let handle = body.get_handle();
        reader.with_node(|| false, |node| {
            node.pile.put::<blobencodings::UTF8String, _>("unrelated payload")?;
            node.pile.put::<blobencodings::UTF8String, _>(body)?;
            Ok(())
        }).unwrap();
        let now = reader.snapshot(|| false).unwrap();
        interest.check(Some(&now), false);
        assert!(notifications.try_recv().is_err(), "unread payloads do not invalidate views");
        // The immutable reader misses even though bytes arrived before its
        // first lazy read. Its new interest must compare to the old baseline.
        assert!(lazy.clone().get::<Blob<blobencodings::UTF8String>, _>(handle).is_err());
        interest.check(Some(&now), false);
        interest.check(Some(&now), false);
        assert_eq!(notifications.try_recv().unwrap(), (3, 7));
        assert!(notifications.try_recv().is_err(), "bursts coalesce until rearmed");
        let config = reader.config(|| false).unwrap();
        reader.arm(&interest, 3, 8, Some(now.clone()), Some(config), tracker, || false).unwrap();
        interest.check(Some(&now), false);
        assert!(notifications.try_recv().is_err(), "fresh observation converges");
        interest.check(None, true);
        assert_eq!(notifications.try_recv().unwrap(), (3, 8));
        let config = reader.config(|| false).unwrap();
        reader.arm(&interest, 3, 9, None, Some(config), Default::default(), || false).unwrap();
        interest.check(None, true);
        assert!(notifications.try_recv().is_err(), "unchanged errors do not spin");
        interest.check(Some(&now), false);
        assert_eq!(notifications.try_recv().unwrap(), (3, 9));
        let config = reader.config(|| false).unwrap();
        reader.arm(&interest, 3, 10, Some(now), Some(config), Default::default(), || false).unwrap();
        stop.store(true, Ordering::Release);
        interest.check(None, true);
        assert!(notifications.try_recv().is_err(), "close suppresses work");
        drop(interest);
        assert!(reader.interests.lock().unwrap().as_ref().unwrap().upgrade().is_none());
        reader.node.lock().unwrap().take().unwrap().close().unwrap();
    }

    #[test]
    fn config_choices_invalidate_without_widget_reads() {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let node = NodeStore::open(&bootstrap.pile, signer).unwrap();
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        let reader = ViewerNode { node: Arc::new(Mutex::new(Some(node))), shutdown: receiver, interests: Default::default() };
        let (sender, notifications) = std::sync::mpsc::channel();
        let interest = reader.watch(Arc::new(AtomicBool::new(false)), move |generation, serial| {
            sender.send((generation, serial)).unwrap();
        }).unwrap();
        let config = reader.config(|| false).unwrap();
        config.workspace_ids().unwrap();
        let baseline = reader.snapshot(|| false).unwrap();
        reader.arm(&interest, 1, 1, Some(baseline), Some(config.clone()), Default::default(), || false).unwrap();
        reader.with_node(|| false, |node| node.create_workspace(&config, "New choice")).unwrap();
        interest.check(Some(&reader.snapshot(|| false).unwrap()), false);
        assert_eq!(notifications.try_recv().unwrap(), (1, 1));
        reader.node.lock().unwrap().take().unwrap().close().unwrap();
    }

    #[test]
    fn closed_owner_is_never_bootstrapped() {
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        let reader = ViewerNode { node: Arc::new(Mutex::new(None)), shutdown: receiver, interests: Default::default() };
        assert!(reader.config(|| false).is_err());
    }

    #[test]
    fn waiting_for_a_busy_owner_remains_cancellable() {
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        let shared = Arc::new(Mutex::new(None));
        let held = shared.lock().unwrap();
        let reader = ViewerNode { node: shared.clone(), shutdown: receiver, interests: Default::default() };
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert!(reader.config(|| calls.fetch_add(1, Ordering::Relaxed) > 0).is_err());
        assert!(calls.load(Ordering::Relaxed) >= 2);
        drop(held);
    }

    #[test]
    fn viewer_reuses_owner_and_revalidates_exact_choices_without_closing_it() {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let mut node = NodeStore::open(&bootstrap.pile, signer).unwrap();
        let view = node.config().unwrap();
        let first = node.create_workspace(&view, "Design").unwrap();
        let view = node.config().unwrap();
        let second = node.create_workspace(&view, "Design").unwrap();
        let shared = Arc::new(Mutex::new(Some(node)));
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        let reader = ViewerNode { node: shared.clone(), shutdown: receiver, interests: Default::default() };
        assert!(Arc::ptr_eq(&shared, &reader.node));
        let view = reader.config(|| false).unwrap();
        let selected = view.workspace(first).unwrap().unwrap();
        assert_eq!(view.workspace(second).unwrap().unwrap().label, selected.label);
        assert_ne!(first, second);
        reader.revalidate(&selected, || false).unwrap();
        let mut changed = selected.clone();
        changed.routes.clear();
        assert!(reader.revalidate(&changed, || false).is_err());
        assert!(reader.with_workspace::<()>(&selected, || true, |_, _| panic!("cancelled")).is_err());
        drop(reader);
        let mut guard = shared.lock().unwrap();
        let node = guard.as_mut().unwrap();
        assert!(node.config().unwrap().workspace(first).unwrap().is_some());
        guard.take().unwrap().close().unwrap();
    }
}
