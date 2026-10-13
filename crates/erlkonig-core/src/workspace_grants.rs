//! Enrollment from natively exchanged grants, with no metadata handoff.
//!
//! Native grant exchange delivers a proof naming this node when a peer
//! holding it connects. A landed proof is resident in the pile; its
//! descriptor becomes resident once sync or repair fetched it. This module
//! projects those resident proofs and descriptors into an enrollment prompt
//! and publishes a local workspace over exactly the granted roots, the way
//! bootstrap enrollment does, but reading what native exchange delivered
//! instead of a signed handoff. It fetches, grants and imports nothing.
//!
//! A descriptor carries the issuing workspace's anchor among its tags and,
//! since 2026-10-13, the faculty role it was created for. Older descriptors
//! declare no role; for those the role is read from the `label · role`
//! suffix of the descriptor name and reported as such, so a reader can see
//! which derivation it is trusting.
//!
//! The reader is open world. A proof may name a resource that is no
//! collection at all; a descriptor may arrive before the name blob it
//! references, or carry tags this module does not model. None of that is an
//! error of the listing: what cannot be read degrades one entry, with the
//! reason, and what is no collection is skipped and counted.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use faculties::schemas::config::config;
use triblespace::core::capability::{capability_action, capability_delegate_action};
use triblespace::core::collection::{
    ACTION_READ, ACTION_WRITE, Collection, CollectionHandle, descriptor, records,
};
use triblespace::core::metadata;
use triblespace::core::query::register::{ObservationOrder, maximal};
use triblespace::core::repo::{BlobStoreGet, BlobStoreList, CapabilityProofRead, SnapshotSource};
use triblespace::prelude::*;

use crate::node_schema as attrs;
use crate::node_store::{ConfigView, NodeStore};
use crate::workspace::Workspace;
use crate::workspace_sharing::Enrolled;

/// Where a collection's faculty role was read from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoleSource {
    /// `collection_role` on the descriptor.
    Declared,
    /// The `label · role` suffix of the descriptor name.
    Name,
}

impl RoleSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Name => "name",
        }
    }
}

/// One collection some resident proof grants to this node. A projection of
/// one snapshot, recomputed per request; nothing here is persisted.
#[derive(Clone, Debug)]
pub(crate) struct GrantedCollection {
    pub root: CollectionHandle,
    pub descriptor_resident: bool,
    pub name: Option<String>,
    /// Why `name` or `anchor` is missing from a resident descriptor.
    pub reason: Option<String>,
    /// The single non-kind tag of the descriptor, when there is exactly one.
    pub anchor: Option<Id>,
    pub role: Option<(Id, RoleSource)>,
    /// Keys that signed the edge naming this node, as public identities.
    pub issuers: BTreeSet<[u8; 32]>,
    /// Actions the resident definitions of those edges grant.
    pub read: bool,
    pub write: bool,
    pub delegate: bool,
    /// Edges naming this node whose definition blob is not resident.
    pub definitions_pending: usize,
    /// Native READ/WRITE admission under the resident descriptor, when it
    /// could be evaluated.
    pub admitted: Option<(bool, bool)>,
    /// The root is routed by a current local workspace.
    pub enrolled_locally: bool,
}

/// Granted collections sharing one issuing workspace anchor.
#[derive(Clone, Debug)]
pub(crate) struct PendingShare {
    pub anchor: Id,
    pub label: Option<String>,
    pub collections: Vec<GrantedCollection>,
    /// A local workspace revision for this anchor exists (current, changed
    /// or withdrawn); enrollment will not overwrite it.
    pub local_revision: bool,
}

/// One listing: grouped shares, roots without a single anchor, and the
/// number of granted resources that are no collection descriptor.
#[derive(Clone, Debug)]
pub(crate) struct PendingListing {
    pub shares: Vec<PendingShare>,
    pub ungrouped: Vec<GrantedCollection>,
    pub skipped: usize,
}

/// Kind tags a descriptor blob carries that are not a workspace anchor.
const KNOWN_KINDS: [Id; 2] = [
    records::KIND_COLLECTION_DESCRIPTOR,
    records::KIND_COLLECTION_MAPPING,
];

fn note(reason: &mut Option<String>, text: String) {
    match reason {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&text);
        }
        None => *reason = Some(text),
    }
}

/// The `label` of a `label · role` descriptor name, when `role` is known.
fn label_of(name: &str, role: Option<Id>) -> String {
    role.and_then(faculties::collection_names::name_for)
        .and_then(|suffix| name.strip_suffix(suffix))
        .and_then(|stem| stem.strip_suffix(" · "))
        .unwrap_or(name)
        .to_owned()
}

/// Current and withdrawn local revisions for one anchor exist at all.
fn has_local_revision(facts: &TribleSet, anchor: Id) -> bool {
    exists!(pattern!(facts, [{ _?state @ metadata::tag: attrs::WORKSPACE, config::anchor: anchor }]))
}

/// The current local workspace equal to `workspace`, when repeat enrollment
/// is a no-op. Any other local revision for the anchor refuses: a later edit
/// or withdrawal is never overwritten.
pub(crate) fn existing_enrollment(
    expected: &ConfigView,
    workspace: &Workspace,
) -> Result<Option<Workspace>> {
    let anchor = workspace.anchor;
    let local = expected.facts()?;
    let order = ObservationOrder::new(&local, metadata::supersedes.id());
    let existing: Vec<_> = find!(id: Id, and!(
        pattern!(&local, [{ ?id @ metadata::tag: attrs::WORKSPACE, config::anchor: anchor }]),
        maximal(id, &order),
    ))
    .collect();
    if existing.is_empty() {
        return Ok(None);
    }
    if existing.len() == 1 {
        if let Some(value) = expected.workspace(existing[0])? {
            if value.label == workspace.label && value.routes == workspace.routes {
                return Ok(Some(value));
            }
        }
    }
    bail!("this share's local workspace was changed or withdrawn; enrollment will not overwrite it")
}

impl NodeStore {
    /// Every collection a resident proof grants to this node, annotated from
    /// its resident descriptor, and the count of granted resources that are
    /// resident but no collection descriptor. Reads one snapshot; fetches
    /// nothing.
    pub(crate) fn granted_collections(
        &mut self,
        expected: &ConfigView,
    ) -> Result<(Vec<GrantedCollection>, usize)> {
        let me = self.signer.verifying_key();
        let mut local_roots = BTreeSet::new();
        for revision in expected.workspace_ids()? {
            if let Ok(Some(workspace)) = expected.workspace(revision) {
                local_roots.extend(workspace.routes.values().copied());
            }
        }
        self.pile.refresh().context("refresh node pile")?;
        let snapshot = self.pile.snapshot()?;
        // One row per granted root; proofs naming this node contribute the
        // edge that names it: its issuer and its definition's actions.
        let mut rows: BTreeMap<CollectionHandle, GrantedCollection> = BTreeMap::new();
        for proof in snapshot.proofs()? {
            let proof = proof?;
            let root = CollectionHandle::new(proof.resource().into_bytes());
            let mut issuer = proof.root_key();
            for (delegate, capability) in proof.delegated_keys().zip(proof.capabilities()) {
                if delegate == me {
                    let row = rows.entry(root).or_insert_with(|| GrantedCollection {
                        root,
                        descriptor_resident: false,
                        name: None,
                        reason: None,
                        anchor: None,
                        role: None,
                        issuers: BTreeSet::new(),
                        read: false,
                        write: false,
                        delegate: false,
                        definitions_pending: 0,
                        admitted: None,
                        enrolled_locally: local_roots.contains(&root),
                    });
                    row.issuers.insert(issuer.to_bytes());
                    if snapshot.contains_blob(capability)? {
                        let definition: TribleSet = snapshot.get(capability)?;
                        row.read |= exists!(pattern!(&definition, [{ capability_action: ACTION_READ }]));
                        row.write |= exists!(pattern!(&definition, [{ capability_action: ACTION_WRITE }]));
                        row.delegate |= exists!(pattern!(&definition, [{ capability_delegate_action: _?action }]));
                    } else {
                        row.definitions_pending += 1;
                    }
                }
                issuer = delegate;
            }
        }
        let mut skipped = 0;
        let mut collections = Vec::with_capacity(rows.len());
        for (root, mut row) in rows {
            if !snapshot.contains_blob(root)? {
                collections.push(row);
                continue;
            }
            // A resident resource is a collection only when it decodes as
            // facts holding one descriptor entity. Anything else is some
            // other resource a proof may name; it is no share and is skipped.
            let facts: Result<TribleSet, _> = snapshot.get(root);
            let Ok(facts) = facts else {
                skipped += 1;
                continue;
            };
            let Ok(entity) = descriptor::entity(&facts) else {
                skipped += 1;
                continue;
            };
            row.descriptor_resident = true;
            match descriptor::name(&facts).ok().flatten() {
                Some(name) if snapshot.contains_blob(name)? => {
                    let name: Result<View<str>, _> = snapshot.get(name);
                    match name {
                        Ok(name) => row.name = Some(name.to_string()),
                        Err(_) => note(&mut row.reason, "name unreadable".to_owned()),
                    }
                }
                Some(_) => note(&mut row.reason, "name pending".to_owned()),
                None => {}
            }
            let anchors: BTreeSet<Id> = find!(tag: Id, pattern!(&facts, [{ entity @ metadata::tag: ?tag }]))
                .filter(|tag| !KNOWN_KINDS.contains(tag))
                .collect();
            match anchors.len() {
                1 => row.anchor = anchors.into_iter().next(),
                0 => note(&mut row.reason, "descriptor carries no candidate anchor".to_owned()),
                n => note(&mut row.reason, format!("descriptor carries {n} candidate anchors")),
            }
            let declared: BTreeSet<Id> =
                find!(role: Id, pattern!(&facts, [{ entity @ attrs::collection_role: ?role }])).collect();
            row.role = match declared.into_iter().collect::<Vec<_>>()[..] {
                [role] if faculties::collection_names::name_for(role).is_some() => {
                    Some((role, RoleSource::Declared))
                }
                _ => row.name.as_deref().and_then(|name| {
                    let (_, suffix) = name.rsplit_once(" · ")?;
                    let (role, _) = faculties::collection_names::table()
                        .into_iter()
                        .find(|(_, candidate)| *candidate == suffix)?;
                    Some((role, RoleSource::Name))
                }),
            };
            row.admitted = Collection::<blobencodings::SimpleArchive>::open(&snapshot, row.root)
                .ok()
                .and_then(|collection| {
                    Some((
                        collection.reader_is_admitted(&snapshot, me).ok()?,
                        collection.writer_is_admitted(&snapshot, me).ok()?,
                    ))
                });
            collections.push(row);
        }
        Ok((collections, skipped))
    }

    /// Granted collections grouped by their issuing workspace anchor. With
    /// `all`, roots already routed locally are included (enrollment compares
    /// the whole group); without, only roots no local workspace routes.
    pub(crate) fn pending_shares(
        &mut self,
        expected: &ConfigView,
        all: bool,
    ) -> Result<PendingListing> {
        let facts = expected.facts()?;
        let mut groups: BTreeMap<Id, PendingShare> = BTreeMap::new();
        let mut ungrouped = Vec::new();
        let (collections, skipped) = self.granted_collections(expected)?;
        for row in collections {
            if row.enrolled_locally && !all {
                continue;
            }
            let Some(anchor) = row.anchor else {
                ungrouped.push(row);
                continue;
            };
            let share = groups.entry(anchor).or_insert_with(|| PendingShare {
                anchor,
                label: None,
                collections: Vec::new(),
                local_revision: has_local_revision(&facts, anchor),
            });
            if share.label.is_none() {
                share.label = row
                    .name
                    .as_deref()
                    .map(|name| label_of(name, row.role.map(|(role, _)| role)));
            }
            share.collections.push(row);
        }
        Ok(PendingListing {
            shares: groups.into_values().collect(),
            ungrouped,
            skipped,
        })
    }

    /// Publish a local workspace over exactly the granted roots of one
    /// anchor and select them, as bootstrap enrollment does. No proof,
    /// descriptor or key is imported, copied or created.
    pub(crate) fn enroll_pending_share(
        &mut self,
        expected: &ConfigView,
        anchor: Id,
        label: &str,
    ) -> Result<Enrolled> {
        if label.trim().is_empty() {
            bail!("local workspace label is required");
        }
        if self.config_changed(expected)? {
            bail!("node configuration changed; inspect before enrolling");
        }
        let share = self
            .pending_shares(expected, true)?
            .shares
            .into_iter()
            .find(|share| share.anchor == anchor)
            .context("no resident grant names a collection with this workspace anchor; call config_list_pending_shares")?;
        let mut workspace = Workspace {
            id: anchor,
            anchor,
            label: label.trim().to_owned(),
            routes: BTreeMap::new(),
        };
        let mut write = true;
        let mut delegate = true;
        for collection in &share.collections {
            let (role, _) = collection.role.with_context(|| {
                format!(
                    "granted collection blake3:{} declares no understood faculty role; enroll it with explicit routes",
                    hex::encode(collection.root.raw)
                )
            })?;
            if workspace.routes.insert(role, collection.root).is_some() {
                bail!("this share grants two collections for one faculty role; enroll it with explicit routes");
            }
            write &= collection.write;
            delegate &= collection.delegate;
        }
        if let Some(existing) = existing_enrollment(expected, &workspace)? {
            return Ok(Enrolled {
                workspace: existing,
                source: anchor,
                write,
                delegate,
                already_enrolled: true,
            });
        }
        workspace.id = self.put_workspace(expected, &workspace)?;
        Ok(Enrolled {
            workspace,
            source: anchor,
            write,
            delegate,
            already_enrolled: false,
        })
    }
}
