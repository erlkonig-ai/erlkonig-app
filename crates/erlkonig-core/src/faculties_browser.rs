//! The viewer borrows the application's already-loaded NodeStore.
//! No path-based open, signer reload, peer, or independent storage lifetime.
use anyhow::{Result, bail};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}, TryLockError};
use std::time::Duration;
use triblespace::core::repo::pile::Pile;
use crate::{node_store::ConfigView, runtime::SharedNode, workspace::Workspace};

#[derive(Clone)]
pub struct ViewerNode {
    pub(crate) node: SharedNode,
    pub(crate) shutdown: tokio::sync::watch::Receiver<bool>,
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
    fn closed_owner_is_never_bootstrapped() {
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        let reader = ViewerNode { node: Arc::new(Mutex::new(None)), shutdown: receiver };
        assert!(reader.config(|| false).is_err());
    }

    #[test]
    fn waiting_for_a_busy_owner_remains_cancellable() {
        let (_shutdown, receiver) = tokio::sync::watch::channel(false);
        let shared = Arc::new(Mutex::new(None));
        let held = shared.lock().unwrap();
        let reader = ViewerNode { node: shared.clone(), shutdown: receiver };
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
        let reader = ViewerNode { node: shared.clone(), shutdown: receiver };
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
