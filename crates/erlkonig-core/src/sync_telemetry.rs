//! Operator-selected aggregate telemetry from the node's sync worker.

use anyhow::{Context, Result, bail};
use faculties::schemas::config::config;
use std::collections::BTreeSet;
use triblespace::core::{
    collection::{CollectionHandle, CollectionStoreExt},
    metadata,
    repo::StorageFlush,
};
use triblespace::prelude::*;
use triblespace_net::{
    health::HealthSnapshot,
    telemetry::{attrs::worker, sync::Publisher},
};

use crate::{
    node_schema::SYNC_TELEMETRY,
    node_store::{ConfigView, NodeStore, report_destination, reporting_revisions},
};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Destination {
    pub root: CollectionHandle,
    pub worker: String,
}

impl ConfigView {
    pub(crate) fn sync_telemetry(&self) -> Result<Option<Destination>> {
        let facts = self.facts()?;
        let mut choices = BTreeSet::new();
        for revision in reporting_revisions(&facts, SYNC_TELEMETRY) {
            let values = find!((root: CollectionHandle, label: Inline<inlineencodings::Handle<blobencodings::UTF8String>>),
                pattern!(&facts, [{ revision @ config::selects: ?root, worker: ?label }]));
            let mut values = values.peekable();
            if values.peek().is_none() {
                if exists!(pattern!(&facts, [{ revision @ config::selects: _?root }]))
                    || exists!(pattern!(&facts, [{ revision @ worker: _?label }]))
                {
                    bail!("sync telemetry requires both a destination and a worker label");
                }
                choices.insert(None);
            }
            for (root, label) in values {
                choices.insert(Some(Destination {
                    root,
                    worker: self
                        .text(label)
                        .context("sync telemetry worker label is unavailable")?,
                }));
            }
        }
        match choices.len() {
            0 | 1 => Ok(choices.into_iter().next().flatten()),
            _ => bail!(
                "conflicting sync telemetry settings; choose a destination and worker or disable reporting"
            ),
        }
    }
}

impl NodeStore {
    pub(crate) fn set_sync_telemetry(
        &mut self,
        expected: &ConfigView,
        target: Option<&Destination>,
    ) -> Result<()> {
        if self.config_changed(expected)? {
            bail!("node configuration changed since inspection");
        }
        if let Some(target) = target {
            Publisher::open(&mut self.pile, &self.signer, target.root, &target.worker)?;
        }
        if expected
            .sync_telemetry()
            .is_ok_and(|current| current.as_ref() == target)
        {
            return Ok(());
        }
        let previous = reporting_revisions(&expected.facts()?, SYNC_TELEMETRY);
        self.pile.commit(
            self.config,
            &self.signer,
            entity! {
                config::anchor: SYNC_TELEMETRY,
                config::selects?: target.map(|target| target.root),
                worker?: target.map(|target| target.worker.as_str()),
                metadata::supersedes*: previous,
            },
        )?;
        self.pile.flush()?;
        Ok(())
    }
}

/// Preserve counters only within the same configured destination and worker.
#[derive(Default)]
pub(crate) struct Reporter {
    active: Option<(Destination, Publisher)>,
}

impl Reporter {
    pub(crate) fn publisher(&mut self) -> Option<&mut Publisher> {
        self.active.as_mut().map(|(_, publisher)| publisher)
    }

    pub(crate) fn publish(&mut self, node: &mut NodeStore, health: &HealthSnapshot) -> Result<()> {
        let previous = self.active.take();
        let view = node.config()?;
        let Some(target) = view.sync_telemetry()? else {
            return Ok(());
        };
        let publisher = match previous {
            Some((old, publisher)) if old == target => {
                report_destination(
                    view.observed.snapshot(),
                    target.root,
                    node.signer.verifying_key(),
                )?;
                publisher
            }
            _ => Publisher::open(&mut node.pile, &node.signer, target.root, &target.worker)?,
        };
        let result = publisher.publish(&mut node.pile, health);
        self.active = Some((target, publisher));
        Ok(result?)
    }
}
