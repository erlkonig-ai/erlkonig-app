//! A request-scoped local unlock, never a session-wide unlocked vault.
//!
//! The encrypted request is frozen by Faculties before its store lease is
//! released. This bridge has no store and cannot retry or change that request.
//! Transport disconnect currently does NOT cancel admitted synchronous calls;
//! shutdown and the monotonic deadline do cancel queued and visible prompts.

use anyhow::{Result, bail};
use faculties::secrets::protected::ProtectedAccess;
pub use faculties::secrets::protected::{Purpose, Unlocked};
pub type AccessResult = Result<Unlocked>;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}, mpsc};
use std::time::{Duration, Instant};

const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

pub fn unavailable() -> AccessResult { bail!("local protected access is unavailable") }

#[derive(Clone)]
pub struct Cancellation {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl Cancellation {
    fn new(timeout: Duration) -> Self {
        Self { cancelled: Arc::new(AtomicBool::new(false)), deadline: Instant::now() + timeout }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || Instant::now() >= self.deadline
    }

    pub fn cancel(&self) { self.cancelled.store(true, Ordering::Release); }
}

pub struct ProtectedPrompt {
    pub workspace: String,
    pub workspace_label: String,
    pub request: ProtectedAccess,
    pub cancellation: Cancellation,
    reply: Option<mpsc::SyncSender<Result<Unlocked>>>,
}

impl ProtectedPrompt {
    pub fn answer(mut self, answer: Result<Unlocked>) {
        if let Some(reply) = self.reply.take() {
            let answer = if self.cancellation.is_cancelled() {
                Err(anyhow::anyhow!("protected access cancelled or expired"))
            } else { answer };
            let _ = reply.try_send(answer);
        }
    }
}

impl Drop for ProtectedPrompt {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            self.cancellation.cancel();
            let _ = reply.try_send(Err(anyhow::anyhow!("protected access cancelled")));
        }
    }
}

struct Active {
    cancellation: Cancellation,
    reply: mpsc::SyncSender<Result<Unlocked>>,
}

#[derive(Default)]
struct State {
    pending: Option<ProtectedPrompt>,
    active: Option<Active>,
    closed: bool,
}

#[derive(Clone, Default)]
pub struct ProtectedAccessBridge { state: Arc<Mutex<State>> }

impl ProtectedAccessBridge {
    pub fn request(&self, workspace: String, workspace_label: String, request: ProtectedAccess) -> Result<Unlocked> {
        self.request_with_timeout(workspace, workspace_label, request, PROMPT_TIMEOUT)
    }

    fn request_with_timeout(&self, workspace: String, workspace_label: String, request: ProtectedAccess, timeout: Duration) -> Result<Unlocked> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let cancellation = Cancellation::new(timeout);
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed { bail!("local protected access is unavailable"); }
            if state.active.is_some() { bail!("local protected access is busy"); }
            state.active = Some(Active { cancellation: cancellation.clone(), reply: reply.clone() });
            state.pending = Some(ProtectedPrompt {
                workspace, workspace_label, request, cancellation: cancellation.clone(), reply: Some(reply),
            });
        }
        let answer = receiver.recv_timeout(timeout);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active.take();
        state.pending.take();
        // This is admission, not a reusable authorization grant. Faculties
        // subsequently reopens and verifies its exact sealed request.
        if cancellation.is_cancelled() || state.closed {
            cancellation.cancel();
            bail!("protected access cancelled or expired");
        }
        match answer {
            Ok(answer) => answer,
            Err(_) => {
                cancellation.cancel();
                bail!("protected access cancelled or expired")
            }
        }
    }

    pub fn take_prompt(&self) -> Option<ProtectedPrompt> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).pending.take()
    }

    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        if let Some(active) = state.active.take() {
            active.cancellation.cancel();
            let _ = active.reply.try_send(Err(anyhow::anyhow!("protected access cancelled")));
        }
        state.pending.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faculties::secrets::protected::{Protection, prepare_access, prepare_raw};
    use triblespace::{core::{collection::CollectionHandle, repo::SnapshotSource}, prelude::*};

    fn fixture() -> ProtectedAccess {
        let instant = hifitime::Epoch::from_tai_seconds(1.0);
        let sealed = prepare_raw("synthetic", b"synthetic private value", (instant, instant).try_to_inline().unwrap(),
            Protection { password: b"synthetic passphrase", device: None }).unwrap();
        let mut blobs = sealed.fragment.blobs().clone();
        let reader = blobs.snapshot().unwrap();
        prepare_access(&reader, sealed.fragment.facts(), CollectionHandle::new([8; 32]), sealed.secret, None, Purpose::Get)
            .unwrap().unwrap()
    }

    fn visible(bridge: &ProtectedAccessBridge) -> ProtectedPrompt {
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(prompt) = bridge.take_prompt() { return prompt; }
            assert!(Instant::now() < until, "prompt was not delivered");
            std::thread::yield_now();
        }
    }

    #[test]
    fn cancellation_discards_late_unlock_without_affecting_next_request() {
        let request = fixture();
        let late_unlock = request.unlock_password(b"synthetic passphrase").unwrap();
        let bridge = ProtectedAccessBridge::default();
        let worker_bridge = bridge.clone();
        let first = request.clone();
        let worker = std::thread::spawn(move || worker_bridge.request("workspace".into(), "Synthetic".into(), first));
        let old_prompt = visible(&bridge);
        assert!(bridge.request("other".into(), "Other".into(), request.clone()).is_err(), "busy must not replace the pending request");
        // Model cancellation waking the waiter while a native callback still
        // retains its prompt. No dependency on the scheduler winning a 50ms race.
        old_prompt.cancellation.cancel();
        assert!(old_prompt.reply.as_ref().unwrap().try_send(Err(anyhow::anyhow!("synthetic cancellation"))).is_ok());
        assert!(worker.join().unwrap().is_err());
        assert!(old_prompt.cancellation.is_cancelled());

        let worker_bridge = bridge.clone();
        let worker = std::thread::spawn(move || worker_bridge.request("workspace".into(), "Synthetic".into(), request));
        let prompt = visible(&bridge);
        old_prompt.answer(Ok(late_unlock));
        assert!(!prompt.cancellation.is_cancelled());
        let answer = prompt.request.unlock_password(b"synthetic passphrase");
        prompt.answer(answer);
        assert!(worker.join().unwrap().is_ok());
    }

    #[test]
    fn deadline_expires_queued_request_without_native_access() {
        let bridge = ProtectedAccessBridge::default();
        assert!(bridge.request_with_timeout("workspace".into(), "Synthetic".into(), fixture(), Duration::ZERO).is_err());
        assert!(bridge.take_prompt().is_none());
        assert!(bridge.state.lock().unwrap().active.is_none());
    }

    #[test]
    fn close_cancels_visible_prompt_and_drop_denies_without_unlocking() {
        let request = fixture();
        for close in [false, true] {
            let bridge = ProtectedAccessBridge::default();
            let worker_bridge = bridge.clone();
            let request = request.clone();
            let worker = std::thread::spawn(move || worker_bridge.request("workspace".into(), "Synthetic".into(), request));
            let prompt = visible(&bridge);
            if close {
                bridge.close();
                assert!(prompt.cancellation.is_cancelled());
            }
            drop(prompt);
            assert!(worker.join().unwrap().is_err());
        }
    }

    #[test]
    fn cancelled_request_discards_success() {
        let request = fixture();
        let answer = request.unlock_password(b"synthetic passphrase");
        let bridge = ProtectedAccessBridge::default();
        let worker_bridge = bridge.clone();
        let worker = std::thread::spawn(move || worker_bridge.request("workspace".into(), "Synthetic".into(), request));
        let prompt = visible(&bridge);
        prompt.cancellation.cancel();
        prompt.answer(answer);
        assert!(worker.join().unwrap().is_err());
    }

    #[test]
    fn cancellation_is_request_scoped_and_deadline_is_terminal() {
        let first = Cancellation::new(Duration::from_secs(1));
        let second = Cancellation::new(Duration::from_secs(1));
        first.clone().cancel();
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled());
        assert!(Cancellation::new(Duration::ZERO).is_cancelled());
    }

    #[test]
    fn close_marks_bridge_unavailable_without_a_prompt() {
        let bridge = ProtectedAccessBridge::default();
        bridge.close();
        assert!(bridge.state.lock().unwrap().closed);
        assert!(bridge.take_prompt().is_none());
    }
}
