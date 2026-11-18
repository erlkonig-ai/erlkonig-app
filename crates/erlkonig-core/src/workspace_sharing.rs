//! Native workspace grants; authority remains collection-local.
//!
//! Sharing publishes exact native grant evidence. Peer exchange and repair
//! deliver proofs and descriptors; recipient workspace enrollment stays local.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use triblespace::core::capability::{
    CapabilityProof, CapabilityRequest, QuorumOutcome, capability_action, capability_delegate_action,
};
use triblespace::core::collection::{
    ACTION_READ, ACTION_WRITE, AdmissionPolicy, Collection, CollectionHandle,
    collection_action_is_admitted_by_descriptor, descriptor,
};
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

/// Resident invocation support at one resource/action. Each alternative has
/// its own distinct trust roots. No invitation or vote counter is stored.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionAdmission {
    pub admitted: bool,
    alternatives: Vec<PolicySupport>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
enum PolicySupport {
    Open,
    Quorum {
        threshold: u32,
        have: usize,
        roots: Vec<String>,
        contributing_roots: Vec<String>,
    },
}

pub(crate) fn action_admission(
    reader: &impl BlobStoreGet,
    root: CollectionHandle,
    facts: &TribleSet,
    subject: VerifyingKey,
    action: Id,
    proofs: &[CapabilityProof],
) -> ActionAdmission {
    let representation = Some(blobencodings::SimpleArchive::id());
    let admission = collection_action_is_admitted_by_descriptor(
        reader, root, facts, representation, action, subject, proofs,
    );
    let admitted = matches!(admission, QuorumOutcome::Met);

    // A policy root supplies its own share. For every other contributor,
    // verify a signed prefix to this subject for this exact resource/action.
    let request = CapabilityRequest::new(root.into(), action);
    let mut support = BTreeSet::from([subject.to_bytes()]);
    for proof in proofs {
        let origin = proof.root_key();
        if !support.contains(origin.as_bytes())
            && proof.verify(reader, origin, subject, request).is_ok()
        {
            support.insert(origin.to_bytes());
        }
    }
    let alternatives = descriptor::admission_policies(reader, facts, action, representation)
        .map(|policy| match policy {
            AdmissionPolicy::Open => PolicySupport::Open,
            AdmissionPolicy::Quorum(quorum) => {
                let contributing_roots: Vec<_> = quorum.roots().iter()
                    .filter(|key| support.contains(key.as_bytes()))
                    .map(|key| hex::encode(key.as_bytes()))
                    .collect();
                PolicySupport::Quorum {
                    threshold: quorum.invoke_threshold(),
                    have: contributing_roots.len(),
                    roots: quorum.roots().iter().map(|key| hex::encode(key.as_bytes())).collect(),
                    contributing_roots,
                }
            }
        })
        .collect();
    ActionAdmission { admitted, alternatives }
}

#[derive(Serialize)]
pub(crate) struct SharedCollection {
    root: String,
    read: ActionAdmission,
    #[serde(skip_serializing_if = "Option::is_none")]
    write: Option<ActionAdmission>,
}

impl SharedCollection {
    pub fn admitted(&self) -> bool {
        self.read.admitted && self.write.as_ref().is_none_or(|write| write.admitted)
    }
}

impl NodeStore {
    pub(crate) fn share_workspace(
        &mut self,
        expected: &ConfigView,
        revision: Id,
        subject: VerifyingKey,
        access: Access,
        delegate: bool,
    ) -> Result<Vec<SharedCollection>> {
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
        let actions = if access.write() {
            vec![ACTION_READ, ACTION_WRITE]
        } else {
            vec![ACTION_READ]
        };
        // Keep only prefixes this signer can extend over a requested collection.
        let mut parents_by_collection = BTreeMap::<_, Vec<_>>::new();
        for proof in source.proofs()? {
            let proof = proof?;
            let root = CollectionHandle::new(proof.resource().into_bytes());
            if roots.contains(&root) && proof.root_key() != signer {
                for prefix in proof.prefixes().filter(|prefix| prefix.subject() == signer) {
                    parents_by_collection.entry(root).or_default().push(prefix.to_proof());
                }
            }
        }
        for root in &roots {
            Collection::<blobencodings::SimpleArchive>::open(&source, *root)?;
            let facts: TribleSet = source.get(*root)?;
            for action in &actions {
                let policies: Vec<_> = descriptor::admission_policies(
                    &source, &facts, *action, Some(blobencodings::SimpleArchive::id()),
                ).collect();
                if policies.contains(&AdmissionPolicy::Open) {
                    continue;
                }
                let definition = entity! {
                    capability_action: *action,
                    capability_delegate_action*: delegate.then_some(*action),
                };
                let handle =
                    staging.put::<blobencodings::SimpleArchive, _>(definition.facts().clone())?;
                material.insert(handle.transmute());
                let mut contributed = false;
                if policies.iter().any(|policy| policy.has_root(signer)) {
                    let proof = CapabilityProof::new((*root).into(), &self.signer, handle, subject);
                    staging.insert_proof(proof)?;
                    contributed = true;
                }
                // A configured root supplies only its own quorum share. It
                // may also hold independently rooted delegation paths needed
                // for the recipient to meet the native threshold.
                for parent in parents_by_collection.get(root).into_iter().flatten() {
                    if !policies.iter().any(|policy| policy.has_root(parent.root_key())) {
                        continue;
                    }
                    // Repeating a self-share must not append another identical
                    // edge to a path the signer has already issued to itself.
                    if subject == signer && parent.leaf_issuer() == signer
                        && parent.capabilities().last() == Some(handle)
                    {
                        continue;
                    }
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
                    if child.validate_delegation(&snapshot).is_ok() {
                        staging.insert_proof(child)?;
                        contributed = true;
                    }
                }
                if !contributed {
                    bail!("issuer cannot contribute a native grant for every requested workspace collection and action");
                }
            }
        }
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
        let snapshot = self.pile.snapshot()?;
        let mut proofs_by_resource = BTreeMap::<_, Vec<_>>::new();
        for proof in snapshot.proofs()? {
            let proof = proof?;
            let resource = CollectionHandle::new(proof.resource().into_bytes());
            if roots.contains(&resource) && proof.delegated_keys().any(|key| key == subject) {
                proofs_by_resource.entry(resource).or_default().push(proof);
            }
        }
        roots.into_iter().map(|root| {
            let facts: TribleSet = snapshot.get(root)?;
            let proofs = proofs_by_resource.get(&root).map(Vec::as_slice).unwrap_or_default();
            let admission = |action| action_admission(&snapshot, root, &facts, subject, action, proofs);
            Ok(SharedCollection {
                root: format!("blake3:{}", hex::encode(root.raw)),
                read: admission(ACTION_READ),
                write: access.write().then(|| admission(ACTION_WRITE)),
            })
        }).collect()
    }
}
