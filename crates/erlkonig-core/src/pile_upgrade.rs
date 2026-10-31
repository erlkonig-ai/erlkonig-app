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
        pile::{Pile, PileFile, PileSnapshot},
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
    locator_published: bool,
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
                locator_published: false,
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
        if self.is_cancelled() {
            if self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .locator_published
            {
                bail!(
                    "startup cancelled after updated storage was selected; the new locator remains selected for next startup and the original pile is retained"
                );
            }
            bail!("startup cancelled before locator publication; the original pile is retained");
        }
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

fn config_snapshot(path: &Path, signer: &SigningKey) -> Result<PileSnapshot> {
    let file = PileFile::open_read_only(path)?;
    let mut pile = Pile::with_host(file, Some(signer.verifying_key()));
    Ok(pile.snapshot()?)
}

fn config_view(snapshot: PileSnapshot, signer: &SigningKey) -> Result<ConfigView> {
    let handle = faculties::config::handle(signer.verifying_key());
    let collection = Collection::open(&snapshot, handle)?;
    ConfigView::from_snapshot(snapshot, collection)
}

/// Compare the native reader outcomes, not a second model of configuration.
/// An unchanged unavailable observation is valid migration input; losing an
/// available observation is not. The ordinary runtime owns reconciliation.
fn compare_config_views(before: Result<ConfigView>, after: Result<ConfigView>) -> Result<()> {
    let (before, after) = match (before, after) {
        (Ok(before), Ok(after)) => (before, after),
        (Err(before), Err(after)) => {
            ensure!(
                format!("{before:#}") == format!("{after:#}"),
                "candidate changed the configuration acquisition failure"
            );
            return Ok(());
        }
        (Err(_), Ok(_)) => return Ok(()),
        (Ok(_), Err(error)) => {
            bail!("candidate made configuration acquisition unavailable: {error:#}")
        }
    };
    match (before.facts(), after.facts()) {
        (Ok(_), Ok(_)) => {}
        (Err(before), Err(after)) => {
            ensure!(
                format!("{before:#}") == format!("{after:#}"),
                "candidate changed the configuration payload failure"
            );
            return Ok(());
        }
        (Err(_), Ok(_)) => return Ok(()),
        (Ok(_), Err(error)) => bail!("candidate made configuration facts unavailable: {error:#}"),
    }
    ensure!(
        before.settings().map_err(|e| format!("{e:#}"))
            == after.settings().map_err(|e| format!("{e:#}")),
        "candidate changed the application's settings read outcome"
    );
    let revisions = after.workspace_ids().map_err(|e| format!("{e:#}"));
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
    Ok(())
}

/// Descriptor retention is independent of Config projection. In particular,
/// old signed /32 records cannot be projected by the current reader: their
/// strict canonical audit owns preservation, and unavailable candidate data
/// remains an ordinary runtime condition rather than a migration veto.
fn validate(source: &Path, candidate: &Path, signer: &SigningKey, canonical: bool) -> Result<()> {
    let before = config_snapshot(source, signer)?;
    let after = config_snapshot(candidate, signer)?;
    let handle = faculties::config::handle(signer.verifying_key());
    let had_config = before.contains_blob(handle)?;
    let has_config = after.contains_blob(handle)?;
    ensure!(
        !had_config || has_config,
        "candidate lost the previously resident application configuration"
    );
    if !has_config {
        return Ok(());
    }
    let after = config_view(after, signer);
    if had_config && !canonical {
        return compare_config_views(config_view(before, signer), after);
    }
    if let Ok(after) = after {
        // Exercise the same native paths; a missing payload or an unresolved
        // setting is reported by normal startup. This claims no data recovery.
        let _ = after.facts();
        let _ = after.settings();
        if let Ok(revisions) = after.workspace_ids() {
            for revision in revisions {
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

/// Rename is the activation commit point. The caller's custody checks and
/// cancellation are repeated after temp-file durability, just before rename.
/// This is cooperative checking, not exclusion of arbitrary external writers.
fn publish_locator(
    path: &Path,
    bytes: &[u8],
    control: &StartupControl,
    validate_custody: impl FnOnce() -> Result<()>,
) -> Result<()> {
    crate::status::write_atomic_checked(path, bytes, || {
        validate_custody().map_err(std::io::Error::other)?;
        control.check().map_err(std::io::Error::other)
    })?;
    control
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .locator_published = true;
    Ok(())
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
    let canonical = inspection
        .requirements
        .iter()
        .any(|r| matches!(r, Requirement::Canonical { .. }));
    // A bounded unfamiliar kind in current framing is native forward
    // compatibility. It only prevents a rewrite that cannot preserve it.
    if inspection.current_framing && !canonical {
        return Ok(binding);
    }
    if !inspection.unsupported.is_empty() {
        bail!(
            "This pile contains a storage format this Erlkonig cannot convert. Keep the original pile and use a release that understands that format before reopening Erlkonig. No replacement was activated."
        );
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
    control.check()?;
    publish_locator(
        &layout.dir.join("node.json"),
        &serde_json::to_vec_pretty(&locator)?,
        control,
        || {
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
            Ok(())
        },
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
