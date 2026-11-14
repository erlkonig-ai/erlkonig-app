//! Native workspace grants; authority remains collection-local.
//!
//! Sharing publishes exact native grant evidence. Peer exchange and repair
//! deliver proofs and descriptors; recipient workspace enrollment stays local.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use triblespace::core::blob::encodings::UnknownBlob;
use triblespace::core::capability::{capability_action, capability_delegate_action};
use triblespace::core::collection::{
    ACTION_READ, ACTION_WRITE, AdmissionPolicy, Collection, CollectionGrantError,
    CollectionHandle, descriptor, grant_collection_capability,
};
use triblespace::core::metadata;
use triblespace::core::repo::memoryrepo::MemoryRepo;
use triblespace::core::repo::{
    BlobStoreGet, BlobStoreList, BlobStorePut, CapabilityProofRead, CapabilityProofStore,
    SnapshotSource, StorageFlush, transfer,
};
use triblespace::prelude::*;

use crate::node_store::{ConfigView, NodeStore};
use crate::workspace::Workspace;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    fn write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

pub(crate) struct Enrolled {
    pub workspace: Workspace,
    pub source: Id,
    pub write: bool,
    pub delegate: bool,
    pub already_enrolled: bool,
    /// An enrolled workspace gained routes in a new revision.
    pub extended: bool,
}

pub(crate) fn public_identity(value: &str) -> Result<VerifyingKey> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(value, &mut bytes).context("recipient must be an exact public node ID")?;
    let key = VerifyingKey::from_bytes(&bytes).context("invalid recipient public node ID")?;
    AdmissionPolicy::quorum([key], 1, None)
        .context("recipient must be a canonical non-weak public node ID")?;
    Ok(key)
}

/// Stage this exact descriptor, its name and resident admission definitions
/// needed to prepare native grants.
fn root_material<S: BlobStoreGet + BlobStoreList>(
    source: &S,
    staging: &mut MemoryRepo,
    root: CollectionHandle,
) -> Result<BTreeSet<Inline<inlineencodings::Handle<UnknownBlob>>>> {
    let facts: TribleSet = source
        .get(root)
        .context("workspace root descriptor unavailable")?;
    let mut handles = BTreeSet::from([root.transmute()]);
    for (handle, _) in descriptor::capability_policies(&facts, None) {
        if source.contains_blob(handle)? {
            let definition: TribleSet = source.get(handle)?;
            if exists!(pattern!(&definition, [{ capability_action: ACTION_READ }]))
                || exists!(pattern!(&definition, [{ capability_action: ACTION_WRITE }]))
            {
                handles.insert(handle.transmute());
            }
        }
    }
    for name in find!(name: Inline<inlineencodings::Handle<blobencodings::UTF8String>>, pattern!(&facts, [
        { metadata::tag: triblespace::core::collection::records::KIND_COLLECTION_DESCRIPTOR,
            triblespace::core::collection::records::collection_name: ?name },
    ])) {
        if source.contains_blob(name)? {
            handles.insert(name.transmute());
        }
    }
    for copied in transfer(source, staging, handles.iter().copied()) {
        copied.context("copy workspace root admission material")?;
    }
    Collection::<blobencodings::SimpleArchive>::open(&staging.snapshot()?, root)?;
    Ok(handles)
}

fn admitted(
    staging: &mut MemoryRepo,
    workspace: &Workspace,
    subject: VerifyingKey,
    write: bool,
) -> Result<()> {
    let snapshot = staging.snapshot()?;
    for root in workspace.routes.values().copied().collect::<BTreeSet<_>>() {
        let collection = Collection::<blobencodings::SimpleArchive>::open(&snapshot, root)?;
        if !collection.reader_is_admitted(&snapshot, subject)?
            || (write && !collection.writer_is_admitted(&snapshot, subject)?)
        {
            bail!(
                "prepared grant does not provide the requested native admission for every workspace root"
            );
        }
    }
    Ok(())
}

impl NodeStore {
    pub(crate) fn share_workspace(
        &mut self,
        expected: &ConfigView,
        revision: Id,
        subject: VerifyingKey,
        access: Access,
        delegate: bool,
    ) -> Result<()> {
        if self.config_changed(expected)? {
            bail!("node configuration changed; inspect before sharing");
        }
        let workspace = expected
            .workspace(revision)?
            .context("workspace is no longer current; inspect before sharing")?;
        if workspace.routes.is_empty() {
            bail!("workspace has no collection routes to share");
        }
        let source = self.pile.snapshot()?;
        let signer = self.signer.verifying_key();
        let mut staging = MemoryRepo::default();
        let mut material = BTreeSet::new();
        let roots: BTreeSet<_> = workspace.routes.values().copied().collect();
        for root in &roots {
            material.extend(root_material(&source, &mut staging, *root)?);
        }
        let actions = if access.write() {
            vec![ACTION_READ, ACTION_WRITE]
        } else {
            vec![ACTION_READ]
        };
        // Existing proofs are queried only for the exact bundle and issuer.
        // Their definitions are brought in only when a path is tried.
        let parents = source
            .proofs()?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for root in &roots {
            for action in &actions {
                let definition = entity! {
                    capability_action: *action,
                    capability_delegate_action*: delegate.then_some(*action),
                };
                let handle =
                    staging.put::<blobencodings::SimpleArchive, _>(definition.facts().clone())?;
                material.insert(handle.transmute());
                match grant_collection_capability(
                    &mut staging,
                    *root,
                    handle,
                    &self.signer,
                    subject,
                ) {
                    Ok(_) | Err(CollectionGrantError::RootNotAuthorized { .. }) => {}
                    Err(CollectionGrantError::OpenPolicy { .. }) => continue,
                    Err(error) => {
                        return Err(error).context("prepare native collection grant");
                    }
                }
                // A configured root supplies only its own quorum share. It
                // may also hold independently rooted delegation paths needed
                // for the recipient to meet the native threshold.
                for parent in parents.iter().filter(|proof| {
                    proof.resource().into_bytes() == root.raw && proof.leaf_key() == signer
                }) {
                    let dependencies: Vec<_> = parent.blob_references().collect();
                    let mut resident = true;
                    for handle in &dependencies {
                        resident &= source.contains_blob(*handle)?;
                    }
                    if !resident {
                        continue;
                    }
                    for copied in transfer(&source, &mut staging, dependencies.iter().copied()) {
                        copied.context("load delegated grant definitions")?;
                    }
                    let Ok(child) = parent.delegate(&self.signer, handle, subject) else {
                        continue;
                    };
                    let snapshot = staging.snapshot()?;
                    if child.validate_delegation(&snapshot).is_ok()
                        && descriptor::validate_proof_evidence(
                            *root,
                            &snapshot.get(*root)?,
                            &child,
                        )
                        .is_ok()
                    {
                        staging.insert_proof(child)?;
                        material.extend(dependencies);
                    }
                }
            }
        }
        admitted(&mut staging, &workspace, subject, access.write())?;
        for role in workspace.routes.keys() {
            crate::workspace::role_name(*role)
                .context("workspace has an unknown faculty role")?;
        }
        // No fallible permission validation remains. Proof records are
        // deterministic native values; repeat issue is a set no-op. This is not
        // a multi-record transaction: an I/O failure may retain earlier evidence.
        let authority = staging.snapshot()?;
        for copied in transfer(&authority, &mut self.pile, material) {
            copied.context("publish native grant admission material")?;
        }
        for proof in authority.proofs()? {
            self.pile.insert_proof(proof?)?;
        }
        self.pile.flush()?;
        Ok(())
    }

}
