//! Explicit settings handoff between node identities. Preparation is a pure
//! read of frozen observations; it neither opens keys nor merges whole piles.
//! Historical facts and conflicting heads travel unchanged. Successors name
//! only the revisions actually observed, never an invented winner.

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use faculties::schemas::config::config;
use triblespace::core::collection::{
    Collection, CollectionHandle, CollectionRecordSelector, CollectionSnapshotExt,
    CollectionStoreExt,
};
use triblespace::core::metadata;
use triblespace::core::query::register::{ObservationOrder, maximal};
use triblespace::core::repo::pile::PileSnapshot;
use triblespace::core::repo::{
    BlobStoreGet, BlobStoreList, SnapshotSource, StorageFlush, StoreChanges, StoreDependencies,
    StoreSnapshot, potential_handles, reachable,
};
use triblespace::prelude::*;
use triblespace_net::health_record;

use crate::{
    config::NodeBinding,
    node_schema as attrs,
    node_store::{ConfigView, NodeSettings, NodeStore},
};

/// A prepared publication, not a second database. No source handle becomes a
/// destination merely because it was in the old configuration.
pub struct MigrationPlan {
    config: Fragment,
    metrics: Option<Fragment>,
    target_snapshot: PileSnapshot,
    target_config: CollectionHandle,
    pub source_config: CollectionHandle,
    pub workspace_successors: usize,
    pub settings_successors: usize,
    pub retained: Vec<String>,
}

impl MigrationPlan {
    /// `accepted_sources` are explicit operator-approved old workspace
    /// bindings. The target must independently hold every exact routed root
    /// and admit its new signer. Settings do not confer collection authority.
    pub fn prepare(
        source: &PileSnapshot,
        source_config: CollectionHandle,
        source_metrics: Option<CollectionHandle>,
        target_snapshot: &PileSnapshot,
        target: &NodeBinding,
        accepted_sources: &[NodeBinding],
        defaults: &NodeSettings,
    ) -> Result<Self> {
        let mut fragment = copy_facts(source, source_config)?;
        let facts = fragment.facts().clone();
        let metrics = source_metrics
            .map(|handle| copy_facts(source, handle))
            .transpose()?;
        let mut plan = Self {
            config: Fragment::empty(),
            metrics,
            source_config,
            target_snapshot: target_snapshot.clone(),
            target_config: faculties::config::handle(VerifyingKey::from_bytes(&target.signer)?),
            workspace_successors: 0,
            settings_successors: 0,
            retained: Vec::new(),
        };
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let heads: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ config::anchor: attrs::NODE_SETTINGS }]), maximal(state, &order),
        )).collect();
        let any_settings =
            exists!(pattern!(&facts, [{ _?state @ config::anchor: attrs::NODE_SETTINGS }]));
        if heads.is_empty() && !any_settings {
            fragment += entity! {
                config::anchor: attrs::NODE_SETTINGS, attrs::mcp_port: defaults.mcp_port,
                attrs::sync_enabled: defaults.sync_enabled,
                attrs::maintenance_enabled: defaults.maintenance_enabled,
                health_record::attrs::peer*: defaults.peers.iter().map(VerifyingKey::from_bytes).collect::<std::result::Result<Vec<_>,_>>()?,
            };
            plan.settings_successors += 1;
        }
        for state in heads {
            let ports: Vec<u16> =
                find!(port: u16, pattern!(&facts, [{state @ attrs::mcp_port: ?port}])).collect();
            let mut sync: Vec<bool> =
                find!(value: bool, pattern!(&facts, [{state @ attrs::sync_enabled: ?value}]))
                    .collect();
            let mut maintain: Vec<bool> = find!(value: bool, pattern!(&facts, [{state @ attrs::maintenance_enabled: ?value}])).collect();
            if sync.is_empty() {
                sync.push(defaults.sync_enabled);
            }
            if maintain.is_empty() {
                maintain.push(defaults.maintenance_enabled);
            }
            let mut peers: Vec<VerifyingKey> = find!(peer: VerifyingKey, pattern!(&facts, [{state @ health_record::attrs::peer: ?peer}])).collect();
            if peers.is_empty()
                && !exists!(pattern!(&facts,[{state @ attrs::sync_enabled:_?explicit}]))
            {
                peers = defaults
                    .peers
                    .iter()
                    .map(VerifyingKey::from_bytes)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
            }
            if ports.is_empty() {
                plan.retained.push(format!(
                    "Settings {state:X} has no understood port; retained without choosing a value"
                ));
            }
            for port in ports {
                for sync in &sync {
                    for maintain in &maintain {
                        fragment += entity! {
                            config::anchor: attrs::NODE_SETTINGS, attrs::mcp_port: port,
                            attrs::sync_enabled: *sync, attrs::maintenance_enabled: *maintain,
                            health_record::attrs::peer*: peers.iter().copied(),
                            metadata::supersedes: state,
                        };
                        plan.settings_successors += 1;
                    }
                }
            }
        }
        let signer = VerifyingKey::from_bytes(&target.signer)?;
        for revision in find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::WORKSPACE, attrs::enrolled: true }]),
            maximal(state, &order),
        )) {
            if !exists!(pattern!(&facts, [{revision @ attrs::managed_pile: _?managed}])) {
                // Already path-free: retaining its exact original facts is enough.
                continue;
            }
            let bindings: Vec<_> = find!(
                (pile: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
                 key: Inline<inlineencodings::Handle<blobencodings::UTF8String>>, signer: VerifyingKey),
                pattern!(&facts, [{ revision @ attrs::pile_path: ?pile, attrs::key_path: ?key,
                    attrs::expected_signer: ?signer }])
            ).collect();
            let allowed = !bindings.is_empty() && bindings.iter().all(|(pile,key,signer)| {
                let p: Result<View<str>,_> = source.get(*pile);
                let k: Result<View<str>,_> = source.get(*key);
                matches!((p,k), (Ok(p),Ok(k)) if accepted_sources.iter().any(|binding|
                    binding.pile == std::path::Path::new(&*p) && binding.key == std::path::Path::new(&*k)
                    && binding.signer == signer.to_bytes()))
            });
            let routes: Vec<(Id, CollectionHandle)> =
                find!((role: Id,root: CollectionHandle),pattern!(&facts,[
                    {revision @ attrs::workspace_route: _?route},
                    {_?route @ config::anchor: ?role, config::selects: ?root},
                ]))
                .collect();
            let resident = routes.iter().all(|(_, root)| {
                let collection =
                    faculties::collection_names::open_workspace_in(target_snapshot, *root);
                matches!(collection, Ok(collection) if
                    collection.reader_is_admitted(target_snapshot,signer).unwrap_or(false)
                    || collection.writer_is_admitted(target_snapshot,signer).unwrap_or(false))
            });
            if !allowed || !resident {
                plan.retained.push(format!("Workspace {revision:X} retained unavailable: unapproved old binding or exact roots unavailable/unadmitted at target"));
                continue;
            }
            let mut converted = false;
            for (anchor, label) in find!(
                (anchor: Id,label: Inline<inlineencodings::Handle<blobencodings::UTF8String>>),
                pattern!(&facts,[{revision @ config::anchor:?anchor,metadata::name:?label}])
            ) {
                let mut route_facts = Fragment::empty();
                for (role, root) in &routes {
                    route_facts += entity! {config::anchor:*role,config::selects:*root};
                }
                fragment += entity! {
                    metadata::tag: attrs::WORKSPACE,config::anchor:anchor,attrs::enrolled:true,
                    metadata::name:label,attrs::workspace_route*:route_facts,metadata::supersedes:revision,
                };
                plan.workspace_successors += 1;
                converted = true;
            }
            if !converted {
                plan.retained.push(format!(
                    "Workspace {revision:X} is incomplete and retained unchanged"
                ));
            }
        }
        // Obsolete management facts stay historical evidence, never running
        // store/key routing. Report them rather than discarding their intent.
        for entry in find!(entry: Id, pattern!(&facts,[{_?state @ metadata::tag:attrs::MANAGED_PILE,config::anchor:?entry}]))
        {
            plan.retained.push(format!("Historical managed entry {entry:X} retained; no extra store or identity is activated"));
        }
        plan.config = fragment;
        Ok(plan)
    }

    /// Add one explicitly supplied existing bundle. This neither creates roots
    /// nor resolves any root by a name; routes may come from an operator's
    /// verified previous exact Faculty bindings during the handoff.
    pub fn add_workspace(
        &mut self,
        anchor: Id,
        label: &str,
        routes: &std::collections::BTreeMap<Id, CollectionHandle>,
        target: &PileSnapshot,
        signer: VerifyingKey,
    ) -> Result<()> {
        if label.trim().is_empty() {
            bail!("workspace label is required");
        }
        let mut selections = Fragment::empty();
        for (role, root) in routes {
            if faculties::collection_names::name_for(*role).is_none() {
                bail!("unknown faculty role {role:X}");
            }
            let collection = faculties::collection_names::open_workspace_in(target, *root)?;
            if !collection.reader_is_admitted(target, signer)?
                && !collection.writer_is_admitted(target, signer)?
            {
                bail!(
                    "target signer cannot use selected existing collection {}",
                    hex::encode(root.raw)
                );
            }
            selections += entity! {config::anchor:*role,config::selects:*root};
        }
        self.config += entity! {
            metadata::tag:attrs::WORKSPACE,config::anchor:anchor,attrs::enrolled:true,
            metadata::name:label.trim().to_owned(),attrs::workspace_route*:selections,
        };
        self.workspace_successors += 1;
        Ok(())
    }

    /// Explicit application to the target only. The source snapshot cannot be
    /// mutated. Metrics publish before the config adoption boundary; retries
    /// union identical facts if an I/O failure interrupts this two-collection
    /// publication. No transaction across collections is claimed.
    pub fn apply(self, target: &mut NodeStore, expected: &ConfigView) -> Result<()> {
        target.pile.refresh()?;
        let mut interests = StoreDependencies::default();
        interests
            .records
            .insert(CollectionRecordSelector::Collection(self.target_config));
        if target
            .pile
            .snapshot()?
            .changes_for(&self.target_snapshot, &interests)
            != StoreChanges::NONE
        {
            bail!(
                "target configuration changed since plan preparation; prepare a fresh migration plan"
            );
        }
        if target.config_changed(expected)? {
            bail!("target configuration changed; prepare a fresh migration plan");
        }
        if let Some(metrics) = self.metrics {
            target
                .pile
                .commit(target.metrics, &target.signer, metrics)?;
        }
        target
            .pile
            .commit(target.config, &target.signer, self.config)?;
        target.pile.flush()?;
        Ok(())
    }
}

/// Copy the native fact union and its resident conservative blob closure.
/// This visits only this exact config/metrics publication, not the whole pile.
/// Unknown facts, annotations and concurrent revisions remain intact.
fn copy_facts(snapshot: &PileSnapshot, handle: CollectionHandle) -> Result<Fragment> {
    let collection: Collection<blobencodings::SimpleArchive> = Collection::open(snapshot, handle)?;
    let observed = snapshot.collection_acquiring(collection)?;
    let facts: TribleSet = observed
        .view()
        .context("read complete migration source collection")?;
    let mut fragment = Fragment::new([], facts.clone());
    let mut roots = Vec::new();
    for handle in potential_handles(&facts) {
        if snapshot.contains_blob(handle)? {
            roots.push(handle);
        }
    }
    for handle in reachable(snapshot, roots) {
        let blob: Blob<blobencodings::UnknownBlob> = snapshot.get(handle)?;
        fragment.put::<blobencodings::UnknownBlob, _>(blob);
    }
    Ok(fragment)
}

#[cfg(test)]
mod tests;
