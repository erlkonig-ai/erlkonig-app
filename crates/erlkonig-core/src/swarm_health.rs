//! The node owner's one explicit destination for native peer observations.

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use faculties::schemas::config::config;
use std::collections::BTreeSet;
use triblespace::core::{
    clock::{epoch_now, mono_now},
    collection::{Collection, CollectionHandle, CollectionStoreExt, descriptor},
    metadata,
    query::register::{ObservationOrder, maximal},
    repo::{BlobStoreGet, StorageFlush, pile::PileSnapshot},
};
use triblespace::prelude::*;
use triblespace_net::{
    health::HealthSnapshot,
    health_record::{self, Recorder},
};

use crate::{
    node_schema::HEALTH_REPORTING,
    node_store::{ConfigView, NodeStore},
};

fn revisions(facts: &TribleSet) -> Vec<Id> {
    let order = ObservationOrder::new(facts, metadata::supersedes.id());
    find!(revision: Id, and!(
        pattern!(facts, [{ ?revision @ config::anchor: HEALTH_REPORTING }]),
        maximal(revision, &order),
    ))
    .collect()
}

impl ConfigView {
    pub(crate) fn health_destination(&self) -> Result<Option<CollectionHandle>> {
        let facts = self.facts()?;
        let mut choices = BTreeSet::new();
        for revision in revisions(&facts) {
            let mut roots = find!(root: CollectionHandle,
                pattern!(&facts, [{ revision @ config::selects: ?root }]))
            .peekable();
            if roots.peek().is_none() {
                choices.insert(None);
            }
            choices.extend(roots.map(Some));
        }
        match choices.len() {
            0 | 1 => Ok(choices.into_iter().next().flatten()),
            _ => bail!(
                "conflicting node health destinations; explicitly select one or disable reporting"
            ),
        }
    }
}

fn destination(
    snapshot: &PileSnapshot,
    handle: CollectionHandle,
    signer: VerifyingKey,
) -> Result<Collection<blobencodings::SimpleArchive>> {
    let collection =
        Collection::open(snapshot, handle).context("open native health destination")?;
    let facts: TribleSet = snapshot.get(handle)?;
    if descriptor::source(&facts)?.is_some() {
        bail!("health destination must be a source collection");
    }
    if !collection.writer_is_admitted(snapshot, signer)? {
        bail!("node key lacks WRITE admission to the health destination");
    }
    Ok(collection)
}

impl NodeStore {
    pub(crate) fn set_health_destination(
        &mut self,
        expected: &ConfigView,
        root: Option<CollectionHandle>,
    ) -> Result<()> {
        if self.config_changed(expected)? {
            bail!("node configuration changed since inspection");
        }
        if let Some(handle) = root {
            destination(
                expected.observed.snapshot(),
                handle,
                self.signer.verifying_key(),
            )?;
        }
        if expected.health_destination().is_ok_and(|current| current == root) {
            return Ok(());
        }
        let previous = revisions(&expected.facts()?);
        self.pile.commit(
            self.config,
            &self.signer,
            entity! {
                config::anchor: HEALTH_REPORTING,
                config::selects?: root,
                metadata::supersedes*: previous,
            },
        )?;
        self.pile.flush()?;
        Ok(())
    }
}

/// Keep episode continuity within one destination and owner session.
#[derive(Default)]
pub(crate) struct Reporter {
    active: Option<(CollectionHandle, Recorder)>,
}

impl Reporter {
    pub(crate) fn publish(
        &mut self,
        node: &mut NodeStore,
        health: Option<&HealthSnapshot>,
    ) -> Result<()> {
        let view = node.config()?;
        let Some(root) = view.health_destination()? else {
            self.active = None;
            return Ok(());
        };
        let signer = node.signer.verifying_key();
        let collection = destination(view.observed.snapshot(), root, signer)?;
        // Restore the updated recorder only after successful publication. A
        // failed write must never seed the next sample's recovery episode.
        let previous = self.active.take().filter(|(old, _)| *old == root);
        let fresh = previous.is_none();
        let mut recorder = previous
            .map(|(_, recorder)| recorder)
            .unwrap_or_else(|| Recorder::new(signer));
        let mut report = match health {
            Some(health) => recorder.record_measurements(
                epoch_now(),
                health_record::measurements(health, mono_now()),
            )?,
            None => recorder.record(
                epoch_now(),
                [health_record::Condition {
                    component: health_record::Component::Host,
                    collection: None,
                    peer: None,
                    state: health_record::State::Unknown,
                    alert: false,
                }],
            )?,
        };
        if fresh {
            report += health_record::vocabulary();
        }
        node.pile.commit(collection, &node.signer, report)?;
        node.pile.flush()?;
        self.active = Some((root, recorder));
        Ok(())
    }
}
