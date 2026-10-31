//! One ephemeral local-entry rendezvous. No values enter MCP arguments, and
//! the UI receives presentation metadata, never a store/key or write handle.

use anyhow::{Result, bail};
use serde::Deserialize;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    #[default]
    Login,
    Secret,
}

#[derive(Clone, Debug)]
pub struct EntryRequest {
    pub workspace: String,
    pub workspace_label: String,
    pub service: String,
    pub name: String,
    pub kind: EntryKind,
}

/// Deliberately neither Debug nor Clone. AppKit may retain its own copies;
/// owned Rust buffers are cleared on drop, without promising total erasure.
pub struct EntryValue {
    username: Option<Zeroizing<String>>,
    value: Zeroizing<String>,
}

impl EntryValue {
    pub fn login(username: Zeroizing<String>, password: Zeroizing<String>) -> Self {
        Self {
            username: Some(username),
            value: password,
        }
    }

    pub fn secret(value: Zeroizing<String>) -> Self {
        Self {
            username: None,
            value,
        }
    }

    pub(crate) fn encode(&self, kind: EntryKind) -> Result<Zeroizing<Vec<u8>>> {
        match (kind, self.username.as_ref()) {
            (EntryKind::Login, Some(username)) => {
                // The owner calls this before Secrets::add. An invalid local
                // Save must not encode a payload or publish a secret version.
                if username.is_empty() || self.value.is_empty() {
                    bail!("Local login entry requires both username and password. No secret was saved. Request a new local entry and fill both fields.");
                }
                // Serialize borrowed fields: no serde_json::Value copies.
                #[derive(serde::Serialize)]
                struct Login<'a> {
                    username: &'a str,
                    password: &'a str,
                }
                Ok(Zeroizing::new(serde_json::to_vec(&Login {
                    username,
                    password: &self.value,
                })?))
            }
            (EntryKind::Secret, None) => {
                if self.value.is_empty() {
                    bail!("Local secret entry requires a nonempty value. No secret was saved. Request a new local entry.");
                }
                Ok(Zeroizing::new(self.value.as_bytes().to_vec()))
            }
            _ => bail!("local entry kind mismatch; no secret was saved"),
        }
    }
}

pub enum EntryAnswer {
    Submit(EntryValue),
    Cancelled,
    Unavailable,
}

pub struct SecretPrompt {
    pub request: EntryRequest,
    reply: Option<mpsc::SyncSender<EntryAnswer>>,
    closed: Arc<AtomicBool>,
}

impl SecretPrompt {
    pub fn is_cancelled(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn cancellation_check(&self) -> impl Fn() -> bool + Send + 'static {
        let closed = self.closed.clone();
        move || closed.load(Ordering::Acquire)
    }

    pub fn answer(mut self, answer: EntryAnswer) {
        if let Some(reply) = self.reply.take() {
            let answer = if self.is_cancelled() {
                EntryAnswer::Cancelled
            } else {
                answer
            };
            let _ = reply.try_send(answer);
        }
    }
}

impl Drop for SecretPrompt {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.try_send(EntryAnswer::Cancelled);
        }
    }
}

#[derive(Default)]
struct State {
    pending: Option<SecretPrompt>,
    active: Option<mpsc::SyncSender<EntryAnswer>>,
}

#[derive(Clone, Default)]
pub struct SecretEntry {
    state: Arc<Mutex<State>>,
    closed: Arc<AtomicBool>,
}

impl SecretEntry {
    pub(crate) fn request(&self, request: EntryRequest) -> Result<EntryAnswer> {
        let (reply, receiver) = mpsc::sync_channel(1);
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if self.closed.load(Ordering::Acquire) {
                return Ok(EntryAnswer::Unavailable);
            }
            if state.active.is_some() {
                bail!("local secret entry is busy; no new prompt or secret was created");
            }
            state.active = Some(reply.clone());
            state.pending = Some(SecretPrompt {
                request,
                reply: Some(reply),
                closed: self.closed.clone(),
            });
        }
        let answer = receiver.recv().unwrap_or(EntryAnswer::Cancelled);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = None;
        // This is the Save admission boundary. Closing before admission wins;
        // a Save admitted before shutdown is ordinary work that may drain.
        if self.closed.load(Ordering::Acquire) {
            Ok(EntryAnswer::Cancelled)
        } else {
            Ok(answer)
        }
    }

    /// Only the platform event-loop thread takes and presents this prompt.
    pub fn take_prompt(&self) -> Option<SecretPrompt> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .take()
    }

    /// Cancel pending AND already-visible entry before draining native calls.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.closed.store(true, Ordering::Release);
        state.pending.take();
        if let Some(reply) = state.active.take() {
            let _ = reply.try_send(EntryAnswer::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        thread,
        time::{Duration, Instant},
    };

    fn request() -> EntryRequest {
        EntryRequest {
            workspace: "synthetic workspace".into(),
            workspace_label: "Test".into(),
            service: "Synthetic service".into(),
            name: "fixture".into(),
            kind: EntryKind::Login,
        }
    }

    fn visible(bridge: &SecretEntry) -> SecretPrompt {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(prompt) = bridge.take_prompt() {
                return prompt;
            }
            assert!(Instant::now() < deadline, "prompt was not delivered");
            thread::yield_now();
        }
    }

    #[test]
    fn single_prompt_and_dropped_ui_cancel_without_values() {
        let bridge = SecretEntry::default();
        let worker_bridge = bridge.clone();
        let worker = thread::spawn(move || worker_bridge.request(request()).unwrap());
        let prompt = visible(&bridge);
        assert!(bridge.request(request()).is_err());
        drop(prompt);
        assert!(matches!(worker.join().unwrap(), EntryAnswer::Cancelled));
    }

    #[test]
    fn close_cancels_visible_prompt_and_rejects_a_late_save() {
        let bridge = SecretEntry::default();
        let worker_bridge = bridge.clone();
        let worker = thread::spawn(move || worker_bridge.request(request()).unwrap());
        let prompt = visible(&bridge);
        bridge.close();
        assert!(prompt.is_cancelled());
        prompt.answer(EntryAnswer::Submit(EntryValue::secret(Zeroizing::new(
            "invented".into(),
        ))));
        assert!(matches!(worker.join().unwrap(), EntryAnswer::Cancelled));
        assert!(matches!(
            bridge.request(request()).unwrap(),
            EntryAnswer::Unavailable
        ));
    }

    #[test]
    fn close_cancels_queued_prompt() {
        let bridge = SecretEntry::default();
        let worker_bridge = bridge.clone();
        let worker = thread::spawn(move || worker_bridge.request(request()).unwrap());
        let prompt = visible(&bridge);
        // Put it back to exercise the queued state without an AppKit object.
        bridge.state.lock().unwrap().pending = Some(prompt);
        bridge.close();
        assert!(bridge.take_prompt().is_none());
        assert!(matches!(worker.join().unwrap(), EntryAnswer::Cancelled));
    }
}
