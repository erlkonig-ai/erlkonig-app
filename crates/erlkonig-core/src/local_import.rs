//! Local-only file and passphrase transport. Neither value is a tool argument.
use anyhow::{Result, bail};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub struct ImportRequest {
    pub workspace: String,
    pub workspace_label: String,
    pub previous_import: Option<String>,
}

/// Open once at local selection, never reopen a pathname after confirmation.
/// No Debug/Serialize: paths, plaintext and passphrases stay out of diagnostics.
pub struct ImportSource { file: crate::local_files::LocalFile }

impl ImportSource {
    pub fn open(path: &Path) -> Result<Self> {
        if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("1pux")) {
            bail!("Select a regular .1pux export; nothing was saved");
        }
        let file = crate::local_files::LocalFile::open(path)
            .map_err(|_| anyhow::anyhow!("Cannot open selected regular export; nothing was saved"))?;
        Ok(Self { file })
    }

    pub(crate) fn read(self, limit: usize) -> Result<Zeroizing<Vec<u8>>> {
        self.file.read(limit).map_err(|_| anyhow::anyhow!(
            "Selected export changed, is unreadable, or exceeds the import limit; nothing was saved"
        ))
    }
}

pub enum ImportAnswer {
    Submit { source: ImportSource, passphrase: Zeroizing<String> },
    Cancelled,
    Unavailable,
    Invalid,
}

pub struct ImportPrompt {
    pub request: ImportRequest,
    reply: Option<mpsc::SyncSender<ImportAnswer>>,
    closed: Arc<AtomicBool>,
    deadline: Instant,
}

impl ImportPrompt {
    pub fn is_cancelled(&self) -> bool { self.closed.load(Ordering::Acquire) || Instant::now() >= self.deadline }
    pub fn cancellation_check(&self) -> impl Fn() -> bool + Send + 'static {
        let closed = self.closed.clone();
        let deadline = self.deadline;
        move || closed.load(Ordering::Acquire) || Instant::now() >= deadline
    }
    pub fn answer(mut self, answer: ImportAnswer) {
        if let Some(reply) = self.reply.take() {
            let answer = if self.is_cancelled() { ImportAnswer::Cancelled } else { answer };
            let _ = reply.try_send(answer);
        }
    }
}
impl Drop for ImportPrompt {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() { let _ = reply.try_send(ImportAnswer::Cancelled); }
    }
}
#[derive(Default)]
struct State { pending: Option<ImportPrompt>, active: Option<mpsc::SyncSender<ImportAnswer>> }
#[derive(Clone, Default)]
pub struct LocalImport { state: Arc<Mutex<State>>, closed: Arc<AtomicBool> }

impl LocalImport {
    pub(crate) fn request(&self, request: ImportRequest) -> Result<ImportAnswer> {
        self.request_timeout(request, Duration::from_secs(120))
    }
    fn request_timeout(&self, request: ImportRequest, timeout: Duration) -> Result<ImportAnswer> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let deadline = Instant::now() + timeout;
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if self.closed.load(Ordering::Acquire) { return Ok(ImportAnswer::Unavailable); }
            if state.active.is_some() { bail!("local import is busy; nothing was saved"); }
            state.active = Some(reply.clone());
            state.pending = Some(ImportPrompt { request, reply: Some(reply), closed: self.closed.clone(), deadline });
        }
        let answer = receiver.recv_timeout(timeout).unwrap_or(ImportAnswer::Cancelled);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = None;
        state.pending.take();
        // Admission: all local UI has finished. Shutdown/expiry before this
        // boundary wins. Admitted work drains once, never automatically retries.
        if self.closed.load(Ordering::Acquire) || Instant::now() >= deadline {
            Ok(ImportAnswer::Cancelled)
        } else { Ok(answer) }
    }
    pub fn take_prompt(&self) -> Option<ImportPrompt> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).pending.take()
    }
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.closed.store(true, Ordering::Release);
        state.pending.take();
        if let Some(reply) = state.active.take() { let _ = reply.try_send(ImportAnswer::Cancelled); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> ImportRequest { ImportRequest { workspace: "synthetic".into(), workspace_label: "Test".into(), previous_import: None } }
    #[test]
    fn source_is_regular_bounded_unchanged_and_never_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("synthetic.1pux");
        std::fs::write(&path, b"synthetic").unwrap();
        assert!(ImportSource::open(&path).unwrap().read(8).is_err());
        assert!(ImportSource::open(dir.path()).is_err());
        let folder = dir.path().join("folder.1pux");
        std::fs::create_dir(&folder).unwrap();
        assert!(ImportSource::open(&folder).is_err());
        #[cfg(unix)] {
            let link = dir.path().join("symlink.1pux");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(ImportSource::open(&link).is_err());
        }
        let source = ImportSource::open(&path).unwrap();
        std::fs::write(&path, b"changed").unwrap();
        assert!(source.read(100).is_err());
        let source = ImportSource::open(&path).unwrap();
        std::fs::rename(&path, dir.path().join("moved.1pux")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        // Rename can change ctime: either reject or read the original inode,
        // but never import the replacement pathname's bytes.
        if let Ok(bytes) = source.read(100) { assert_eq!(&*bytes, b"changed"); }
        let wrong = dir.path().join("synthetic.zip");
        std::fs::write(&wrong, b"synthetic").unwrap();
        assert!(ImportSource::open(&wrong).is_err());
    }
    #[test]
    fn cancellation_expiry_and_unavailable_are_terminal() {
        let bridge = LocalImport::default();
        assert!(matches!(bridge.request_timeout(request(), Duration::ZERO).unwrap(), ImportAnswer::Cancelled));
        assert!(bridge.take_prompt().is_none());
        let worker = bridge.clone();
        let task = std::thread::spawn(move || worker.request(request()).unwrap());
        let deadline = Instant::now() + Duration::from_secs(2);
        let prompt = loop {
            if let Some(prompt) = bridge.take_prompt() { break prompt; }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert!(bridge.request(request()).is_err());
        bridge.close();
        assert!(prompt.is_cancelled());
        prompt.answer(ImportAnswer::Invalid);
        assert!(matches!(task.join().unwrap(), ImportAnswer::Cancelled));
        assert!(matches!(bridge.request(request()).unwrap(), ImportAnswer::Unavailable));
    }
    #[test]
    fn visible_prompt_expiry_rejects_late_answer() {
        let bridge = LocalImport::default();
        let worker = bridge.clone();
        let task = std::thread::spawn(move || worker.request_timeout(request(), Duration::from_millis(100)).unwrap());
        let deadline = Instant::now() + Duration::from_secs(2);
        let prompt = loop {
            if let Some(prompt) = bridge.take_prompt() { break prompt; }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert!(matches!(task.join().unwrap(), ImportAnswer::Cancelled));
        assert!(prompt.is_cancelled());
        prompt.answer(ImportAnswer::Invalid);
        assert!(bridge.take_prompt().is_none());
    }
}
