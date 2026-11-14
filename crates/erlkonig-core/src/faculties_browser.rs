//! Read-only configuration access for the app-launched Faculties window.
//! No Runtime, bootstrap, descriptor publication, endpoint or maintenance owner.
use anyhow::{Result, bail};
use triblespace::core::collection::Collection;
use triblespace::core::repo::SnapshotSource;
use triblespace::core::repo::pile::{Pile, PileFile};

use crate::{config::NodeBinding, node_store::ConfigView, workspace::Workspace};

/// A native reader, retained for the window lifetime. Refresh consumes pile
/// deltas; it does not reopen/reindex the pile for every observation.
pub struct ConfigReader {
    binding: NodeBinding,
    pile: Pile,
    collection: Collection<triblespace::prelude::blobencodings::SimpleArchive>,
}

fn signer(binding: &NodeBinding) -> Result<ed25519_dalek::VerifyingKey> {
    let key = triblespace::core::signing_key_file::load_existing(&binding.key)
        .map_err(|_| anyhow::anyhow!("Erlkonig’s local data cannot be opened."))?;
    if key.verifying_key().to_bytes() != binding.signer {
        bail!("Erlkonig’s local data changed. Close this browser and reopen it from Erlkonig.");
    }
    Ok(key.verifying_key())
}

impl ConfigReader {
    /// Reuse the app's frozen locator binding; never infer a pile or identity.
    /// Errors omit key paths and native bearer handles from the window.
    pub fn open(binding: &NodeBinding) -> Result<Self> {
        if !binding.pile.is_absolute() || !binding.key.is_absolute() {
            bail!("Erlkonig’s local data cannot be opened.");
        }
        let public = signer(binding)?;
        let file = PileFile::open_read_only(&binding.pile)
            .map_err(|_| anyhow::anyhow!("The app's existing data is unavailable."))?;
        let mut pile = Pile::with_host(file, Some(public));
        let observed = pile
            .snapshot()
            .map_err(|_| anyhow::anyhow!("Erlkonig’s local data is unavailable."))?;
        let collection = Collection::open(&observed, faculties::config::handle(public))
            .map_err(|_| anyhow::anyhow!("Workspaces are unavailable on this device."))?;
        Ok(Self {
            binding: binding.clone(),
            pile,
            collection,
        })
    }

    pub fn config(&mut self) -> Result<ConfigView> {
        signer(&self.binding)?;
        self.pile
            .refresh()
            .map_err(|_| anyhow::anyhow!("Erlkonig’s local data is unavailable."))?;
        let observed = self
            .pile
            .snapshot()
            .map_err(|_| anyhow::anyhow!("Erlkonig’s local data is unavailable."))?;
        let view = ConfigView::from_snapshot(observed, self.collection)
            .map_err(|_| anyhow::anyhow!("Workspaces are unavailable on this device."))?;
        view.facts()
            .map_err(|_| anyhow::anyhow!("Workspaces are unavailable on this device."))?;
        Ok(view)
    }

    /// Same-ID annotations can change routes: compare the complete value.
    pub fn revalidate(&mut self, selected: &Workspace) -> Result<()> {
        let current = self.config()?.workspace(selected.id).map_err(|_| {
            anyhow::anyhow!("This workspace is unavailable. Choose a workspace again.")
        })?;
        if current.as_ref() != Some(selected) {
            bail!("Workspace changed; choose it again.");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Bootstrap, init, node_store::NodeStore};

    #[test]
    fn missing_node_is_never_bootstrapped() {
        let dir = tempfile::tempdir().unwrap();
        let binding = NodeBinding {
            pile: dir.path().join("missing.pile"),
            key: dir.path().join("missing.key"),
            signer: [0; 32],
        };
        assert!(ConfigReader::open(&binding).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn exact_choice_survives_duplicate_names_but_not_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let binding = NodeBinding {
            pile: bootstrap.pile.clone(),
            key: bootstrap.key,
            signer: signer.verifying_key().to_bytes(),
        };
        let mut node = NodeStore::open(&binding.pile, signer).unwrap();
        let view = node.config().unwrap();
        let first = node.create_workspace(&view, "Design").unwrap();
        let view = node.config().unwrap();
        let second = node.create_workspace(&view, "Design").unwrap();
        let before = std::fs::read(&binding.pile).unwrap();
        let mut reader = ConfigReader::open(&binding).unwrap();
        let view = reader.config().unwrap();
        let selected = view.workspace(first).unwrap().unwrap();
        assert_eq!(
            view.workspace(second).unwrap().unwrap().label,
            selected.label
        );
        assert_ne!(first, second);
        reader.revalidate(&selected).unwrap();
        let mut same_id_different_value = selected.clone();
        same_id_different_value.routes.clear();
        assert!(reader.revalidate(&same_id_different_value).is_err());
        assert_eq!(std::fs::read(&binding.pile).unwrap(), before);
        let mut changed = selected.clone();
        changed.label = "Revised design".into();
        let view = node.config().unwrap();
        node.put_workspace(&view, &changed).unwrap();
        assert!(reader.revalidate(&selected).is_err());
        node.close().unwrap();
        std::fs::write(&binding.key, b"unreadable replacement").unwrap();
        assert!(reader.config().is_err());
    }
}
