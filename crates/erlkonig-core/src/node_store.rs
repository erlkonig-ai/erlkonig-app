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
    Collection, CollectionHandle, CollectionSnapshot, CollectionSnapshotExt, CollectionStoreExt,
};
use triblespace::core::metadata::{self, MetaDescribe};
use triblespace::core::query::register::{ObservationOrder, maximal};
use triblespace::core::repo::pile::{Pile, PileSnapshot};
use triblespace::core::repo::{
    BlobStoreGet, SnapshotSource, StorageFlush, StoreChanges, StoreDependencies, StoreSnapshot,
};
use triblespace::prelude::*;
use triblespace_net::health_record;

use crate::node_schema as attrs;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeSettings {
    pub mcp_port: u16,
    pub sync_enabled: bool,
    pub maintenance_enabled: bool,
    pub peers: Vec<[u8; 32]>,
}

impl Default for NodeSettings {
    fn default() -> Self {
        Self {
            mcp_port: 41773,
            sync_enabled: true,
            maintenance_enabled: true,
            peers: Vec::new(),
        }
    }
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
    fn text(
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

    fn is_current(&self, snapshot: &PileSnapshot) -> bool {
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
            let mut peers: Vec<_> = find!(peer: VerifyingKey,
                pattern!(&facts, [{ state @ health_record::attrs::peer: ?peer }]))
            .map(|key| key.to_bytes())
            .collect();
            peers.sort_unstable();
            peers.dedup();
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
                    peers: peers.clone(),
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
        let mut pile = faculties::storage::open_pile_strict_as(path, signer.verifying_key())?;
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
            health_record::attrs::peer*: value.peers.iter().map(VerifyingKey::from_bytes).collect::<std::result::Result<Vec<_>,_>>()?,
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
        for (role, name) in crate::workspace::roles() {
            use triblespace::core::capability::policy;
            use triblespace::core::collection::records as descriptor;
            // The opaque bundle tag gives equally named bundles separate
            // identity; the readable name need not carry a protocol ID.
            let collection: Collection<blobencodings::SimpleArchive> = staging.register_collection(entity! {
                metadata::tag*: [descriptor::KIND_COLLECTION_DESCRIPTOR, workspace.anchor],
                descriptor::collection_name: format!("{} · {}", label.trim(), name),
                policy::resource_policy*: faculties::collection_names::private_policy(self.signer.verifying_key()).fragment(),
                descriptor::collection_representation*: <blobencodings::SimpleArchive as MetaDescribe>::describe(),
            })?;
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

    pub fn put_workspace(&mut self, expected: &ConfigView, value: &Workspace) -> Result<Id> {
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
        let mut routes = Fragment::empty();
        for (role, root) in &value.routes {
            routes += entity! {
                config::anchor: *role, config::selects: *root,
            };
        }
        let fragment = entity! {
            metadata::tag: attrs::WORKSPACE, config::anchor: value.anchor,
            attrs::enrolled: true, metadata::name: value.label.clone(),
            attrs::workspace_route*: routes, metadata::supersedes*: previous,
        };
        let id = fragment.root().expect("workspace revision has a root");
        self.pile.commit(self.config, &self.signer, fragment)?;
        self.pile.flush()?;
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
