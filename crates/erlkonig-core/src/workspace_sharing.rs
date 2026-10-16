//! One-way workspace sharing; authority remains native and collection-local.
//!
//! The handoff contains descriptor/name metadata and one native signed COMMIT,
//! never capability proofs or definition bodies. Signature verification is not
//! collection admission: enrolled known roots bootstrap through native repair.
//! No invitation registry, authorization handshake or replacement data root
//! is created. Local publication never asserts replica synchronization.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use faculties::schemas::config::config;
use serde::{Deserialize, Serialize};
use triblespace::core::blob::{Blob, Bytes, encodings::UnknownBlob};
use triblespace::core::capability::{capability_action, capability_delegate_action};
use triblespace::core::collection::{
    ACTION_READ, ACTION_WRITE, AdmissionPolicy, Collection, CollectionCommit, CollectionGrantError,
    CollectionHandle, CollectionStoreExt, descriptor, grant_collection_capability,
};
use triblespace::core::metadata;
use triblespace::core::repo::memoryrepo::MemoryRepo;
use triblespace::core::repo::{
    BlobStoreGet, BlobStoreList, BlobStorePut, CapabilityProofRead, CapabilityProofStore,
    SnapshotSource, StorageFlush, transfer,
};
use triblespace::macros::id_hex;
use triblespace::prelude::*;

use crate::node_schema as attrs;
use crate::node_store::{ConfigView, NodeStore};
use crate::workspace::Workspace;

// Minted verbatim with installed `trible genid`, 2026-10-11. Attribute identity
// includes encoding. Manifest entity IDs are opaque; no reader re-derives them.
const BOOTSTRAP: Id = id_hex!("D2133CF8ECFBC76482A809D26619C527");
attributes! {
    "E7839D5A62F3FEAC9C5B57FC62E3BD0A" as recipient: inlineencodings::ED25519PublicKey;
    "EDA5820B5BE702A3CD515F88CC03E4BA" as source_workspace: inlineencodings::GenId;
    "21BCA8E19D4D1565D5FFE6AE89E08CC0" as write_access: inlineencodings::Boolean;
    "0A8BF5A2A918FF20DEB55C120147AC30" as delegate_access: inlineencodings::Boolean;
}

const MAX_BOOTSTRAP_BYTES: usize = 4 * 1024 * 1024;
const MAX_BLOBS: usize = 4096;

/// An interchange value of one bounded operation, never a persisted catalog.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transport {
    version: u8,
    commit: String,
    blobs: Vec<WireBlob>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireBlob {
    handle: String,
    bytes: String,
}

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

/// Operation-local signed metadata, not native collection admission or a
/// registry. Inspection imports nothing and selects no network work.
pub(crate) struct PreparedShare {
    pub workspace: Workspace,
    pub source: Id,
    pub issuer: VerifyingKey,
    pub write: bool,
    pub delegate: bool,
    pub store: MemoryRepo,
    material: BTreeSet<Inline<inlineencodings::Handle<UnknownBlob>>>,
}

pub(crate) fn public_identity(value: &str) -> Result<VerifyingKey> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(value, &mut bytes).context("recipient must be an exact public node ID")?;
    let key = VerifyingKey::from_bytes(&bytes).context("invalid recipient public node ID")?;
    AdmissionPolicy::quorum([key], 1, None)
        .context("recipient must be a canonical non-weak public node ID")?;
    Ok(key)
}

/// Copy this exact descriptor and its name. Only the issuer's native grant
/// preparation additionally needs resident policy-definition bodies; these
/// never travel in the metadata handoff.
fn root_material<S: BlobStoreGet + BlobStoreList>(
    source: &S,
    staging: &mut MemoryRepo,
    root: CollectionHandle,
    admission: bool,
) -> Result<BTreeSet<Inline<inlineencodings::Handle<UnknownBlob>>>> {
    let facts: TribleSet = source
        .get(root)
        .context("workspace root descriptor unavailable")?;
    let mut handles = BTreeSet::from([root.transmute()]);
    for (handle, _) in descriptor::capability_policies(&facts, None) {
        if admission && source.contains_blob(handle)? {
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
    ) -> Result<String> {
        if self.config_changed(expected)? {
            bail!("node configuration changed; inspect before inviting");
        }
        let workspace = expected
            .workspace(revision)?
            .context("workspace is no longer current; inspect before inviting")?;
        if workspace.routes.is_empty() {
            bail!("workspace has no collection routes to share");
        }
        let source = self.pile.snapshot()?;
        let signer = self.signer.verifying_key();
        let mut staging = MemoryRepo::default();
        let mut material = BTreeSet::new();
        let roots: BTreeSet<_> = workspace.routes.values().copied().collect();
        for root in &roots {
            material.extend(root_material(&source, &mut staging, *root, true)?);
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
                        return Err(error).context("prepare native collection bootstrap");
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
                        copied.context("load delegated bootstrap definitions")?;
                    }
                    let Ok(child) = parent.delegate(&self.signer, handle, subject) else {
                        continue;
                    };
                    let snapshot = staging.snapshot()?;
                    if child.validate_delegation(&snapshot).is_ok()
                        && descriptor::validate_proof_evidence(
                            &snapshot,
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
        let mut routes = Fragment::empty();
        for (role, root) in &workspace.routes {
            crate::workspace::role_name(*role)
                .context("workspace has an unknown faculty role")?;
            routes += entity! { config::anchor: *role, config::selects: *root };
        }
        let mut manifest = entity! {
            metadata::tag: BOOTSTRAP, source_workspace: workspace.id,
            recipient: subject, write_access: access.write(), delegate_access: delegate,
            metadata::name: workspace.label, attrs::workspace_route*: routes,
        };
        // Metadata and authority are separate native values. Export only the
        // exact descriptors/names and signed manifest, never grant evidence.
        let mut metadata = MemoryRepo::default();
        let mut disclosed = BTreeSet::new();
        for root in &roots {
            disclosed.extend(root_material(&source, &mut metadata, *root, false)?);
        }
        disclosed.extend(
            manifest
                .blobs_mut()
                .snapshot()?
                .iter()
                .map(|(handle, _)| handle),
        );
        let envelope = metadata.collection(
            "Erlkonig workspace bootstrap",
            faculties::collection_names::private_policy(signer),
        )?;
        let commit = metadata.commit(envelope, &self.signer, manifest)?;
        disclosed.insert(Inline::new(commit.data().raw));
        let snapshot = metadata.snapshot()?;
        let transport = Transport {
            version: 2,
            commit: hex::encode(commit.to_bytes()),
            blobs: disclosed
                .into_iter()
                .map(|handle| {
                    let blob: Blob<UnknownBlob> = snapshot.get(handle)?;
                    Ok(WireBlob {
                        handle: hex::encode(handle.raw),
                        bytes: hex::encode(&blob.bytes),
                    })
                })
                .collect::<Result<_>>()?,
        };
        let encoded = serde_json::to_string(&transport)?;
        if encoded.len() > MAX_BOOTSTRAP_BYTES || transport.blobs.len() > MAX_BLOBS {
            bail!("workspace bootstrap exceeds the supported transport size");
        }
        // No fallible permission/manifest validation remains. Proof records are
        // deterministic native values; repeat issue is a set no-op. This is not
        // a multi-record transaction: an I/O failure may retain earlier evidence.
        let authority = staging.snapshot()?;
        for copied in transfer(&authority, &mut self.pile, material) {
            copied.context("publish bootstrap admission material")?;
        }
        for proof in authority.proofs()? {
            self.pile.insert_proof(proof?)?;
        }
        self.pile.flush()?;
        Ok(encoded)
    }

    pub(crate) fn probe_workspace_share(&self, bootstrap: &str) -> Result<PreparedShare> {
        if bootstrap.len() > MAX_BOOTSTRAP_BYTES {
            bail!("workspace bootstrap exceeds the supported transport size");
        }
        let transport: Transport =
            serde_json::from_str(bootstrap).context("malformed workspace bootstrap")?;
        if transport.version != 2 || transport.blobs.len() > MAX_BLOBS {
            bail!("unsupported or oversized workspace bootstrap");
        }
        let commit = CollectionCommit::from_bytes(
            hex::decode(&transport.commit)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("malformed bootstrap COMMIT"))?,
        )?;
        let issuer = VerifyingKey::from_bytes(&commit.public_key().raw)?;
        let mut staging = MemoryRepo::default();
        for value in transport.blobs {
            let blob = Blob::<UnknownBlob>::new(Bytes::from_source(hex::decode(value.bytes)?));
            if hex::encode(blob.get_handle().raw) != value.handle {
                bail!("bootstrap blob does not match its native content handle");
            }
            staging.put::<UnknownBlob, _>(blob)?;
        }
        let snapshot = staging.snapshot()?;
        // from_bytes strictly verifies the native COMMIT signature. Read its
        // exact content-addressed member, without claiming collection READ or
        // WRITE admission (which native bootstrap has not yet acquired).
        let facts: TribleSet = snapshot.get(Inline::<
            inlineencodings::Handle<blobencodings::SimpleArchive>,
        >::new(commit.data().raw))?;
        let subject = self.signer.verifying_key();
        let candidates: BTreeSet<_> = find!(
            (id: Id, source: Id, label: Inline<inlineencodings::Handle<blobencodings::UTF8String>>, write: bool, delegate: bool),
            pattern!(&facts, [{ ?id @ metadata::tag: BOOTSTRAP, recipient: subject,
                source_workspace: ?source, metadata::name: ?label,
                write_access: ?write, delegate_access: ?delegate }])
        ).collect();
        if candidates.len() != 1 {
            bail!("bootstrap has no unambiguous manifest for this node identity");
        }
        let (anchor, source, label, write, delegate) = *candidates.first().unwrap();
        let label: View<str> = snapshot.get(label)?;
        if label.trim().is_empty() {
            bail!("bootstrap workspace name is empty");
        }
        let workspace = Workspace {
            id: anchor,
            anchor,
            label: label.to_string(),
            routes: BTreeMap::new(),
        };
        let mut workspace = workspace;
        for (role, root) in find!((role: Id, root: CollectionHandle), pattern!(&facts, [
            { anchor @ attrs::workspace_route: _?route },
            { _?route @ config::anchor: ?role, config::selects: ?root },
        ])) {
            crate::workspace::role_name(role)
                .context("bootstrap names an unknown faculty role")?;
            if workspace
                .routes
                .insert(role, root)
                .is_some_and(|old| old != root)
            {
                bail!("bootstrap has ambiguous roots for a faculty role");
            }
        }
        if workspace.routes.is_empty() {
            bail!("bootstrap has no understood collection routes");
        }
        let roots: BTreeSet<_> = workspace.routes.values().copied().collect();
        let mut material = BTreeSet::new();
        for root in &roots {
            // A separate staging store would be redundant; collect just the
            // typed dependencies, then copy only these after authorization.
            let mut selected = MemoryRepo::default();
            material.extend(root_material(&snapshot, &mut selected, *root, false)?);
        }
        Ok(PreparedShare {
            workspace,
            source,
            issuer,
            write,
            delegate,
            store: staging,
            material,
        })
    }

    fn publish_share_metadata(&mut self, prepared: &mut PreparedShare) -> Result<()> {
        let snapshot = prepared.store.snapshot()?;
        for copied in transfer(&snapshot, &mut self.pile, prepared.material.iter().copied()) {
            copied.context("import shared descriptor/name metadata")?;
        }
        self.pile.flush()?;
        Ok(())
    }

    pub(crate) fn enroll_workspace_share(
        &mut self,
        expected: &ConfigView,
        bootstrap: &str,
        label: &str,
    ) -> Result<Enrolled> {
        if label.trim().is_empty() {
            bail!("local workspace label is required");
        }
        if self.config_changed(expected)? {
            bail!("node configuration changed; inspect before enrolling");
        }
        let mut prepared = self.probe_workspace_share(bootstrap)?;
        let mut workspace = prepared.workspace.clone();
        workspace.label = label.trim().to_owned();
        // Repeat enrollment never overwrites a later edit or resurrects a
        // withdrawal. Query the opaque manifest anchor; don't re-hash an ID.
        if let Some(value) = crate::workspace_grants::existing_enrollment(expected, &workspace)? {
            return Ok(Enrolled {
                workspace: value,
                source: prepared.source,
                write: prepared.write,
                delegate: prepared.delegate,
                already_enrolled: true,
                extended: false,
            });
        }
        self.publish_share_metadata(&mut prepared)?;
        // Blob insertion changes native observation dependencies even
        // without changing Config. Reobserve after our metadata publication,
        // then check the original Config support/text precondition again.
        let current = self.config()?;
        if current.observation()? != expected.observation()? {
            bail!("node configuration changed during enrollment; inspect again");
        }
        workspace.id = self.put_workspace(&current, &workspace)?;
        Ok(Enrolled {
            workspace,
            source: prepared.source,
            write: prepared.write,
            delegate: prepared.delegate,
            already_enrolled: false,
            extended: false,
        })
    }
}
