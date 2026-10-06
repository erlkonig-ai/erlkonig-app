//! Typed configuration and observations in the application-owned node pile.
//!
//! Only the bootstrap pile/key paths live outside the pile. Configuration is
//! read from the existing Faculty private `config` collection. The store keeps
//! an immutable collection observation for invalidation, not a configuration
//! cache or an inventory of other piles. Nothing here discovers, opens, creates
//! or takes ownership of a managed pile or its key.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use ed25519_dalek::{SigningKey, VerifyingKey};
use faculties::schemas::config::config;
use triblespace::core::collection::{
    Collection, CollectionHandle, CollectionSnapshot, CollectionSnapshotExt, CollectionStoreExt,
};
use triblespace::core::metadata;
use triblespace::core::query::register::{ObservationOrder, maximal};
use triblespace::core::repo::pile::{Pile, PileSnapshot};
use triblespace::core::repo::{
    BlobStoreGet, SnapshotSource, StorageFlush, StoreChanges, StoreDependencies, StoreSnapshot,
};
use triblespace::prelude::*;
use triblespace_net::health_record;

use crate::node_schema as attrs;

/// An edit request or one effective entry projection, never a store mirror.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedPile {
    pub id: Id,
    pub label: String,
    pub pile: PathBuf,
    pub key: PathBuf,
    pub sync_enabled: bool,
    pub maintenance_enabled: bool,
    pub peers: Vec<[u8; 32]>,
    pub sync_collections: Vec<CollectionHandle>,
    pub maintain_targets: Vec<CollectionHandle>,
}

impl ManagedPile {
    /// Enrolling a pile never enables either service implicitly.
    pub fn new(label: impl Into<String>, pile: PathBuf, key: PathBuf) -> Self {
        Self {
            id: genid().id,
            label: label.into(),
            pile,
            key,
            sync_enabled: false,
            maintenance_enabled: false,
            peers: Vec::new(),
            sync_collections: Vec::new(),
            maintain_targets: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeSettings {
    pub mcp_port: u16,
}

impl Default for NodeSettings {
    fn default() -> Self {
        Self { mcp_port: 41773 }
    }
}

/// A frozen native observation. Query results are computed where requested.
#[derive(Clone)]
pub struct ConfigView {
    observed: CollectionSnapshot<PileSnapshot, blobencodings::SimpleArchive>,
    // Exact read interests, including failed text reads, never copied values.
    text_reads: Arc<Mutex<StoreDependencies>>,
}

impl ConfigView {
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

    pub fn managed_ids(&self) -> Result<Vec<Id>> {
        let facts = self.facts()?;
        let mut ids: Vec<Id> = find!(id: Id, pattern!(&facts, [{ _?state @
            metadata::tag: attrs::MANAGED_PILE,
            config::anchor: ?id,
        }]))
        .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Query one entry's maximal revisions. Equal concurrent values agree;
    /// different values remain a conflict, including multiple typed values
    /// on one revision. Extra facts and unfamiliar encodings coexist freely.
    /// The application cannot activate incomplete settings, but the data is
    /// not rejected or rewritten because it cannot yet be interpreted.
    pub fn managed(&self, id: Id) -> Result<Option<ManagedPile>> {
        let facts = self.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let states: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::MANAGED_PILE, config::anchor: id }]),
            maximal(state, &order),
        ))
        .collect();
        if states.is_empty() {
            // No declared revision differs from a declaration whose heads are
            // not interpretable (for example a supersession cycle).
            if exists!(
                pattern!(&facts, [{ _?state @ metadata::tag: attrs::MANAGED_PILE, config::anchor: id }])
            ) {
                bail!(
                    "managed pile {id:X} has no interpretable current revision; services remain off"
                );
            }
            return Ok(None);
        }
        let mut candidates = Vec::new();
        for state in states {
            let mut understood = false;
            let enrolled = exists!(pattern!(&facts, [{ state @ attrs::enrolled: true }]));
            let mut enrollment_understood = false;
            if exists!(pattern!(&facts, [{ state @ attrs::enrolled: false }])) {
                understood = true;
                if !candidates.contains(&None) {
                    candidates.push(None);
                }
            }
            let mut peers: Vec<[u8; 32]> = find!(peer: VerifyingKey,
                pattern!(&facts, [{ state @ health_record::attrs::peer: ?peer }]))
            .map(|peer| peer.to_bytes())
            .collect();
            peers.sort_unstable();
            peers.dedup();
            let mut sync_collections: Vec<CollectionHandle> = find!(handle: CollectionHandle,
                pattern!(&facts, [{ state @ attrs::sync_collection: ?handle }]))
            .collect();
            sync_collections.sort_unstable();
            sync_collections.dedup();
            let mut maintain_targets: Vec<CollectionHandle> = find!(handle: CollectionHandle,
                pattern!(&facts, [{ state @ attrs::maintenance_target: ?handle }]))
            .collect();
            maintain_targets.sort_unstable();
            maintain_targets.dedup();
            for (label, pile, key, sync_enabled, maintenance_enabled) in find!(
                (label: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
                 pile: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
                 key: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
                 sync_enabled: bool, maintenance_enabled: bool),
                pattern!(&facts, [{ state @ attrs::enrolled: true, metadata::name: ?label,
                    attrs::pile_path: ?pile, attrs::key_path: ?key,
                    attrs::sync_enabled: ?sync_enabled,
                    attrs::maintenance_enabled: ?maintenance_enabled,
                }])
            ) {
                understood = true;
                enrollment_understood = true;
                let label = self.text(label)?;
                let pile = self.text(pile)?;
                let key = self.text(key)?;
                if !Path::new(&pile).is_absolute() || !Path::new(&key).is_absolute() {
                    bail!(
                        "managed pile {id:X} revision {state:X} needs absolute pile and key paths; services remain off"
                    );
                }
                let value = Some(ManagedPile {
                    id,
                    label,
                    pile: pile.into(),
                    key: key.into(),
                    sync_enabled,
                    maintenance_enabled,
                    peers: peers.clone(),
                    sync_collections: sync_collections.clone(),
                    maintain_targets: maintain_targets.clone(),
                });
                if !candidates.contains(&value) {
                    candidates.push(value);
                }
            }
            if !understood || (enrolled && !enrollment_understood) {
                bail!(
                    "managed pile {id:X} revision {state:X} is incomplete or not understood; services remain off"
                );
            }
        }
        match candidates.len() {
            1 => Ok(candidates.pop().flatten()),
            n => bail!(
                "managed pile {id:X} has {n} conflicting current configurations; services remain off until explicitly reconciled"
            ),
        }
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
        let mut ports = Vec::new();
        for state in states {
            let values: Vec<u16> = find!(port: u16,
                pattern!(&facts, [{ state @ attrs::mcp_port: ?port }]))
            .collect();
            if values.is_empty() {
                bail!("node settings revision {state:X} has no understood MCP port");
            }
            ports.extend(values);
        }
        ports.sort_unstable();
        ports.dedup();
        match ports.as_slice() {
            [port] => Ok(NodeSettings { mcp_port: *port }),
            _ => bail!("node settings have conflicting MCP ports: {ports:?}"),
        }
    }
}

pub struct NodeStore {
    pub(crate) pile: Pile,
    pub(crate) signer: SigningKey,
    config: Collection<blobencodings::SimpleArchive>,
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

    /// An optimistic stale-view guard, not a CAS or distributed lock. A write
    /// racing after this check produces concurrent revisions, visibly, rather
    /// than superseding facts its author did not observe.
    pub fn put_managed(&mut self, expected: &ConfigView, value: &ManagedPile) -> Result<Id> {
        if self.config_changed(expected)? {
            bail!("node configuration changed since this edit was opened; reload before saving");
        }
        let pile = value
            .pile
            .to_str()
            .context("managed pile path is not UTF-8")?;
        let key = value
            .key
            .to_str()
            .context("managed key path is not UTF-8")?;
        if !value.pile.is_absolute() || !value.key.is_absolute() {
            bail!("managed pile and key paths must be explicit absolute paths");
        }
        let peers = value
            .peers
            .iter()
            .map(VerifyingKey::from_bytes)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let facts = expected.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let previous: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::MANAGED_PILE, config::anchor: value.id }]),
            maximal(state, &order),
        )).collect();
        let fragment = entity! {
            metadata::tag: attrs::MANAGED_PILE,
            config::anchor: value.id,
            attrs::enrolled: true,
            metadata::name: value.label.clone(),
            attrs::pile_path: pile.to_owned(),
            attrs::key_path: key.to_owned(),
            attrs::sync_enabled: value.sync_enabled,
            attrs::maintenance_enabled: value.maintenance_enabled,
            health_record::attrs::peer*: peers,
            attrs::sync_collection*: value.sync_collections.iter().copied(),
            attrs::maintenance_target*: value.maintain_targets.iter().copied(),
            metadata::supersedes*: previous,
        };
        let id = fragment.root().expect("configuration entity has a root");
        self.pile.commit(self.config, &self.signer, fragment)?;
        self.pile.flush()?;
        Ok(id)
    }

    /// Withdraw this entry explicitly, retaining all earlier revisions. A
    /// concurrent enrollment is a visible conflict, not an implicit re-add.
    pub fn remove_managed(&mut self, expected: &ConfigView, entry: Id) -> Result<Id> {
        if self.config_changed(expected)? {
            bail!("node configuration changed since this edit was opened; reload before saving");
        }
        let facts = expected.facts()?;
        let order = ObservationOrder::new(&facts, metadata::supersedes.id());
        let previous: Vec<Id> = find!(state: Id, and!(
            pattern!(&facts, [{ ?state @ metadata::tag: attrs::MANAGED_PILE, config::anchor: entry }]),
            maximal(state, &order),
        )).collect();
        if previous.is_empty() {
            bail!("managed pile {entry:X} has no current enrollment to withdraw");
        }
        let fragment = entity! {
            metadata::tag: attrs::MANAGED_PILE,
            config::anchor: entry,
            attrs::enrolled: false,
            metadata::supersedes*: previous,
        };
        let id = fragment.root().expect("withdrawal entity has a root");
        self.pile.commit(self.config, &self.signer, fragment)?;
        self.pile.flush()?;
        Ok(id)
    }

    pub fn put_mcp_port(&mut self, expected: &ConfigView, port: u16) -> Result<Id> {
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
            attrs::mcp_port: port,
            metadata::supersedes*: previous,
        };
        let id = fragment.root().expect("settings entity has a root");
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
