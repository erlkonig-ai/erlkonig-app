//! A file picker and bounded passphrase modal; no export content reaches UI logs.
use block2::RcBlock;
use erlkonig_core::local_import::{ImportAnswer, ImportPrompt, ImportSource};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSModalPanelRunLoopMode, NSModalResponseOK, NSOpenPanel};
use objc2_foundation::{NSArray, NSRunLoop, NSString, NSTimer, NSURL};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

pub fn import(mtm: MainThreadMarker, prompt: &ImportPrompt, terminate: &Arc<AtomicBool>) -> ImportAnswer {
    import_with_source(mtm, prompt, terminate, None, |path| ImportSource::open(path).ok())
}

/// The synthetic trial passes a freshly opened fixture handle, only when the
/// selected path matches its allowlist. It never opens a user-selected file.
/// Production's wrapper above retains its ordinary local regular-file policy.
pub(crate) fn import_with_source(
    mtm: MainThreadMarker, prompt: &ImportPrompt, terminate: &Arc<AtomicBool>,
    directory: Option<&Path>, source: impl FnOnce(&Path) -> Option<ImportSource>,
) -> ImportAnswer {
    if prompt.is_cancelled() || terminate.load(Ordering::Relaxed) { return ImportAnswer::Cancelled; }
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setResolvesAliases(false);
    if let Some(directory) = directory {
        panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(&directory.to_string_lossy()))));
    }
    #[allow(deprecated)]
    panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[NSString::from_str("1pux")])));
    panel.setPrompt(Some(&NSString::from_str("Choose export")));
    panel.setMessage(Some(&NSString::from_str(&format!(
        "{} — workspace: {}\nID: {}\nChoose a local .1pux export. Next, confirm an app passphrase. No export content is sent to chat.",
        if prompt.request.previous_import.is_some() { "Verify previous protected import" } else { "New protected import" },
        crate::protected_access::bounded_label(&prompt.request.workspace_label),
        prompt.request.workspace,
    ))));
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    let cancelled = prompt.cancellation_check();
    let stopping = terminate.clone();
    let pending_panel = panel.clone();
    let check = RcBlock::new(move |_: std::ptr::NonNull<NSTimer>| {
        if cancelled() || stopping.load(Ordering::Relaxed) {
            unsafe { pending_panel.cancel(None); }
        }
    });
    let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.1, true, &check) };
    unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSModalPanelRunLoopMode); }
    let response = panel.runModal();
    timer.invalidate();
    panel.orderOut(None);
    if response != NSModalResponseOK || prompt.is_cancelled() || terminate.load(Ordering::Relaxed) {
        return ImportAnswer::Cancelled;
    }
    let Some(path) = panel.URL().and_then(|url| url.path()).map(|path| PathBuf::from(path.to_string())) else {
        return ImportAnswer::Invalid;
    };
    let Some(source) = source(&path) else { return ImportAnswer::Invalid; };
    drop(path); // The source is now the opened handle, not a pathname to reopen.
    let detail = format!(
        "Workspace: {}\nID: {}\n{}\n\nOriginal export, items, attachments and receipt: no ordinary node-key access. Older readable copies are NOT revoked.\n\nUse a long, unique app passphrase, not your Mac password. Keep it safe: no Touch ID or recovery is configured. Cancel saves nothing. After Save, import runs once and may take time.",
        crate::protected_access::bounded_label(&prompt.request.workspace_label),
        prompt.request.workspace,
        prompt.request.previous_import.as_ref().map(|id| format!("Verify receipt: {id}\nSame export and passphrase required."))
            .unwrap_or_else(|| "NEW import — existing copies are not deduplicated.".into()),
    );
    match crate::protected_access::passphrase(mtm, "Protect a local 1Password import", &detail, true, prompt.cancellation_check(), terminate) {
        Ok(passphrase) => ImportAnswer::Submit { source, passphrase },
        Err(_) => ImportAnswer::Cancelled,
    }
}
