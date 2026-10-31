//! Explicit synthetic-only trial; never launch the ordinary application.
//! --help and --self-test do not initialize AppKit. --interactive is opt-in.
//! No pile, configuration, keychain, listener, Runtime, plugins, or file I/O.
//! Only the fixed public fixture below is encrypted; entered passwords and
//! opened bytes are zeroizing and never printed. AppKit's own buffers have the
//! same limitations as the unchanged production passphrase implementation.

#[cfg(target_os = "macos")]
#[path = "../crates/erlkonig-mac/src/protected_access.rs"]
mod protected_access;

#[cfg(target_os = "macos")]
mod trial {
    use anyhow::{Result, bail};
    use ed25519_dalek::SigningKey;
    use erlkonig_core::protected_access::{AccessResult, ProtectedAccessBridge, ProtectedPrompt, Purpose};
    use faculties::secrets::{SealedVersion, open_version_from_facts, wrap_recipients};
    use faculties::secrets::protected::{ProtectedAccess, Protection, Unlocked, prepare_access, prepare_raw};
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::NSString;
    use std::{io::{self, Write}, sync::{Arc, atomic::AtomicBool}, thread, time::{Duration, Instant}};
    use triblespace::{core::{collection::CollectionHandle, repo::SnapshotSource}, prelude::*};

    const DUMMY: &[u8] = b"PUBLIC SYNTHETIC FIXTURE: not a company credential";
    const SELF_TEST_PASSWORD: &[u8] = b"public synthetic self-test passphrase";

    pub fn report(phase: &str, pass: bool) {
        // Callers supply fixed phase names only. Never format an error or value.
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{{\"phase\":\"{phase}\",\"pass\":{pass}}}");
        let _ = out.flush();
    }

    fn fixture(password: &[u8]) -> Result<SealedVersion> {
        let instant = hifitime::Epoch::from_tai_seconds(1.0);
        prepare_raw("PUBLIC SYNTHETIC DUMMY ONLY", DUMMY,
            (instant, instant).try_to_inline().map_err(|_| anyhow::anyhow!("invalid fixture interval"))?,
            Protection { password, device: None })
    }

    fn access(sealed: &SealedVersion) -> Result<ProtectedAccess> {
        let mut blobs = sealed.fragment.blobs().clone();
        let reader = blobs.snapshot()?;
        // Public, process-local fixture selector copied from the core tests.
        prepare_access(&reader, sealed.fragment.facts(), CollectionHandle::new([8; 32]),
            sealed.secret, None, Purpose::Get)?.ok_or_else(|| anyhow::anyhow!("missing fixture"))
    }

    fn node_denied(sealed: &SealedVersion) -> Result<()> {
        let mut blobs = sealed.fragment.blobs().clone();
        let reader = blobs.snapshot()?;
        let node = SigningKey::from_bytes(&[7; 32]); // PUBLIC synthetic key only.
        if !sealed.recipients.is_empty()
            || !wrap_recipients(sealed.fragment.facts(), sealed.secret).is_empty()
            || open_version_from_facts(&reader, sealed.fragment.facts(), sealed.secret, &node).is_ok()
        { bail!("unexpected node authority"); }
        Ok(())
    }

    fn verify_open(sealed: &SealedVersion, original: &ProtectedAccess, unlocked: Unlocked) -> Result<()> {
        unlocked.validate(original)?; // Includes original request nonce and purpose.
        let fresh = access(sealed)?;
        let mut blobs = sealed.fragment.blobs().clone();
        let reader = blobs.snapshot()?;
        let opened = unlocked.open_raw(&reader, &fresh)?; // Consumes one-use key.
        let matched = opened.as_slice() == DUMMY;
        drop(opened); // Zeroizing buffer; no returned or logged plaintext.
        if !matched { bail!("unexpected fixture"); }
        Ok(())
    }

    fn pending(bridge: &ProtectedAccessBridge, access: ProtectedAccess, label: &str)
        -> Result<(ProtectedPrompt, thread::JoinHandle<AccessResult>)>
    {
        let worker_bridge = bridge.clone();
        let label = label.to_owned();
        let worker = thread::spawn(move || worker_bridge.request("synthetic-only".into(), label, access));
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(prompt) = bridge.take_prompt() { return Ok((prompt, worker)); }
            if Instant::now() >= deadline {
                bridge.close();
                let _ = worker.join();
                bail!("prompt unavailable");
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[derive(Clone, Copy)]
    enum Expected { Cancel, Wrong, Open }

    fn matches(answer: &AccessResult, expected: Expected) -> bool {
        match (answer, expected) {
            (Ok(_), Expected::Open) => true,
            (Err(error), Expected::Cancel) => error.to_string() == "protected access cancelled or expired",
            (Err(error), Expected::Wrong) => error.to_string() == "protected secret authentication failed",
            _ => false,
        }
    }

    pub fn self_test() -> Result<()> {
        let sealed = fixture(SELF_TEST_PASSWORD)?;
        node_denied(&sealed)?;
        report("self_test_node_denied", true);
        let bridge = ProtectedAccessBridge::default();
        for (phase, expected) in [("self_test_cancel", Expected::Cancel),
            ("self_test_wrong", Expected::Wrong), ("self_test_open", Expected::Open),
            ("self_test_fresh_cancel", Expected::Cancel)]
        {
            let original = access(&sealed)?;
            let (prompt, worker) = pending(&bridge, original.clone(), phase)?;
            let answer = match expected {
                Expected::Cancel => Err(anyhow::anyhow!("protected access cancelled or expired")),
                Expected::Wrong => prompt.request.unlock_password(b"public wrong passphrase"),
                Expected::Open => prompt.request.unlock_password(SELF_TEST_PASSWORD),
            };
            let correct_kind = matches(&answer, expected);
            prompt.answer(answer);
            let answer = worker.join().map_err(|_| anyhow::anyhow!("worker stopped"))?;
            if !correct_kind || !matches(&answer, expected) { bail!("unexpected phase outcome"); }
            if let Ok(unlocked) = answer { verify_open(&sealed, &original, unlocked)?; }
            report(phase, true);
        }
        // Classification must not confuse arbitrary UI errors with Cancel or
        // wrong credentials, nor accept an unexpected successful disclosure.
        let other = Err(anyhow::anyhow!("unrelated failure"));
        if matches(&other, Expected::Cancel) || matches(&other, Expected::Wrong)
            || matches(&access(&sealed)?.unlock_password(SELF_TEST_PASSWORD), Expected::Cancel)
            || matches(&access(&sealed)?.unlock_password(b"public wrong passphrase"), Expected::Cancel)
        { bail!("classification failed"); }
        // A separately prepared nonce cannot validate the original unlock.
        let original = access(&sealed)?;
        if original.unlock_password(SELF_TEST_PASSWORD)?.validate(&access(&sealed)?).is_ok() {
            bail!("nonce binding failed");
        }
        bridge.close();
        report("self_test", true);
        Ok(())
    }

    fn instruction(mtm: MainThreadMarker, title: &str, detail: &str) -> Result<()> {
        // The interactive watchdog bounds this alert, including abandonment.
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(detail));
        alert.addButtonWithTitle(&NSString::from_str("Continue"));
        let stop = alert.addButtonWithTitle(&NSString::from_str("Stop trial"));
        stop.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        let response = alert.runModal();
        alert.window().orderOut(None);
        if response != NSAlertFirstButtonReturn { bail!("trial stopped"); }
        Ok(())
    }

    pub fn interactive() -> Result<()> {
        // One process-local deadline, not a watcher or retry loop. Even an
        // abandoned instructional alert exits the entire helper in ten minutes.
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(600));
            report("overall_deadline", false);
            std::process::exit(1);
        });
        let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("main thread required"))?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let terminate = Arc::new(AtomicBool::new(false));
        let setup_deadline = Instant::now() + Duration::from_secs(120);
        let password = super::protected_access::passphrase(mtm,
            "Synthetic trial — throwaway passphrase ONLY",
            "Choose a NEW throwaway passphrase and confirm it. NEVER enter a company credential, a real vault passphrase, or your Mac login password.\n\nOnly fixed public dummy bytes are encrypted in process memory. Nothing is saved; the fixture dies when this helper exits. The production button says ‘Protect and save’, but this helper has no persistence.\n\nNext: Cancel, wrong passphrase, correct passphrase, then a fresh request you Cancel. Generic MCP wording in the reused prompt refers only to this in-process test caller. Touch ID stays disabled. Each request expires after two minutes; the entire trial exits after ten minutes.",
            true, move || Instant::now() >= setup_deadline, &terminate)?;
        let sealed = fixture(password.as_bytes())?;
        drop(password);
        report("setup", true);
        node_denied(&sealed)?;
        report("node_denied", true);
        let bridge = ProtectedAccessBridge::default();
        for (phase, label, detail, expected) in [
            ("cancel", "1/4: CANCEL; do not enter a passphrase",
                "In the next protected request dialog, click Cancel without entering a passphrase. This must reveal nothing.", Expected::Cancel),
            ("wrong_passphrase", "2/4: Enter a WRONG nonempty throwaway passphrase",
                "In the next dialog, enter a NONEMPTY passphrase DIFFERENT from your new throwaway passphrase, then click Allow this request. Authentication must fail. Clicking Cancel is not this test and stops the trial.", Expected::Wrong),
            ("correct_passphrase", "3/4: Enter the CORRECT throwaway passphrase",
                "In the next dialog, enter the throwaway passphrase you chose at setup and click Allow this request. The helper checks the fixed dummy bytes in memory and immediately drops them; it prints no value.", Expected::Open),
            ("fresh_prompt_cancel", "4/4: FRESH read; CANCEL again",
                "A new request must prompt again after the successful read. In the next dialog, click Cancel without entering a passphrase. The earlier approval must not unlock this request.", Expected::Cancel),
        ] {
            instruction(mtm, label, detail)?;
            let original = access(&sealed)?;
            let (prompt, worker) = pending(&bridge, original.clone(), label)?;
            let answer = super::protected_access::unlock(mtm, &prompt, &terminate);
            // A timeout is not a deliberate Cancel. Never automatically retry.
            let correct_kind = !prompt.cancellation.is_cancelled() && matches(&answer, expected);
            prompt.answer(answer);
            let answer = worker.join().map_err(|_| anyhow::anyhow!("worker stopped"))?;
            let passed = correct_kind && matches(&answer, expected);
            if !passed { report(phase, false); bridge.close(); bail!("unexpected phase outcome"); }
            if let Ok(unlocked) = answer { verify_open(&sealed, &original, unlocked)?; }
            report(phase, true);
        }
        bridge.close();
        report("complete", true);
        Ok(())
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    {
        // Do not use the normal application's argument parser or initialization.
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        let result = match args.as_slice() {
            [arg] if arg == "--interactive" => trial::interactive(),
            [arg] if arg == "--self-test" => trial::self_test(),
            [] => { trial::report("help", true); return; }
            [arg] if arg == "--help" => { trial::report("help", true); return; }
            _ => { trial::report("arguments", false); std::process::exit(2); }
        };
        if result.is_err() {
            trial::report("stopped", false);
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        println!("{{\"phase\":\"unsupported_platform\",\"pass\":false}}");
        std::process::exit(1);
    }
}
