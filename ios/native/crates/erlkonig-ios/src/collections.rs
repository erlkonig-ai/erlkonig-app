//! Snapshot-derived UI pages and explicit native sync choices. No catalog or grants.
use anyhow::{Context, Result, ensure};
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::Serialize;
use triblespace_core::{
    capability::QuorumOutcome,
    collection::{self, CollectionHandle, CollectionRead, CollectionStoreExt, descriptor,
        record_counts::RecordCountStatus, private_policy,
        selection::{self, CONFIG_COLLECTION_NAME, Selection}},
    metadata, prelude::*, repo::{CapabilityProofRead, StoreChanges, StoreDependencies, StoreRead, StoreSnapshot, pile::PileSnapshot},
    collection::observed_store::ObservedStore,
    query::register::{ObservationOrder, maximal},
};

mod attrs {
    use triblespace_core::prelude::*;
    include!("sync_intent_schema.rs");
}

pub const PAGE_SIZE: usize = 32;

/// Existing saved Join means the listener may continue. Its policy archive is
/// retained in history; it no longer causes any collection to be selected.
pub fn listening(pile: &mut Pile, key: &SigningKey) -> Result<bool> {
    listening_in(&pile.snapshot()?, key.verifying_key())
}

pub(crate) fn listening_in(snapshot: &PileSnapshot, reader: VerifyingKey) -> Result<bool> {
    let facts = selection::config_facts(snapshot, reader)?;
    let order = ObservationOrder::new(&facts, metadata::supersedes.id());
    let values: Vec<_> = find!((state: Id, enabled: bool), and!(
        pattern!(&facts, [{ ?state @ attrs::config_anchor: attrs::NODE_SETTINGS,
            attrs::sync_enabled: ?enabled }]), maximal(state, &order),
    )).map(|(_, enabled)| enabled).collect();
    Ok(!values.is_empty() && values.iter().all(|enabled| *enabled))
}

pub fn start_discovery(pile: &mut Pile, key: &SigningKey) -> Result<()> {
    if listening(pile, key)? { return Ok(()); }
    let facts = selection::config_facts(&pile.snapshot()?, key.verifying_key())?;
    let order = ObservationOrder::new(&facts, metadata::supersedes.id());
    let previous: Vec<_> = find!(state: Id, and!(
        pattern!(&facts, [{ ?state @ attrs::config_anchor: attrs::NODE_SETTINGS }]),
        maximal(state, &order),
    )).collect();
    let config = pile.collection(CONFIG_COLLECTION_NAME, private_policy(key.verifying_key()))?;
    pile.commit(config, key, entity! {
        attrs::config_anchor: attrs::NODE_SETTINGS, attrs::sync_enabled: true,
        attrs::maintenance_enabled: false, attrs::mcp_port: 0u16,
        metadata::supersedes*: previous,
    })?;
    pile.flush().context("save discovery intent")?;
    Ok(())
}

pub fn state_token(state: &Selection) -> String {
    match state {
        Selection::Unset => "unset".into(),
        Selection::Selected => "selected".into(),
        Selection::Unselected => "unselected".into(),
        Selection::Conflicted(heads) => format!("conflicted:{}", heads.iter()
            .map(|id| format!("{id:X}")).collect::<Vec<_>>().join(",")),
    }
}

#[derive(Clone, Serialize)]
pub struct Row {
    #[serde(skip)]
    handle: CollectionHandle,
    pub root: String,
    pub name: String,
    pub state: String,
    pub selected: bool,
    pub conflicted: bool,
    pub readable: bool,
    pub counts: Option<crate::storage::Counts>,
    pub count_error: Option<String>,
    pub counts_replay_complete: Option<bool>,
}
#[derive(Clone, Default, Serialize)]
pub struct Page {
    pub rows: Vec<Row>,
    pub offset: usize,
    pub total: usize,
    pub has_more: bool,
    pub pending: usize,
    pub prefix_bytes: u64,
}

/// One disposable metadata projection, bounded at the FFI boundary. Collection
/// payloads are never read here. Native admission follows all supported policies,
/// exactly as desktop AllAccessible does; availability does not select anything.
pub fn page(pile: &mut Pile, key: &SigningKey, offset: usize) -> Result<Page> {
    let snapshot = pile.snapshot()?;
    project(&snapshot, key.verifying_key(), offset, PAGE_SIZE)
}

/// The same ephemeral row projection for a page or a whole chart observation.
/// A chart supplies its immutable snapshot and does not select or track roots.
pub(crate) fn project(snapshot: &PileSnapshot, reader: VerifyingKey, offset: usize, limit: usize) -> Result<Page> {
    project_observation(snapshot, snapshot, reader, offset, limit, true)
}

/// The current immutable replay prefix, including readable observed choices.
/// Unknown config bytes are distinct from an observed Unset choice.
#[cfg(test)]
pub(crate) fn project_prefix(snapshot: &PileSnapshot, reader: VerifyingKey, offset: usize, limit: usize) -> Result<Page> {
    project_observation(snapshot, snapshot, reader, offset, limit, false)
}

pub(crate) struct PrefixPage {
    snapshot: PileSnapshot,
    dependencies: StoreDependencies,
    reader: VerifyingKey,
    page: Page,
}

impl PrefixPage {
    fn metadata_current(&self, snapshot: &PileSnapshot, reader: VerifyingKey, offset: usize) -> Result<bool> {
        Ok(self.reader == reader && self.page.offset == offset
            && snapshot.changes_for(&self.snapshot, &self.dependencies).is_empty()
            && (!snapshot.changes_since(&self.snapshot).contains(StoreChanges::COLLECTION_RECORDS)
                || snapshot.collections()? == self.snapshot.collections()?))
    }

    pub fn observe(previous: &mut Option<Self>, snapshot: &PileSnapshot, reader: VerifyingKey, offset: usize) -> Result<Page> {
        if let Some(old) = previous {
            if old.metadata_current(snapshot, reader, offset)? {
                for row in &mut old.page.rows {
                    (row.counts, row.count_error, row.counts_replay_complete) = count_fields(snapshot.record_count_status(row.handle));
                }
                old.page.prefix_bytes = snapshot.prefix_len() as u64;
                old.snapshot = snapshot.clone();
                return Ok(old.page.clone());
            }
        }
        // Metadata reads include missing descriptor/name blobs, config and grants.
        // Candidate roots and per-row counts have separate indexed checks above.
        let tracked = ObservedStore::new(snapshot.clone());
        let page = project_observation(snapshot, &tracked, reader, offset, PAGE_SIZE, false)?;
        *previous = Some(Self { snapshot: snapshot.clone(), dependencies: tracked.dependencies(), reader, page: page.clone() });
        Ok(page)
    }
}

fn project_observation(source: &PileSnapshot, snapshot: &impl StoreRead, reader: VerifyingKey, offset: usize, limit: usize, complete: bool) -> Result<Page> {
    let facts = if !complete && !snapshot.contains_blob(selection::config_handle(reader))? {
        None
    } else {
        match selection::config_facts(snapshot, reader) {
            Ok(facts) => Some(facts),
            Err(selection::ConfigReadError::Read(collection::CollectionReadError::Realization(
                collection::CollectionRealizationError::MissingDependency { .. },
            ))) if !complete => None,
            Err(error) if !complete && triblespace_core::repo::is_missing_blob(&error) => None,
            // Corrupt or unsupported resident config is an error, never empty.
            Err(error) => return Err(error.into()),
        }
    };
    let proofs = snapshot.proofs()?.collect::<Result<Vec<_>, _>>()?;
    let mut candidates = source.collections()?;
    candidates.extend(proofs.iter().map(|p| CollectionHandle::new(p.resource().into_bytes())));
    if let Some(facts) = &facts {
        candidates.extend(find!(root: CollectionHandle,
            pattern!(facts, [{ _?state @ selection::sync_collection: ?root }])));
    }
    candidates.sort_unstable(); candidates.dedup();
    let mut page = Page { offset, prefix_bytes: source.prefix_len() as u64, ..Page::default() };
    for root in candidates {
        if root == selection::config_handle(reader) { continue; }
        let state = facts.as_ref().map(|facts| selection::sync_selection(facts, root));
        let descriptor: Option<TribleSet> = snapshot.get(root).ok();
        if descriptor.as_ref().is_some_and(|d| descriptor::parents(d).is_ok_and(|p| !p.is_empty())) {
            continue;
        }
        let admission = collection::collection_reader_is_admitted_by(
            snapshot, root, reader, &proofs);
        let readable = matches!(admission, Ok(QuorumOutcome::Met));
        if !complete && descriptor.is_none() && state.is_none() { continue; }
        if descriptor.is_none() || matches!(admission, Err(_) | Ok(QuorumOutcome::Undefined(_))) {
            page.pending += 1;
        }
        if !readable && matches!(state, None | Some(Selection::Unset | Selection::Unselected)) { continue; }
        let index = page.total; page.total += 1;
        if index < offset || page.rows.len() >= limit { continue; }
        let mut names = std::collections::BTreeSet::new();
        if let Some(descriptor) = descriptor {
            for handle in find!(name: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
                pattern!(&descriptor, [{ _?descriptor @ collection::records::collection_name: ?name }])) {
                if let Ok(name) = snapshot.get::<Blob<blobencodings::UTF8String>, _>(handle) {
                    if let Ok(name) = std::str::from_utf8(&name.bytes) {
                        names.insert(name.chars().take(80).collect::<String>());
                    }
                }
            }
        }
        let (counts, count_error, counts_replay_complete) = count_fields(source.record_count_status(root));
        page.rows.push(Row {
            handle: root,
            root: hex::encode(root.raw),
            name: if names.is_empty() {
                if complete { "Unnamed collection".into() } else { hex::encode(root.raw) }
            } else { names.into_iter().collect::<Vec<_>>().join(" / ").chars().take(160).collect() },
            state: state.as_ref().map(state_token).unwrap_or_else(|| "unknown".into()),
            selected: state == Some(Selection::Selected),
            conflicted: matches!(state, Some(Selection::Conflicted(_))), readable,
            counts, count_error, counts_replay_complete,
        });
    }
    page.has_more = page.total > offset.saturating_add(page.rows.len());
    Ok(page)
}

/// Projection only: native owns canonical/direct state; no fallback enumeration.
fn count_fields(status: RecordCountStatus) -> (Option<crate::storage::Counts>, Option<String>, Option<bool>) {
    match status {
        RecordCountStatus::Ready { counts, replay_complete } =>
            (Some(crate::storage::Counts { records: counts.records as usize, direct_present: counts.direct_present as usize }), None, Some(replay_complete)),
        RecordCountStatus::Failed(error) => (None, Some(error.to_string()), None),
        RecordCountStatus::Untracked { .. } => (None, None, None),
    }
}

/// Only the owned worker writes. Recheck both current authority and the state the
/// user actually saw; a newly arrived conflict/stale choice is never overwritten.
pub fn choose(pile: &mut Pile, key: &SigningKey, root: CollectionHandle,
    expected: &str, enabled: bool) -> Result<()> {
    let snapshot = pile.snapshot()?;
    let facts = selection::config_facts(&snapshot, key.verifying_key())?;
    let current = selection::sync_selection(&facts, root);
    ensure!(state_token(&current) == expected, "Selection changed; review the current choice");
    if enabled {
        let descriptor: TribleSet = snapshot.get(root).context("collection metadata unavailable")?;
        ensure!(descriptor::parents(&descriptor)?.is_empty(), "Select the parent collection");
        let proofs = snapshot.proofs()?.collect::<Result<Vec<_>, _>>()?;
        ensure!(matches!(collection::collection_reader_is_admitted_by(
            &snapshot, root, key.verifying_key(), &proofs)?, QuorumOutcome::Met),
            "Read access is not currently established");
    }
    if (enabled && current == Selection::Selected) || (!enabled && current == Selection::Unselected) { return Ok(()); }
    drop(snapshot);
    let config = pile.collection(CONFIG_COLLECTION_NAME, private_policy(key.verifying_key()))?;
    selection::write_sync_selection(pile, config, key, root, enabled)?;
    pile.flush().context("save collection choice")?;
    Ok(())
}

#[cfg(test)]
mod tests;
