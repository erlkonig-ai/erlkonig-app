//! Native measurements and service failure observations in the node pile,
//! never in a managed data pile. Counter meanings and rate/freshness logic
//! are shared with `triblespace_net::telemetry`; absent measurements stay absent.
//! Service events are separate facts, not counter samples or a second log store.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use hifitime::Epoch;
use triblespace::core::collection::{CollectionSnapshotExt, CollectionStoreExt};
use triblespace::core::metadata;
use triblespace::core::repo::{BlobStoreGet, SnapshotSource, StorageFlush};
use triblespace::prelude::*;
use triblespace_net::{health_record, telemetry};

use crate::{node_schema, node_store::NodeStore};

pub use faculties::schemas::service_events::{KIND_SERVICE_EVENT, ServiceCode, ServicePhase};

/// One occurrence supplied at the actual transition, not reconstructed from
/// status frames. The caller owns an attempt ID created before startup, so its
/// initiating cause and independent cleanup failures can name the same attempt.
/// Clone an existing event if retrying publication; do not regenerate its time.
#[derive(Clone, Debug)]
pub struct ServiceEvent {
    endpoint: VerifyingKey,
    attempt: Id,
    observed_at: Inline<inlineencodings::NsTAIInterval>,
    elapsed_ns: u128,
    phase: ServicePhase,
    code: ServiceCode,
}

impl ServiceEvent {
    pub fn now(
        endpoint: VerifyingKey,
        attempt: Id,
        elapsed: Duration,
        phase: ServicePhase,
        code: ServiceCode,
    ) -> Result<Self> {
        let now = Epoch::now().context("read service event wall clock")?;
        Ok(Self {
            endpoint,
            attempt,
            observed_at: (now, now)
                .try_to_inline()
                .map_err(|error| anyhow::anyhow!("service event time: {error:?}"))?,
            elapsed_ns: elapsed.as_nanos(),
            phase,
            code,
        })
    }
}

/// Runtime aggregates one worker scope over a session, then batches these at
/// a bounded cadence (not UI frames). Counts are cumulative for that session.
#[derive(Clone, Debug)]
pub struct Sample {
    pub managed: Option<Id>,
    /// Actual managed worker's signing identity. In the maintenance role this
    /// identifies the local author, not a claim that a network listener exists.
    /// Required for managed scopes; process scopes may use the node identity.
    pub endpoint: Option<VerifyingKey>,
    pub worker: String,
    pub role: String,
    pub session: Id,
    pub observed_at: Inline<inlineencodings::NsTAIInterval>,
    pub elapsed_ns: u128,
    pub received_bytes: Option<u128>,
    pub sent_bytes: Option<u128>,
    /// Successful native maintenance publications, not blobs or bytes.
    pub publications: Option<u128>,
    pub failed: Option<u128>,
    /// Process CPU time across threads; emit only in a process role.
    pub cpu_ns: Option<u128>,
    /// An actual observed failure, not a success inferred from absence.
    pub error: Option<String>,
}

impl Sample {
    pub fn now(
        managed: Option<Id>,
        worker: &str,
        role: &str,
        session: Id,
        elapsed: Duration,
    ) -> Result<Self> {
        let now = Epoch::now().context("read observation wall clock")?;
        Ok(Self {
            managed,
            endpoint: None,
            worker: worker.into(),
            role: role.into(),
            session,
            observed_at: (now, now)
                .try_to_inline()
                .map_err(|error| anyhow::anyhow!("observation time: {error:?}"))?,
            elapsed_ns: elapsed.as_nanos(),
            received_bytes: None,
            sent_bytes: None,
            publications: None,
            failed: None,
            cpu_ns: None,
            error: None,
        })
    }
}

/// A presentation projection with all native telemetry ambiguity retained.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricReport {
    /// Repeated references stay repeated; never arbitrarily pick one.
    pub managed: Vec<Id>,
    /// The node identities that collected the observation, distinct from the
    /// measured worker identity in `report.node`.
    pub collectors: Vec<[u8; 32]>,
    pub report: telemetry::WorkerReport,
    pub errors: Vec<String>,
}

/// Current wall time in the exact TAI-nanosecond coordinate used by telemetry.
pub fn now_tai_ns() -> Result<i128> {
    Ok(Epoch::now()
        .context("read observation wall clock")?
        .to_tai_duration()
        .total_nanoseconds())
}

impl NodeStore {
    /// Publish a transition's immutable events directly, without the periodic
    /// sample coalescer. Success means native commit and flush both succeeded.
    /// Failure remains a caller-visible persistence problem, never a new event
    /// recursively published here. This does not change collection permissions
    /// or replication selection and does not claim crash durability on error.
    pub fn append_service_events(&mut self, events: &[ServiceEvent]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut batch = Fragment::empty();
        for event in events {
            let collector = entity! {
                health_record::attrs::endpoint: self.signer.verifying_key(),
            };
            let collector_id = collector.root().expect("collector has a root");
            let subject = entity! {
                health_record::attrs::endpoint: event.endpoint,
                health_record::attrs::node: collector_id,
                telemetry::attrs::worker: "native",
                telemetry::attrs::role: "services",
                telemetry::attrs::stage: event.phase.as_str(),
            };
            let subject_id = subject.root().expect("service event subject has a root");
            let occurrence = entity! {
                metadata::tag: KIND_SERVICE_EVENT,
                telemetry::attrs::subject: subject_id,
                health_record::attrs::session: event.attempt,
                metadata::created_at: event.observed_at,
                telemetry::attrs::elapsed_ns: event.elapsed_ns,
                metadata::name: event.code.as_str(),
            };
            // Wording annotates the occurrence; it is not part of its identity.
            let occurrence_id = occurrence.root().expect("service event has a root");
            batch += entity! { ExclusiveId::force_ref(&occurrence_id) @
                metadata::description: event.code.description(),
            };
            batch += collector;
            batch += subject;
            batch += occurrence;
        }
        self.pile
            .commit(self.metrics, &self.signer, batch)
            .context("append service events to node metrics")?;
        self.pile
            .flush()
            .context("flush service events to node metrics")?;
        Ok(())
    }

    /// One native commit for the whole batch. Callers own cadence and must not
    /// submit zero-filled substitutes for failed or unavailable observations.
    pub fn append_metrics(&mut self, samples: &[Sample]) -> Result<()> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut batch = Fragment::empty();
        for sample in samples {
            let endpoint = match (sample.managed, sample.endpoint) {
                (Some(_), None) => {
                    bail!("managed telemetry requires the measured worker signing identity")
                }
                (_, Some(endpoint)) => endpoint,
                (None, None) => self.signer.verifying_key(),
            };
            // ShortString is the existing telemetry vocabulary. Reject an
            // unsuitable producer label before constructing any publication.
            if sample.worker.len() > 32
                || sample.role.len() > 32
                || sample.worker.as_bytes().contains(&0)
                || sample.role.as_bytes().contains(&0)
            {
                bail!("telemetry worker and role labels must fit 32 UTF-8 bytes without NUL");
            }
            if sample.cpu_ns.is_some() && (sample.role != "process" || sample.managed.is_some()) {
                bail!(
                    "process CPU must be reported in a process scope, not duplicated per managed pile"
                );
            }
            let collector = entity! {
                health_record::attrs::endpoint: self.signer.verifying_key(),
            };
            let collector_id = collector.root().expect("collector has a root");
            let subject = entity! {
                health_record::attrs::endpoint: endpoint,
                health_record::attrs::node: collector_id,
                telemetry::attrs::worker: sample.worker.as_str(),
                telemetry::attrs::role: sample.role.as_str(),
                node_schema::managed_pile?: sample.managed,
            };
            batch += collector;
            let subject_id = subject.root().expect("measurement subject has a root");
            let measurement = entity! {
                metadata::tag: telemetry::KIND_SAMPLE,
                telemetry::attrs::subject: subject_id,
                health_record::attrs::session: sample.session,
                metadata::created_at: sample.observed_at,
                telemetry::attrs::elapsed_ns: sample.elapsed_ns,
                telemetry::attrs::received_bytes?: sample.received_bytes,
                telemetry::attrs::sent_bytes?: sample.sent_bytes,
                telemetry::attrs::completed?: sample.publications,
                telemetry::attrs::failed?: sample.failed,
                telemetry::attrs::cpu_ns?: sample.cpu_ns,
                metadata::description?: sample.error.clone(),
            };
            batch += subject;
            batch += measurement;
        }
        self.pile.commit(self.metrics, &self.signer, batch)?;
        self.pile.flush()?;
        Ok(())
    }

    pub fn metrics(&mut self, now_tai_ns: i128, max_age: Duration) -> Result<Vec<MetricReport>> {
        self.pile.refresh()?;
        let snapshot = self.pile.snapshot()?;
        let observed = snapshot.collection_acquiring(self.metrics)?;
        let facts: TribleSet = observed.view()?;
        telemetry::observe(&facts, now_tai_ns, max_age)
            .into_iter()
            .map(|report| {
                let mut managed: Vec<Id> = find!(entry: Id,
                pattern!(&facts, [{ report.subject @ node_schema::managed_pile: ?entry }]))
                .collect();
                managed.sort_unstable();
                managed.dedup();
                let mut collectors: Vec<[u8; 32]> = find!(collector: VerifyingKey,
                pattern!(&facts, [
                    { report.subject @ health_record::attrs::node: _?node },
                    { _?node @ health_record::attrs::endpoint: ?collector },
                ]))
                .map(|key| key.to_bytes())
                .collect();
                collectors.sort_unstable();
                collectors.dedup();
                let mut errors = Vec::new();
                for text in find!(text: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
                pattern!(&facts, [{ report.report @ metadata::description: ?text }]))
                {
                    let text: View<str> = snapshot.get(text)?;
                    errors.push(text.to_string());
                }
                errors.sort_unstable();
                errors.dedup();
                Ok(MetricReport {
                    managed,
                    collectors,
                    report,
                    errors,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
