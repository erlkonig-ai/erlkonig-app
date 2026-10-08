//! Explicit query workspace bindings and user-triggered local discovery.
//!
//! Discovery projects names and admission from one read-only local snapshot.
//! It never opens a faculty, materializes collection contents, asks a peer,
//! registers descriptors, changes a pile, or starts maintenance/replication.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use triblespace::core::collection::{Collection, CollectionHandle, CollectionRead, descriptor};
use triblespace::core::repo::pile::{Pile, PileFile, PileSnapshot};
use triblespace::core::repo::{BlobStoreGet, SnapshotSource};
use triblespace::core::signing_key_file;
use triblespace::prelude::*;

use crate::node_store::ManagedPile;

/// Names label roles; they never resolve a collection by themselves.
pub fn roles() -> Vec<(Id, &'static str)> {
    faculties::collection_names::table()
}

/// One edit request / native-fact projection, not a catalog of the store.
/// `id` is the only request-facing ID. `anchor` exists for explicit UI edits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    pub id: Id,
    pub anchor: Id,
    pub label: String,
    pub managed_id: Id,
    pub pile: PathBuf,
    pub key: PathBuf,
    pub signer: [u8; 32],
    pub routes: BTreeMap<Id, CollectionHandle>,
}

impl Workspace {
    pub fn new(label: impl Into<String>, managed: &ManagedPile, signer: [u8; 32]) -> Self {
        let anchor = genid().id;
        Self {
            id: anchor,
            anchor,
            label: label.into(),
            managed_id: managed.id,
            pile: managed.pile.clone(),
            key: managed.key.clone(),
            signer,
            routes: BTreeMap::new(),
        }
    }
}

/// Display row from one explicit discovery/probe, not persisted configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootChoice {
    pub handle: CollectionHandle,
    pub name: String,
    pub read: bool,
    pub write: bool,
}

impl RootChoice {
    pub fn description(&self) -> String {
        format!(
            "{} · READ {} · WRITE {} · blake3:{}",
            self.name,
            if self.read { "yes" } else { "no" },
            if self.write { "yes" } else { "no" },
            hex::encode(self.handle.raw)
        )
    }
}

/// Keeps only the immutable source snapshot. Callers query it as needed.
pub struct Discovery {
    snapshot: PileSnapshot,
    signer: VerifyingKey,
}

impl Discovery {
    /// Explicit picker action only. The key is loaded, never created, and the
    /// data file is opened read-only, so even a future accidental write fails.
    pub fn open(managed: &ManagedPile) -> Result<Self> {
        let signer = signing_key_file::load_existing(&managed.key)
            .context("load selected workspace signing key")?
            .verifying_key();
        let file = PileFile::open_read_only(&managed.pile)
            .context("open selected workspace pile read-only")?;
        let mut pile = Pile::with_host(file, Some(signer));
        let snapshot = pile
            .snapshot()
            .context("freeze local workspace discovery")?;
        Ok(Self { snapshot, signer })
    }

    pub fn signer(&self) -> [u8; 32] {
        self.signer.to_bytes()
    }

    /// Indexed collection-record roots only. Descriptor-only roots have no
    /// collection record and cannot appear here: use `probe` for an exact
    /// handle. Unavailable/unrecognized roots do not become empty candidates.
    pub fn roots(&self) -> Result<(Vec<RootChoice>, Vec<String>)> {
        let mut roots = Vec::new();
        let mut unavailable = Vec::new();
        for handle in self.snapshot.collections()? {
            match self.probe(handle) {
                Ok(root) => roots.push(root),
                Err(error) => {
                    unavailable.push(format!("blake3:{}: {error:#}", hex::encode(handle.raw)))
                }
            }
        }
        roots.sort_by(|a, b| a.name.cmp(&b.name).then(a.handle.cmp(&b.handle)));
        roots.dedup_by_key(|root| root.handle);
        Ok((roots, unavailable))
    }

    /// Probe an exact descriptor already present locally; no full blob scan,
    /// hydration or name-based inference. READ and WRITE remain independent.
    pub fn probe(&self, handle: CollectionHandle) -> Result<RootChoice> {
        let collection = Collection::<blobencodings::SimpleArchive>::open(&self.snapshot, handle)
            .context("root descriptor unavailable or unsupported")?;
        let facts: TribleSet = self.snapshot.get(handle).context("read root descriptor")?;
        let name_handle = descriptor::name(&facts)?.context("descriptor is not a named root")?;
        let name: View<str> = self
            .snapshot
            .get(name_handle)
            .context("read descriptor name")?;
        Ok(RootChoice {
            handle,
            name: name.to_string(),
            read: collection
                .reader_is_admitted(&self.snapshot, self.signer)
                .context("check local READ evidence")?,
            write: collection
                .writer_is_admitted(&self.snapshot, self.signer)
                .context("check local WRITE evidence")?,
        })
    }

    /// A picker selection must name a root compatible with the selected role
    /// and usable by this signer. This does not confer missing permissions.
    pub fn select(&self, role: Id, handle: CollectionHandle) -> Result<RootChoice> {
        let expected =
            faculties::collection_names::name_for(role).context("unknown faculty role")?;
        let root = self.probe(handle)?;
        if root.name != expected {
            bail!(
                "role {expected:?} requires that exact descriptor name, not {:?}",
                root.name
            );
        }
        if !root.read && !root.write {
            bail!("selected signer has neither READ nor WRITE admission to this exact root");
        }
        Ok(root)
    }
}

#[cfg(test)]
mod tests;
