//! Explicit query workspace bindings and user-triggered local discovery.
//!
//! Discovery projects names and admission from one read-only local snapshot.
//! It never opens a faculty, materializes collection contents, asks a peer,
//! registers descriptors, changes a pile, or starts maintenance/replication.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use triblespace::core::collection::{Collection, CollectionHandle, descriptor};
use triblespace::core::metadata;
use triblespace::core::repo::pile::{Pile, PileFile, PileSnapshot};
use triblespace::core::repo::{BlobStoreGet, SnapshotSource, StoreRead};
use triblespace::core::signing_key_file;
use triblespace::prelude::*;

use crate::config::NodeBinding;

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
    pub routes: BTreeMap<Id, CollectionHandle>,
}

impl Workspace {
    pub fn new(label: impl Into<String>) -> Self {
        let anchor = genid().id;
        Self {
            id: anchor,
            anchor,
            label: label.into(),
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
    pub fn open(node: &NodeBinding) -> Result<Self> {
        let signer = signing_key_file::load_existing(&node.key)
            .context("load selected workspace signing key")?
            .verifying_key();
        if signer.to_bytes() != node.signer {
            bail!("node signing identity changed; restart only after an explicit identity handoff");
        }
        let file = PileFile::open_read_only(&node.pile)
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

    /// Record roots, invitation resources and exact saved workspace routes.
    /// Unavailable/unrecognized roots do not become empty candidates.
    pub fn roots(&self) -> Result<(Vec<RootChoice>, Vec<String>)> {
        let mut roots = Vec::new();
        let mut unavailable = Vec::new();
        let mut candidates = candidates(&self.snapshot, [])?;
        // Saved roles keep empty descriptor-only roots discoverable.
        let config = Collection::<blobencodings::SimpleArchive>::open(
            &self.snapshot,
            faculties::config::handle(self.signer),
        );
        use triblespace::core::collection::CollectionSnapshotExt;
        if let Ok(config) = config {
            if let Ok(observed) = self.snapshot.collection(config) {
                if let Ok(facts) = observed.view::<TribleSet>() {
                    candidates.extend(find!(root: CollectionHandle, pattern!(&facts, [
                        { _?workspace @ metadata::tag: crate::node_schema::WORKSPACE,
                            crate::node_schema::workspace_route: _?route },
                        { _?route @ faculties::schemas::config::config::selects: ?root },
                    ])));
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        for handle in candidates {
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
        faculties::collection_names::name_for(role).context("unknown faculty role")?;
        let root = self.probe(handle)?;
        if !root.read && !root.write {
            bail!("selected signer has neither READ nor WRITE admission to this exact root");
        }
        Ok(root)
    }
}

/// Discovery starts from record handles, explicit saved roots, and resources
/// mentioned by proofs. A proof is only a candidate, never an admission grant.
pub fn candidates<S: StoreRead>(
    snapshot: &S,
    extra: impl IntoIterator<Item = CollectionHandle>,
) -> Result<Vec<CollectionHandle>> {
    let mut values = snapshot.collections()?;
    values.extend(extra);
    for proof in snapshot.proofs()? {
        values.push(CollectionHandle::new(proof?.resource().into_bytes()));
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

/// Disposable projection of one frozen snapshot into an activation request.
/// It is recomputed on new snapshot evidence, not a store mirror.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AllAccessible {
    pub sync: Vec<CollectionHandle>,
    pub maintenance: Vec<CollectionHandle>,
    pub pending: Vec<String>,
    pub denied: Vec<CollectionHandle>,
}

impl AllAccessible {
    pub fn discover<S: StoreRead>(
        snapshot: &S,
        signer: VerifyingKey,
        extra: impl IntoIterator<Item = CollectionHandle>,
    ) -> Result<Self> {
        use triblespace::core::collection::{attached_to, collection_reader_is_admitted_by};
        let proofs = snapshot
            .proofs()?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut selected = Self::default();
        for handle in candidates(snapshot, extra)? {
            let result = (|| -> Result<Option<bool>> {
                let facts: TribleSet = snapshot.get(handle).context("descriptor unavailable")?;
                // Attachments have no READ/WRITE policy. Parent admission below
                // determines which host-local maintenance to schedule.
                if !descriptor::parents(&facts)?.is_empty() {
                    return Ok(None);
                }
                let admitted = collection_reader_is_admitted_by(snapshot, handle, signer, &proofs)?;
                if !admitted {
                    for proof in proofs
                        .iter()
                        .filter(|proof| proof.resource().into_bytes() == handle.raw)
                    {
                        for capability in proof.capabilities() {
                            if !snapshot.contains_blob(capability)? {
                                bail!("invitation capability metadata pending");
                            }
                        }
                    }
                }
                Ok(Some(admitted))
            })();
            match result {
                Ok(Some(true)) => {
                    selected.sync.push(handle);
                    selected.maintenance.push(handle);
                    for attached in attached_to(snapshot, handle)? {
                        selected.maintenance.push(attached.handle);
                    }
                }
                Ok(Some(false)) => selected.denied.push(handle),
                Ok(None) => {}
                Err(error) => selected
                    .pending
                    .push(format!("blake3:{}: {error:#}", hex::encode(handle.raw))),
            }
        }
        selected.maintenance.sort_unstable();
        selected.maintenance.dedup();
        Ok(selected)
    }
}

#[cfg(test)]
mod tests;
