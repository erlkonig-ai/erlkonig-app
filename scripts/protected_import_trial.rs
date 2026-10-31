//! Opt-in synthetic-only native picker/passphrase acceptance. No live runtime.
use anyhow::{Result, bail};
use erlkonig_core::{config::NodeBinding, local_import::{ImportSource, LocalImport}, node_store::NodeStore, workspace_dispatch::WorkspaceDispatch};
use faculties::{mcp::{Bytes, Faculty}, out::{Out, Part}};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::NSString;
use serde_json::{Value, json};
use std::{fs, io::Write, path::{Path, PathBuf}, sync::{Arc, atomic::AtomicBool, mpsc}, thread, time::{Duration, Instant}};
use triblespace::core::signing_key_file;

const PHASES: [(&str, &str, &str); 5] = [
    ("import_picker_cancel", "1/5 — Cancel the file picker", "This trial uses only a fresh public synthetic export and throwaway store. No company credentials. In the next file picker, click Cancel."),
    ("import_password_cancel", "2/5 — Choose synthetic export; Cancel passphrase", "Choose PUBLIC-SYNTHETIC.1pux in the folder shown, then Cancel the passphrase dialog. Any other selection is rejected without being opened."),
    ("import_new", "3/5 — New synthetic import", "Choose PUBLIC-SYNTHETIC.1pux, then choose and confirm a NEW throwaway app passphrase. Remember it for step 5. Never enter a company/vault/Mac password. Only synthetic data will be saved."),
    ("import_wrong_passphrase", "4/5 — Verify using a WRONG passphrase", "Choose PUBLIC-SYNTHETIC.1pux. Enter and confirm a NONEMPTY DIFFERENT throwaway passphrase. This must fail verification without creating another import. Cancel is not this test."),
    ("import_verify", "5/5 — Verify using the CORRECT passphrase", "Choose PUBLIC-SYNTHETIC.1pux. Enter and confirm the throwaway passphrase from step 3. The receipt must be reused without another publication. Nothing is sent to a model."),
];

fn fixture(root: &Path) -> Result<PathBuf> {
    if !root.is_absolute() || root.exists()
        || std::env::var_os("HOME").map(PathBuf::from) != Some(root.join("home"))
        || std::env::var_os("TMPDIR").map(PathBuf::from) != Some(root.join("tmp")) {
        bail!("new isolated root and matching environment required");
    }
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(root)?;
    fs::DirBuilder::new().mode(0o700).create(root.join("home"))?;
    fs::DirBuilder::new().mode(0o700).create(root.join("tmp"))?;
    let path = root.join("PUBLIC-SYNTHETIC.1pux");
    let file = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    let mut zip = zip::ZipWriter::new(file);
    for (name, bytes) in [
        ("export.attributes", br#"{"version":3}"#.as_slice()),
        ("export.data", br#"{"accounts":[{"vaults":[{"items":[{"uuid":"public-synthetic-id","overview":{"title":"PUBLIC SYNTHETIC TITLE"},"details":{"notesPlain":"PUBLIC SYNTHETIC CONTENT: not a company credential"}}]}]}]}"#.as_slice()),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored))?;
        zip.write_all(bytes)?;
    }
    zip.finish()?.sync_all()?;
    // Resolve only the freshly created synthetic path, never a selection.
    Ok(path.canonicalize()?)
}

fn worker(root: PathBuf, bridge: LocalImport, start: mpsc::Receiver<()>, done: mpsc::Sender<bool>) -> Result<()> {
    let pile = root.join("synthetic.pile");
    let key = root.join("synthetic.key");
    fs::OpenOptions::new().write(true).create_new(true).open(&pile)?;
    let signer = signing_key_file::init(&key)?;
    let identity = signer.verifying_key().to_bytes();
    let mut node = NodeStore::open(&pile, signer)?;
    let view = node.config()?;
    let workspace = node.create_workspace(&view, "PUBLIC SYNTHETIC IMPORT TRIAL")?;
    let view = node.config()?;
    node.close()?;
    let dispatch = WorkspaceDispatch::new(NodeBinding { pile: pile.clone(), key, signer: identity }, move || Ok(view.clone()))?
        .with_local_import(bridge);
    let result = (|| {
        let mut receipt: Option<String> = None;
        for index in 0..PHASES.len() {
            start.recv().map_err(|_| anyhow::anyhow!("trial stopped"))?;
            let mut args = json!({"workspace":format!("{workspace:X}")});
            if index >= 3 { args["previous_import"] = json!(receipt.as_ref().ok_or_else(|| anyhow::anyhow!("missing trial receipt"))?); }
            let before = fs::metadata(&pile)?.len();
            let mut text = String::new();
            dispatch.call("secrets_import_1password_local", Bytes::from(serde_json::to_vec(&args)?), &mut Out::new(&mut |part| {
                if let Part::Text { text: value } = part { text.push_str(&value); }
                Ok(())
            }))?;
            let response: Value = serde_json::from_str(&text)?;
            let no_disclosure = !text.contains("PUBLIC SYNTHETIC TITLE") && !text.contains("PUBLIC SYNTHETIC CONTENT")
                && !text.contains("PUBLIC-SYNTHETIC.1pux") && !text.contains("passphrase\":");
            let unchanged = fs::metadata(&pile)?.len() == before;
            let passed = no_disclosure && match index {
                0 | 1 => response["status"] == "cancelled" && unchanged,
                2 => response["status"] == "saved" && response["protected"] == true && response["reused"] == false && response["counts"]["items"] == 1,
                3 => response["status"] == "not_saved" && unchanged,
                4 => response["status"] == "saved" && response["reused"] == true && response["receipt"] == json!(receipt) && unchanged,
                _ => false,
            };
            if index == 2 && passed { receipt = response["receipt"].as_str().map(str::to_owned); }
            done.send(passed).map_err(|_| anyhow::anyhow!("trial stopped"))?;
            if !passed { bail!("unexpected synthetic phase outcome"); }
        }
        Ok(())
    })();
    dispatch.finish(result)
}

pub fn interactive(root: &Path) -> Result<()> {
    let source = fixture(root)?;
    // Bound instructional dialogs too. Exit is a synthetic-only emergency
    // bound, not a rollback or a guarantee of destructor-based zeroization.
    thread::spawn(|| { thread::sleep(Duration::from_secs(900)); crate::trial::report("import_overall_deadline", false); std::process::exit(1); });
    let mtm = MainThreadMarker::new().ok_or_else(|| anyhow::anyhow!("main thread required"))?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let bridge = LocalImport::default();
    let (start, start_rx) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let root = root.to_owned();
    let worker_bridge = bridge.clone();
    let worker = thread::spawn(move || worker(root, worker_bridge, start_rx, done_tx));
    let terminate = Arc::new(AtomicBool::new(false));
    let result = (|| {
        for (phase, title, detail) in PHASES {
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(title));
            alert.setInformativeText(&NSString::from_str(detail));
            alert.addButtonWithTitle(&NSString::from_str("Continue"));
            alert.addButtonWithTitle(&NSString::from_str("Stop trial"));
            let response = alert.runModal();
            alert.window().orderOut(None);
            if response != NSAlertFirstButtonReturn { bail!("trial stopped"); }
            // Preopen ONLY our synthetic fixture. The selection callback below
            // never opens the selected pathname, even for an accidental real file.
            let opened = ImportSource::open(&source)?;
            start.send(()).map_err(|_| anyhow::anyhow!("trial worker stopped"))?;
            let deadline = Instant::now() + Duration::from_secs(10);
            let prompt = loop {
                if let Some(prompt) = bridge.take_prompt() { break prompt; }
                if worker.is_finished() || Instant::now() >= deadline { bail!("trial prompt unavailable"); }
                thread::sleep(Duration::from_millis(5));
            };
            let answer = crate::local_import::import_with_source(mtm, &prompt, &terminate, source.parent(), |selected| {
                (selected == source.as_path()).then_some(opened)
            });
            let expired = prompt.is_cancelled();
            prompt.answer(answer);
            let passed = done.recv_timeout(Duration::from_secs(120)).map_err(|_| anyhow::anyhow!("trial worker unavailable"))? && !expired;
            crate::trial::report(phase, passed);
            if !passed { bail!("unexpected synthetic phase outcome"); }
        }
        Ok(())
    })();
    bridge.close();
    drop(start);
    let worker_result = worker.join().map_err(|_| anyhow::anyhow!("trial worker stopped"))?;
    result?;
    worker_result?;
    crate::trial::report("import_complete", true);
    Ok(())
}
