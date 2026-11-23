//! Typed configuration and observations in the single node pile.
//!
//! Only the bootstrap pile/key paths live outside the pile. Configuration is
//! read from the existing Faculty private `config` collection. The store keeps
//! an immutable collection observation for invalidation, not a configuration
//! cache or an inventory of other piles. Nothing here discovers, opens, creates
//! or takes ownership of a second pile or identity.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use ed25519_dalek::{SigningKey, VerifyingKey};
use faculties::schemas::config::config;
use triblespace::core::collection::{
    Collection, CollectionHandle, CollectionSnapshot, CollectionSnapshotExt, CollectionStoreExt, descriptor,
};
use triblespace::core::metadata::{self, MetaDescribe};
use triblespace::core::query::register::{ObservationOrder, maximal};
use triblespace::core::repo::pile::{Pile, PileSnapshot};
use triblespace::core::repo::{
    BlobStoreGet, SnapshotSource, StorageFlush, StoreChanges, StoreDependencies, StoreSnapshot,
};
use triblespace::prelude::*;

use crate::node_schema as attrs;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeSettings {
    pub mcp_port: u16,
    pub sync_enabled: bool,
    pub maintenance_enabled: bool,
}

impl Default for NodeSettings {
    fn default() -> Self {
        Self {
            mcp_port: 41773,
            sync_enabled: true,
            maintenance_enabled: true,
        }
    }
}

pub(crate) fn reporting_revisions(facts: &TribleSet, anchor: Id) -> Vec<Id> {
    let order = ObservationOrder::new(facts, metadata::supersedes.id());
    find!(revision: Id, and!(
        pattern!(facts, [{ ?revision @ config::anchor: anchor }]),
        maximal(revision, &order),
    ))
    .collect()
}

pub(crate) fn report_destination(
    snapshot: &PileSnapshot,
    handle: CollectionHandle,
    signer: VerifyingKey,
) -> Result<Collection<blobencodings::SimpleArchive>> {
    let collection =
        Collection::open(snapshot, handle).context("open native reporting destination")?;
    let facts: TribleSet = snapshot.get(handle)?;
    if descriptor::source(&facts)?.is_some() {
        bail!("reporting destination must be a source collection");
    }
    if !collection.writer_is_admitted(snapshot, signer)? {
        bail!("node key lacks WRITE admission to the reporting destination");
    }
    Ok(collection)
}

/// A frozen native observation. Query results are computed where requested.
#[derive(Clone)]
pub struct ConfigView {
    pub(crate) observed: CollectionSnapshot<PileSnapshot, blobencodings::SimpleArchive>,
    // Exact read interests, including failed text reads, never copied values.
    text_reads: Arc<Mutex<StoreDependencies>>,
}

impl ConfigView {
    pub(crate) fn from_snapshot(
        snapshot: PileSnapshot,
        collection: Collection<blobencodings::SimpleArchive>,
    ) -> Result<Self> {
        Ok(Self {
            observed: snapshot.collection_acquiring(collection)?,
            text_reads: Default::default(),
        })
    }
    pub(crate) fn text(
        &self,
        handle: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
    ) -> Result<String> {
        self.text_reads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .blobs
            .insert(Inline::new(handle.raw));
        let text: View<str> = self
            .observed
            .snapshot()
            .get(handle)
            .context("read configuration text")?;
        Ok(text.to_string())
    }

    pub(crate) fn is_current(&self, snapshot: &PileSnapshot) -> bool {
        self.observed.is_current(snapshot)
            && snapshot.changes_for(
                self.observed.snapshot(),
                &self
                    .text_reads
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            ) == StoreChanges::NONE
    }

    /// Native facts are exposed for typed inspection, not serialized config.
    pub fn facts(&self) -> Result<TribleSet> {
        self.observed
            .view()
            .context("read node configuration facts")
    }

    /// A transport precondition, not a blob handle or an authority. Native
    /// foundations stay private; no per-client settings cache is retained.
    pub(crate) fn observation(&self) -> Result<String> {
        let mut hash = blake3::Hasher::new_derive_key("Erlkonig Config observation v1");
        let support = self.observed.support()?;
        hash.update(&support.collection().handle().raw);
        for member in support.members() {
            hash.update(&member.raw);
        }
        // Labels are outside the Config archive. Include their readability so
        // an observation made while a blob was missing cannot edit the newly
        // readable workspace. Hash only; never export these bearer handles.
        let facts = self.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let names = find!((state: Id, name: Inline<inlineencodings::Handle<blobencodings::UTF8String>>), and!(
            or!(
                pattern!(&facts, [{ ?state @ metadata::tag: attrs::WORKSPACE,
                    attrs::enrolled: true, metadata::name: ?name }]),
                pattern!(&facts, [{ ?state @ config::anchor: attrs::SYNC_TELEMETRY,
                    triblespace_net::telemetry::attrs::worker: ?name }]),
            ),
            maximal(state, &order),
        )).map(|(_, name)| name).collect::<std::collections::BTreeSet<_>>();
        for name in names {
            hash.update(&name.raw);
            hash.update(&[u8::from(self.text(name).is_ok())]);
        }
        Ok(hash.finalize().to_hex().to_string())
    }

    /// Keep the local configuration endpoint available when other settings
    /// conflict. Only an agreed current port is selected; an uninterpretable
    /// port uses the well-known recovery port without choosing a settings fork.
    pub(crate) fn mcp_port(&self) -> Result<u16> {
        let facts = self.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let mut ports = std::collections::BTreeSet::new();
        for state in find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ config::anchor: attrs::NODE_SETTINGS }]),
            maximal(state, &order),
        )) {
            let values: Vec<_> = find!(port: u16,
                pattern!(&facts, [{ state @ attrs::mcp_port: ?port }])).collect();
            if values.is_empty() {
                return Ok(NodeSettings::default().mcp_port);
            }
            ports.extend(values);
        }
        Ok(if ports.len() == 1 { *ports.first().unwrap() } else { NodeSettings::default().mcp_port })
    }

    pub fn settings(&self) -> Result<NodeSettings> {
        let facts = self.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let states: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ config::anchor: attrs::NODE_SETTINGS }]),
            maximal(state, &order),
        ))
        .collect();
        if states.is_empty() {
            if exists!(pattern!(&facts, [{ _?state @ config::anchor: attrs::NODE_SETTINGS }])) {
                bail!("node settings have no interpretable current revision");
            }
            return Ok(NodeSettings::default());
        }
        let mut candidates = Vec::new();
        for state in states {
            let before = candidates.len();
            for (mcp_port, sync_enabled, maintenance_enabled) in find!(
                (port: u16, sync: bool, maintain: bool),
                pattern!(&facts, [{ state @ attrs::mcp_port: ?port,
                    attrs::sync_enabled: ?sync, attrs::maintenance_enabled: ?maintain }])
            ) {
                candidates.push(NodeSettings {
                    mcp_port,
                    sync_enabled,
                    maintenance_enabled,
                });
            }
            if candidates.len() == before {
                bail!(
                    "node settings revision {state:X} is incomplete; explicit settings migration or reconciliation required"
                );
            }
        }
        let first = candidates.first().context("no understood node settings")?;
        if candidates.iter().any(|value| value != first) {
            bail!(
                "node settings have conflicting current configurations; explicitly reconcile before activation"
            );
        }
        Ok(first.clone())
    }

    /// Exact current revision IDs, never a global or per-session selection.
    /// Resolving each ID remains fallible: unknown/conflicting settings are
    /// visible as unavailable rather than falling back to another workspace.
    pub fn workspace_ids(&self) -> Result<Vec<Id>> {
        let facts = self.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let mut ids: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::WORKSPACE,
                attrs::enrolled: true }]),
            maximal(state, &order),
        ))
        .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Resolve the requested revision at the point of use. Editing
    /// a workspace or changing its managed path/key withdraws old authority;
    /// an obsolete ID never redirects to a replacement revision.
    /// This only queries node facts; it does not open the managed pile/key.
    pub fn workspace(&self, revision: Id) -> Result<Option<Workspace>> {
        self.workspace_revision(revision)
    }

    /// Current revision as saved, including a binding whose enrollment moved
    /// or was withdrawn. For UI inspection/removal only; request dispatch
    /// must use `workspace`, which also checks the enrolled binding.
    pub fn workspace_revision(&self, revision: Id) -> Result<Option<Workspace>> {
        let facts = self.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let current = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::WORKSPACE,
                attrs::enrolled: true }]),
            state.is(revision.to_inline()), maximal(state, &order),
        ))
        .next()
        .is_some();
        if !current {
            return Ok(None);
        }
        // Ignoring old path bindings would silently retarget out-of-store data.
        if exists!(pattern!(&facts, [{ revision @ attrs::managed_pile: _?old }])) {
            bail!("legacy workspace {revision:X} requires explicit single-store migration");
        }
        let mut routes = std::collections::BTreeMap::new();
        for (role, root) in find!((role: Id, root: CollectionHandle),
            pattern!(&facts, [
                { revision @ attrs::workspace_route: _?route },
                { _?route @ config::anchor: ?role, config::selects: ?root },
            ])
        ) {
            if routes.insert(role, root).is_some_and(|old| old != root) {
                bail!("workspace {revision:X} has conflicting exact roots for role {role:X}");
            }
        }
        let mut candidates = Vec::new();
        for (anchor, label) in find!(
            (anchor: Id, label: Inline<inlineencodings::Handle<blobencodings::UTF8String>>),
            pattern!(&facts, [{ revision @ config::anchor: ?anchor, metadata::name: ?label }])
        ) {
            let heads: Vec<Id> = find!(state: Id, and!(
                pattern!(&facts, [{ ?state @ metadata::tag: attrs::WORKSPACE,
                    config::anchor: anchor }]), maximal(state, &order),
            ))
            .collect();
            if heads.iter().any(|head| *head != revision)
                || exists!(pattern!(&facts, [{ revision @ attrs::enrolled: false }]))
            {
                bail!(
                    "workspace {revision:X} has concurrent revisions; explicitly reconcile before use"
                );
            }
            let value = Workspace {
                id: revision,
                anchor,
                label: self.text(label)?,
                routes: routes.clone(),
            };

            if !candidates.contains(&value) {
                candidates.push(value);
            }
        }
        if candidates.len() != 1 {
            bail!("workspace {revision:X} has no unambiguous understood binding");
        }
        Ok(candidates.pop())
    }
}

pub struct NodeStore {
    pub(crate) pile: Pile,
    pub(crate) signer: SigningKey,
    pub(crate) config: Collection<blobencodings::SimpleArchive>,
    pub(crate) metrics: Collection<blobencodings::SimpleArchive>,
    last_config: Option<ConfigView>,
}

impl NodeStore {
    /// Open only an already-prepared node pile. The passed signer is the
    /// bootstrap key, not an ambient environment variable or a managed key.
    pub fn open(path: &Path, signer: SigningKey) -> Result<Self> {
        let pile = faculties::storage::open_pile_strict_as(path, signer.verifying_key())?;
        Self::from_pile(pile, signer)
    }

    pub(crate) fn from_pile(mut pile: Pile, signer: SigningKey) -> Result<Self> {
        let config = faculties::collection_names::open(
            &mut pile,
            faculties::schemas::config::DEFAULT_SCOPE_ID,
            signer.verifying_key(),
        )?;
        debug_assert_eq!(
            config.handle(),
            faculties::config::handle(signer.verifying_key())
        );
        let metrics = pile.collection(
            "erlkonig-metrics",
            faculties::collection_names::private_policy(signer.verifying_key()),
        )?;
        Ok(Self {
            pile,
            signer,
            config,
            metrics,
            last_config: None,
        })
    }

    pub fn config(&mut self) -> Result<ConfigView> {
        self.pile.refresh().context("refresh node pile")?;
        let snapshot = self.pile.snapshot()?;
        let view = ConfigView {
            observed: snapshot.collection_acquiring(self.config)?,
            text_reads: Default::default(),
        };
        // Demand the native facts now, so an unavailable observation never
        // looks like an empty-but-valid configuration to the controller.
        view.facts()?;
        Ok(view)
    }

    pub fn config_changed(&mut self, view: &ConfigView) -> Result<bool> {
        if view.observed.cover().collection().handle() != self.config.handle() {
            bail!("configuration observation belongs to a different node");
        }
        self.pile.refresh().context("refresh node pile")?;
        Ok(!view.is_current(&self.pile.snapshot()?))
    }

    pub fn poll_config(&mut self) -> Result<Option<ConfigView>> {
        // Any failed observation clears the last successful delivery. After
        // recovery even unchanged settings must be presented again, because
        // the controller stopped services when configuration became unknown.
        let previous = self.last_config.take();
        self.pile.refresh().context("refresh node pile")?;
        let snapshot = self.pile.snapshot()?;
        if previous
            .as_ref()
            .is_some_and(|view| view.is_current(&snapshot))
        {
            self.last_config = previous;
            return Ok(None);
        }
        let view = ConfigView {
            observed: snapshot.collection_acquiring(self.config)?,
            text_reads: Default::default(),
        };
        view.facts()?;
        self.last_config = Some(view.clone());
        Ok(Some(view))
    }

    pub fn put_mcp_port(&mut self, expected: &ConfigView, port: u16) -> Result<Id> {
        let mut value = expected.settings()?;
        value.mcp_port = port;
        self.put_settings(expected, &value)
    }

    /// A stale-view guard, not CAS: racing writers remain concurrent heads.
    pub fn put_settings(&mut self, expected: &ConfigView, value: &NodeSettings) -> Result<Id> {
        if self.config_changed(expected)? {
            bail!("node configuration changed since this edit was opened; reload before saving");
        }
        let facts = expected.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let previous: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ config::anchor: attrs::NODE_SETTINGS }]),
            maximal(state, &order),
        ))
        .collect();
        let fragment = entity! {
            config::anchor: attrs::NODE_SETTINGS,
            attrs::mcp_port: value.mcp_port,
            attrs::sync_enabled: value.sync_enabled,
            attrs::maintenance_enabled: value.maintenance_enabled,
            metadata::supersedes*: previous,
        };
        let id = fragment.root().expect("settings entity has a root");
        self.pile.commit(self.config, &self.signer, fragment)?;
        self.pile.flush()?;
        Ok(id)
    }

    /// Save only after explicit UI action. The exact binding and routes are
    /// facts in this revision; no managed-pile settings are changed here.
    /// Create a usable private bundle only on Save. All descriptors are built
    /// in memory and travel in the one configuration publication; cancellation
    /// and validation failures append nothing. Label edits never rename roots.
    pub fn create_workspace(&mut self, expected: &ConfigView, label: &str) -> Result<Id> {
        if label.trim().is_empty() {
            bail!("workspace name is required");
        }
        if self.config_changed(expected)? {
            bail!("node configuration changed since this edit was opened; reload before saving");
        }
        let workspace = Workspace::new(label.trim());
        let mut staging = triblespace::core::repo::memoryrepo::MemoryStore::default();
        let mut routes = Fragment::empty();
        let mut roots = Vec::new();
        for (role, name) in faculties::collection_names::table() {
            use triblespace::core::capability::policy;
            use triblespace::core::collection::records as descriptor;
            // The opaque bundle tag gives equally named bundles separate
            // identity; the readable name need not carry a protocol ID.
            let collection: Collection<blobencodings::SimpleArchive> = staging.register_collection(entity! {
                metadata::tag*: [descriptor::KIND_COLLECTION_DESCRIPTOR, workspace.anchor],
                descriptor::collection_name: format!("{} · {}", label.trim(), name),
                attrs::collection_role: role,
                policy::resource_policy*: faculties::collection_names::private_policy(self.signer.verifying_key()).fragment(),
                descriptor::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
            })?;
            roots.push(collection.handle());
            routes += entity! { config::anchor: role, config::selects: collection.handle() };
        }
        let mut fragment = entity! {
            metadata::tag: attrs::WORKSPACE, config::anchor: workspace.anchor,
            attrs::enrolled: true, metadata::name: workspace.label,
            attrs::workspace_route*: routes,
        };
        fragment.blobs_mut().union(staging.blobs);
        let id = fragment.root().expect("workspace revision has a root");
        self.pile.commit(self.config, &self.signer, fragment)?;
        self.pile.flush()?;
        self.select_requested_roots(roots).with_context(|| {
            format!("workspace {id:X} was published; native sync selection failed; reinspect before retrying")
        })?;
        Ok(id)
    }

    /// Descriptor-only bundle members are discovery seeds, not permissions.
    pub fn workspace_roots(&mut self) -> Result<Vec<CollectionHandle>> {
        let facts = self.config()?.facts()?;
        Ok(find!(root: CollectionHandle, pattern!(&facts, [
            { _?workspace @ metadata::tag: attrs::WORKSPACE, attrs::workspace_route: _?route },
            { _?route @ config::selects: ?root },
        ]))
        .collect())
    }

    /// Current understood saved bundles, including accepted enrollments.
    /// Neither historical routes nor arbitrary admitted grants are selected.
    pub fn enrolled_roots(&mut self) -> Result<Vec<CollectionHandle>> {
        let view = self.config()?;
        let mut roots = std::collections::BTreeSet::new();
        for revision in view.workspace_ids()? {
            if let Ok(Some(workspace)) = view.workspace(revision) {
                roots.extend(workspace.routes.values().copied());
            }
        }
        Ok(roots.into_iter().collect())
    }

    /// Bootstrap legacy bundles only where no native choice exists. A manual
    /// unselection or concurrent native fork is never implicitly overwritten.
    pub fn initialize_workspace_sync(&mut self) -> Result<usize> {
        use triblespace::core::collection::selection::{self, Selection};
        let roots = self.enrolled_roots()?;
        let mut written = 0;
        for root in roots {
            let facts = selection::config_facts(&self.pile.snapshot()?, self.signer.verifying_key())?;
            if selection::sync_selection(&facts, root) == Selection::Unset {
                selection::write_sync_selection(&mut self.pile, self.config, &self.signer, root, true)?;
                written += 1;
            }
        }
        if written > 0 { self.pile.flush()?; }
        Ok(written)
    }

    /// An explicit bundle action selects its newly requested roots AFTER the
    /// workspace commit. Failure does not roll back that earlier publication.
    fn select_requested_roots(&mut self, roots: impl IntoIterator<Item = CollectionHandle>) -> Result<()> {
        use triblespace::core::collection::selection::{self, Selection};
        for root in roots.into_iter().collect::<std::collections::BTreeSet<_>>() {
            let facts = selection::config_facts(&self.pile.snapshot()?, self.signer.verifying_key())?;
            if selection::sync_selection(&facts, root) != Selection::Selected {
                selection::write_sync_selection(&mut self.pile, self.config, &self.signer, root, true)?;
            }
        }
        self.pile.flush()?;
        Ok(())
    }

    /// The daemon publishes reachable local addresses, not bootstrap peers.
    /// Rebinding supersedes old hints; an unchanged observation appends nothing.
    pub(crate) fn record_sync_addresses(&mut self, bound: &[std::net::SocketAddr]) -> Result<bool> {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
        use triblespace::core::collection::selection;
        let addresses = bound.iter().map(|socket| {
            let ip = match socket.ip() {
                IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
                ip => ip,
            };
            SocketAddr::new(ip, socket.port())
        }).collect::<std::collections::BTreeSet<_>>();
        let endpoint = self.signer.verifying_key();
        let facts = selection::config_facts(&self.pile.snapshot()?, endpoint)?;
        if selection::sync_addresses(&facts, endpoint) == addresses { return Ok(false); }
        selection::write_sync_addresses(&mut self.pile, self.config, &self.signer, endpoint, addresses)?;
        self.pile.flush()?;
        Ok(true)
    }

    pub(crate) fn enable_workspace_erp(&mut self, expected: &ConfigView, revision: Id) -> Result<Id> {
        let mut workspace = expected.workspace(revision)?
            .context("workspace is no longer current; inspect its latest revision")?;
        let role = faculties_erp::bookkeeping::DEFAULT_SCOPE_ID;
        if workspace.routes.contains_key(&role) {
            bail!("workspace already has an ERP route; inspect the current binding");
        }
        let mut staging = triblespace::core::repo::memoryrepo::MemoryStore::default();
        use triblespace::core::capability::policy;
        use triblespace::core::collection::records as descriptor;
        // A fresh descriptor entity prevents reusing an older root or its
        // grants when the same workspace opts in after a route was removed.
        let descriptor_id = genid();
        let collection: Collection<blobencodings::SimpleArchive> = staging.register_collection(entity! {
            &descriptor_id @
            metadata::tag*: [descriptor::KIND_COLLECTION_DESCRIPTOR, workspace.anchor],
            descriptor::collection_name: format!("{} · erp", workspace.label),
            attrs::collection_role: role,
            policy::resource_policy*: faculties::collection_names::private_policy(self.signer.verifying_key()).fragment(),
            descriptor::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
        })?;
        let root = collection.handle();
        workspace.routes.insert(role, root);
        self.publish_workspace(expected, &workspace, staging)
    }

    pub fn put_workspace(&mut self, expected: &ConfigView, value: &Workspace) -> Result<Id> {
        self.publish_workspace(expected, value, Default::default())
    }

    fn publish_workspace(
        &mut self,
        expected: &ConfigView,
        value: &Workspace,
        staging: triblespace::core::repo::memoryrepo::MemoryStore,
    ) -> Result<Id> {
        if self.config_changed(expected)? {
            bail!("node configuration changed since this edit was opened; reload before saving");
        }
        if value.label.trim().is_empty() {
            bail!("workspace name is required");
        }
        let facts = expected.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let previous: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::WORKSPACE,
                config::anchor: value.anchor }]), maximal(state, &order),
        ))
        .collect();
        let mut previous_roots = std::collections::BTreeSet::new();
        for revision in &previous {
            if let Ok(Some(workspace)) = expected.workspace_revision(*revision) {
                previous_roots.extend(workspace.routes.values().copied());
            }
        }
        let requested_roots = value.routes.values().copied()
            .filter(|root| !previous_roots.contains(root)).collect::<Vec<_>>();
        let mut routes = Fragment::empty();
        for (role, root) in &value.routes {
            routes += entity! {
                config::anchor: *role, config::selects: *root,
            };
        }
        let mut fragment = entity! {
            metadata::tag: attrs::WORKSPACE, config::anchor: value.anchor,
            attrs::enrolled: true, metadata::name: value.label.clone(),
            attrs::workspace_route*: routes, metadata::supersedes*: previous,
        };
        fragment.blobs_mut().union(staging.blobs);
        let id = fragment.root().expect("workspace revision has a root");
        self.pile.commit(self.config, &self.signer, fragment)?;
        self.pile.flush()?;
        self.select_requested_roots(requested_roots).with_context(|| {
            format!("workspace {id:X} was published; native sync selection failed; reinspect before retrying")
        })?;
        Ok(id)
    }

    pub fn remove_workspace(&mut self, expected: &ConfigView, anchor: Id) -> Result<Id> {
        if self.config_changed(expected)? {
            bail!("node configuration changed since this edit was opened; reload before saving");
        }
        let facts = expected.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let previous: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::WORKSPACE,
                config::anchor: anchor }]), maximal(state, &order),
        ))
        .collect();
        if previous.is_empty() {
            bail!("workspace {anchor:X} has no revision to remove");
        }
        let fragment = entity! {
            metadata::tag: attrs::WORKSPACE, config::anchor: anchor,
            attrs::enrolled: false, metadata::supersedes*: previous,
        };
        let id = fragment.root().expect("workspace withdrawal has a root");
        self.pile.commit(self.config, &self.signer, fragment)?;
        self.pile.flush()?;
        Ok(id)
    }

    /// Withdraw one selected current revision even when its complete binding
    /// cannot be interpreted. Other concurrent revisions remain separately
    /// visible; no target pile, key, route or signer must be reconstructed.
    pub fn remove_workspace_revision(&mut self, expected: &ConfigView, revision: Id) -> Result<Id> {
        if self.config_changed(expected)? {
            bail!("node configuration changed since this edit was opened; reload before saving");
        }
        let facts = expected.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let current = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::WORKSPACE,
                attrs::enrolled: true }]),
            state.is(revision.to_inline()), maximal(state, &order),
        ))
        .next()
        .is_some();
        if !current {
            bail!("workspace revision {revision:X} is no longer current");
        }
        let anchors: Vec<Id> = find!(anchor: Id,
            pattern!(&facts, [{ revision @ config::anchor: ?anchor }])
        )
        .collect();
        let fragment = entity! {
            metadata::tag: attrs::WORKSPACE, config::anchor*: anchors,
            attrs::enrolled: false, metadata::supersedes: revision,
        };
        let id = fragment.root().expect("workspace withdrawal has a root");
        self.pile.commit(self.config, &self.signer, fragment)?;
        self.pile.flush()?;
        Ok(id)
    }

    pub fn close(self) -> Result<()> {
        self.pile.close().context("close node pile")
    }
}

#[cfg(test)]
mod tests;
