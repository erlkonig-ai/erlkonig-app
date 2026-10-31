//! Startup preparation before any store, network or maintenance owner opens.
//! The existing node locator is the activation point; the source is retained.

use crate::{
    config::{NodeBinding, NodeLocator},
    node_store::ConfigView,
    paths::Layout,
};
use anyhow::{Context, Result, bail, ensure};
use ed25519_dalek::SigningKey;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use trible::migration::{self, BranchDecision, Outcome, Requirement};
use triblespace::core::{
    collection::Collection,
    id::genid,
    repo::{
        BlobStoreList, SnapshotSource,
        pile::{Pile, PileFile},
    },
};

/// One pending native choice. The response belongs only to this startup.
pub struct StartupPrompt {
    pub title: String,
    pub details: String,
    pub approve_label: String,
    pub decline_label: String,
    answer: mpsc::SyncSender<bool>,
}

impl StartupPrompt {
    pub fn answer(self, approved: bool) {
        let _ = self.answer.try_send(approved);
    }
}

struct StartupState {
    detail: String,
    notice: Option<String>,
    prompt: Option<StartupPrompt>,
}

/// Transient progress and one choice for the application's one startup task.
/// No migration ledger, index, or durable completion flag is kept here.
#[derive(Clone)]
pub struct StartupControl {
    cancelled: Arc<AtomicBool>,
    state: Arc<Mutex<StartupState>>,
    interactive: bool,
}

impl StartupControl {
    pub fn new(interactive: bool) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(StartupState {
                detail: "Inspecting local storage…".into(),
                notice: None,
                prompt: None,
            })),
            interactive,
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
    pub fn detail(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .detail
            .clone()
    }
    pub fn take_prompt(&self) -> Option<StartupPrompt> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .prompt
            .take()
    }
    pub(crate) fn notice(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .notice
            .clone()
    }
    pub(crate) fn check(&self) -> Result<()> {
        ensure!(
            !self.is_cancelled(),
            "startup cancelled; the original pile is retained"
        );
        Ok(())
    }
    pub(crate) fn progress(&self, detail: impl Into<String>) -> Result<()> {
        self.check()?;
        self.state.lock().unwrap_or_else(|e| e.into_inner()).detail = detail.into();
        Ok(())
    }

    fn migration_progress(&self, progress: migration::Progress) -> bool {
        let stage = match progress.stage {
            migration::Stage::Inspect => "Inspecting local storage",
            migration::Stage::HashSource => "Checking the original storage",
            migration::Stage::Reframe => "Preparing upgraded storage",
            migration::Stage::Describe => "Preparing storage descriptions",
            migration::Stage::Branch => "Importing the selected branch",
            migration::Stage::Ready => "Checking the prepared storage",
            migration::Stage::Canonical => "Updating signed storage records",
            migration::Stage::Audit => "Verifying updated storage records",
        };
        let detail = if progress.total_bytes == 0 {
            format!("{stage}…")
        } else {
            format!(
                "{stage}… {} of {} bytes",
                progress.completed_bytes, progress.total_bytes
            )
        };
        self.progress(detail).is_ok()
    }

    fn approve(
        &self,
        title: String,
        details: String,
        approve_label: &str,
        decline_label: &str,
    ) -> Result<()> {
        ensure!(
            self.interactive,
            "local data needs your approval to update; open Erlkonig to choose Migrate or Not now. Local services have not started and the original pile and node locator are unchanged"
        );
        self.check()?;
        let (answer, received) = mpsc::sync_channel(1);
        self.state.lock().unwrap_or_else(|e| e.into_inner()).prompt = Some(StartupPrompt {
            title,
            details,
            approve_label: approve_label.into(),
            decline_label: decline_label.into(),
            answer,
        });
        loop {
            self.check()?;
            match received.recv_timeout(Duration::from_millis(100)) {
                Ok(true) => return self.check(),
                Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => bail!(
                    "Local data update deferred. Local services are not running. The original pile and node locator are unchanged. Reopen Erlkonig when you are ready to migrate."
                ),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

fn config_view(path: &Path, signer: &SigningKey) -> Result<Option<ConfigView>> {
    let file = PileFile::open_read_only(path)?;
    let mut pile = Pile::with_host(file, Some(signer.verifying_key()));
    let snapshot = pile.snapshot()?;
    let handle = faculties::config::handle(signer.verifying_key());
    if snapshot.contains_blob(handle)? {
        let collection = Collection::open(&snapshot, handle)?;
        return Ok(Some(ConfigView::from_snapshot(snapshot, collection)?));
    }
    Ok(None)
}

/// Compare only the application's existing native readers, including their
/// unresolved outcomes. Conversion must not require cleaner data than normal
/// startup, choose a concurrent winner, or filter unknown application facts.
fn validate(source: &Path, candidate: &Path, signer: &SigningKey, canonical: bool) -> Result<()> {
    let before = config_view(source, signer)?;
    let after = config_view(candidate, signer)?;
    ensure!(
        before.is_none() || after.is_some(),
        "candidate lost the previously resident application configuration"
    );
    if let Some(after) = after {
        // Force the native content read. A signed /32 source cannot itself be
        // projected by this reader; its converter's signed-record audit owns
        // preservation, while these reads exercise the resulting app paths.
        after.facts()?;
        let settings = after.settings().map_err(|e| format!("{e:#}"));
        let revisions = after.workspace_ids().map_err(|e| format!("{e:#}"));
        if let Some(before) = before.filter(|_| !canonical) {
            ensure!(
                before.settings().map_err(|e| format!("{e:#}")) == settings,
                "candidate changed the application's settings read outcome"
            );
            ensure!(
                before.workspace_ids().map_err(|e| format!("{e:#}")) == revisions,
                "candidate changed the application's configured workspace revisions"
            );
            if let Ok(revisions) = revisions {
                for revision in revisions {
                    ensure!(
                        before.workspace(revision).map_err(|e| format!("{e:#}"))
                            == after.workspace(revision).map_err(|e| format!("{e:#}")),
                        "candidate changed the configured workspace {revision:X} read outcome"
                    );
                }
            }
        } else if let Ok(revisions) = revisions {
            for revision in revisions {
                // An unavailable/conflicting workspace remains a normal app
                // reconciliation outcome, not migration ineligibility.
                let _ = after.workspace(revision);
            }
        }
    }
    Ok(())
}

fn locator_bytes(layout: &Layout) -> Result<Option<Vec<u8>>> {
    match fs::read(layout.dir.join("node.json")) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read existing node locator"),
    }
}

/// Called under node.lock with the existing signing key already verified.
pub(crate) fn prepare(
    layout: &Layout,
    binding: NodeBinding,
    signer: &SigningKey,
    control: &StartupControl,
) -> Result<NodeBinding> {
    control.progress("Inspecting local storage…")?;
    let original_locator = locator_bytes(layout)?;
    if layout.uses_locator() {
        if let Some(bytes) = &original_locator {
            let locator: NodeLocator = serde_json::from_slice(bytes)?;
            ensure!(
                locator.binding()? == binding,
                "node locator changed before storage inspection; restart to use the new selection"
            );
        }
    }
    let original = fs::metadata(&binding.pile)?;
    let mut inspection =
        migration::inspect_with_progress(&binding.pile, &mut |p| control.migration_progress(p))?;
    control.check()?;
    if !inspection.unsupported.is_empty() {
        bail!(
            "This pile contains a storage format this Erlkonig cannot convert. Keep the original pile and use a release that understands that format before reopening Erlkonig. No replacement was activated."
        );
    }
    let canonical = inspection
        .requirements
        .iter()
        .any(|r| matches!(r, Requirement::Canonical { .. }));
    if inspection.current_framing && !canonical {
        return Ok(binding);
    }
    ensure!(
        layout.uses_locator(),
        "this explicitly fixed pile needs an upgrade; bind it through the application node locator before starting, so a validated replacement can be activated without overwriting the source"
    );
    control.progress("Waiting for your permission to update local data…")?;
    control.approve("Update local data?".into(), format!(
        "Your local data needs an update before Erlkonig can start. Erlkonig will create an updated copy and keep the original. Your existing identity and signing key will stay the same.\n\nOriginal: {}",
        binding.pile.display()
    ), "Migrate", "Not now")?;
    let filename = binding
        .pile
        .file_name()
        .context("node pile has no filename")?;
    let fresh_sibling = |phase: &str| {
        binding.pile.with_file_name(format!(
            "{}.{}-{:X}.pile",
            filename.to_string_lossy(),
            phase,
            genid().id
        ))
    };
    let mut prepared_source = binding.pile.clone();
    let mut retained_caches = 0;
    if canonical {
        let destination = fresh_sibling("canonical");
        control.progress("Updating signed storage records; the original remains intact…")?;
        match migration::prepare_canonical_with_progress(
            &binding.pile,
            &destination,
            signer,
            &[],
            &mut |p| control.migration_progress(p),
        )
        .with_context(|| {
            format!(
                "prepare signed-record candidate {}; original retained at {}",
                destination.display(),
                binding.pile.display()
            )
        })? {
            migration::CanonicalOutcome::Converted {
                destination: actual,
                retained_original_caches,
                ..
            } => {
                ensure!(
                    actual == destination,
                    "canonical converter returned a different destination"
                );
                prepared_source = destination;
                retained_caches = retained_original_caches;
            }
            migration::CanonicalOutcome::ForeignRefreshRequired { authors } => {
                let authors = authors
                    .iter()
                    .map(hex::encode)
                    .collect::<Vec<_>>()
                    .join("\n");
                bail!(
                    "Some saved records need refreshed signatures from their original authors. Ask those authors for exact canonical refreshes, apply them with trible-migrate canonical, and bind the validated result before restarting Erlkonig. This node cannot sign on their behalf. The original pile and locator are unchanged.\n\nAuthor public keys:\n{authors}"
                );
            }
        }
        control.check()?;
        inspection = migration::inspect_with_progress(&prepared_source, &mut |p| {
            control.migration_progress(p)
        })?;
        ensure!(
            inspection.unsupported.is_empty()
                && !inspection
                    .requirements
                    .iter()
                    .any(|r| matches!(r, Requirement::Canonical { .. })),
            "signed-record candidate still needs an unsupported conversion; no replacement activated"
        );
    }
    let (destination, imported_branches) = if inspection.current_framing {
        // Canonical conversion can retain already-current legacy pins. They
        // do not imply an unperformed import, and never cause another prompt.
        (prepared_source, 0)
    } else {
        let mut names = Vec::new();
        let branch_names = if inspection.legacy_branches.is_empty() {
            Default::default()
        } else {
            control.progress("Reading legacy branch labels…")?;
            migration::legacy_branch_names(&prepared_source)?
        };
        for branch in &inspection.legacy_branches {
            let name = branch_names
                .get(&format!("{branch:X}"))
                .cloned()
                .unwrap_or_else(|| format!("Imported branch {branch:X}"));
            control.progress("Waiting for your branch import choice…")?;
            control.approve(format!("Import {name}?"), format!(
                "Import this branch as the collection “{name}”?\n\nBranch: {branch:X}\nRead and write authority: this node ({})\n\nThe existing signing key will be used. The original pile remains intact. This imports a collection; it does not select a Faculty role, enroll it in a workspace, or recover application settings. Cancel if this node should not own the imported collection.",
                hex::encode(binding.signer)
            ), "Import with this node", "Cancel startup")?;
            names.push((*branch, name));
        }
        let decisions: Vec<_> = names
            .iter()
            .map(|(branch, name)| BranchDecision {
                branch: *branch,
                collection_name: name,
                signer,
                authority: signer.verifying_key(),
            })
            .collect();
        let destination = fresh_sibling("upgrade");
        control.progress("Preparing upgraded storage; the original remains intact…")?;
        let outcome = migration::prepare_with_progress(
            &prepared_source,
            &destination,
            &decisions,
            &mut |p| control.migration_progress(p),
        )
        .with_context(|| {
            format!(
                "prepare fresh candidate {}; original retained at {}",
                destination.display(),
                binding.pile.display()
            )
        })?;
        control.check()?;
        match outcome {
            Outcome::Converted {
                destination: actual,
                branches,
                ..
            } => {
                ensure!(
                    actual == destination,
                    "converter returned a different destination"
                );
                (destination, branches.len())
            }
            Outcome::Current { .. } => {
                bail!("source changed classification during startup; no replacement activated")
            }
            Outcome::ChoiceRequired { .. } => bail!(
                "storage still requires an explicit migration choice; original pile and locator retained"
            ),
            Outcome::Unsupported { .. } => {
                bail!("storage format is unsupported; original pile and locator retained")
            }
        }
    };
    control.progress("Checking upgraded settings and workspaces…")?;
    validate(&binding.pile, &destination, signer, canonical).with_context(|| {
        format!(
            "candidate {} was not activated; original retained at {}",
            destination.display(),
            binding.pile.display()
        )
    })?;
    let selected = NodeBinding {
        pile: destination,
        ..binding.clone()
    };
    let locator = NodeLocator {
        pile: selected.pile.clone(),
        key: selected.key.clone(),
        expected_signer: hex::encode(selected.signer),
    };
    locator.binding()?;
    ensure!(
        locator_bytes(layout)? == original_locator,
        "node locator changed during preparation; candidate was not activated"
    );
    let now = fs::metadata(&binding.pile)?;
    ensure!(
        original.dev() == now.dev()
            && original.ino() == now.ino()
            && original.len() == now.len()
            && original.modified()? == now.modified()?,
        "original pile changed during candidate validation; candidate was not activated"
    );
    ensure!(
        triblespace::core::signing_key_file::load_existing(&binding.key)?
            .verifying_key()
            .to_bytes()
            == binding.signer,
        "node signing identity changed during preparation; candidate was not activated"
    );
    control.check()?;
    crate::status::write_atomic(
        &layout.dir.join("node.json"),
        &serde_json::to_vec_pretty(&locator)?,
    )?;
    let mut notice = format!(
        "Storage upgraded. Original retained at {}. The node identity and existing key are unchanged.",
        binding.pile.display()
    );
    if imported_branches != 0 {
        notice.push_str(&format!(
            " {} legacy branches imported as collections; workspace setup may still be needed.",
            imported_branches
        ));
    }
    if retained_caches != 0 {
        notice.push_str(&format!(
            " {retained_caches} cached results remain in the original and may be recomputed."
        ));
    }
    control
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .notice = Some(notice);
    Ok(selected)
}

#[cfg(test)]
mod tests;
