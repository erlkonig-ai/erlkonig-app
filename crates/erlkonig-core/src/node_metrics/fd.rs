//! Typed, private node metrics for bounded FD exhaustion diagnostics.
//! Anchors were minted with installed `trible genid` on 2026-10-21, with
//! subsystem additions on 2026-10-26 as marked below.
//! These are attribute anchors, not manually invented or reconstructed IDs.

use super::*;
use crate::fd_diagnostics::{Batch, CAPACITY, FD_CAPACITY, TYPE_NAMES};
use triblespace::macros::id_hex;

pub(crate) mod reader;

pub const KIND_FD_EXHAUSTION: Id = id_hex!("E46B35F4FCEFEB58C3940AC16CA0FB58");
pub const KIND_FD_CAPTURE_LOSS: Id = id_hex!("391CF649FDAB35582A0480D562DEB444");

attributes! {
    /// Error-handler subsystem, not the owner of descriptors causing pressure.
    /// Anchor minted with installed trible genid on 2026-10-26.
    "0B58D7745C062B3B8C683BB9B0B2BCFB" as pub error_site: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Live accepted stream wrappers of this app's MCP HTTP endpoint; not
    /// atomic with census and not requests, permits, backlog or all sockets.
    /// Anchor minted with installed trible genid on 2026-10-26.
    "FF56A3D4AD9759C01D80159F1FE63FD8" as pub mcp_http_accepted_streams: inlineencodings::U256BE;
    /// Occurrence sequence within this diagnostic collector lifetime.
    "8AA5061CBA0DFF2F49090BE0D4425141" as pub sequence: inlineencodings::U256BE;
    /// Error-handler snapshot, after upstream error conversion/unwinding.
    "7A75C5C433339759DFC4E4BA22AFB6FA" as pub handler_snapshot: inlineencodings::GenId;
    /// Maximum complete controller census within 60 monotonic seconds, not a peak.
    "19D2EA3B3CA609094495EEA0F754552B" as pub sampled_high_water: inlineencodings::GenId;
    /// Actual getrlimit(RLIMIT_NOFILE) soft value; RLIM_INFINITY is preserved.
    "3096C595F1357C39C6238F498CB540CB" as pub soft_limit: inlineencodings::U256BE;
    /// Actual getrlimit(RLIMIT_NOFILE) hard value; RLIM_INFINITY is preserved.
    "5822670F6590C2D522EB9907A8102100" as pub hard_limit: inlineencodings::U256BE;
    /// Number of returned FD-table entries, a lower bound when incomplete.
    "D50A0156001FF5036F41CC6C4561AA77" as pub fd_count: inlineencodings::U256BE;
    /// One allowlisted type/count aggregate belonging to a census.
    "C439C65D88D3E50FDF5E1D87B995D9A5" as pub fd_type: inlineencodings::GenId;
    /// Count of returned entries in one FD type bucket (including unknown).
    "7DCF8093F282559A9737E96906669F0A" as pub type_count: inlineencodings::U256BE;
    /// List call succeeded below capacity; not an atomic process-wide instant.
    "613427C824EFADE7B43851BD914DB564" as pub census_complete: inlineencodings::Boolean;
    /// Maximum descriptor entries the fixed census buffer can return.
    "4FCACCF9B6BBD63E8568B8B344DAAAE3" as pub census_capacity: inlineencodings::U256BE;
    /// CLOCK_MONOTONIC nanoseconds on this host, not publication time.
    "EDC7C86D3952C39D3382E8FC209E9A85" as pub monotonic_ns: inlineencodings::U256BE;
    /// Complete, clock-valid samples within 60 monotonic seconds (at most 60).
    "30655A8671E265A39C533B83DF835211" as pub recent_sample_count: inlineencodings::U256BE;
    /// Cumulative rejected captures (full/contended queue), saturating at u64.
    "13ACFBE31E0992A15491D542A081C8AF" as pub captures_dropped: inlineencodings::U256BE;
    /// Maximum pending failure occurrences in the preallocated capture queue.
    "84B768191D9A2192DEA784A09E294871" as pub queue_capacity: inlineencodings::U256BE;
}

fn wall_time(unix_ns: Option<i128>) -> Option<Inline<inlineencodings::NsTAIInterval>> {
    let epoch = Epoch::from_unix_duration(hifitime::Duration::from_total_nanoseconds(unix_ns?));
    (epoch, epoch).try_to_inline().ok()
}

impl NodeStore {
    /// Only the controller calls this, outside capture. Commit/flush failure
    /// leaves the caller's fixed batch queued, and never triggers FD capture.
    pub(crate) fn append_fd_events(&mut self, pending: &Batch) -> Result<()> {
        let mut batch = Fragment::empty();
        let collector = entity! { health_record::attrs::endpoint: self.signer.verifying_key() };
        let collector_id = collector.root().expect("collector root");
        let subject = entity! {
            health_record::attrs::endpoint: self.signer.verifying_key(),
            health_record::attrs::node: collector_id,
            telemetry::attrs::worker: "native",
            telemetry::attrs::role: "fd-diagnostics",
            telemetry::attrs::stage: "maintenance-failure-handler",
        };
        let subject_id = subject.root().expect("FD subject root");
        batch += collector;
        batch += subject;
        for event in pending.events.iter().flatten() {
            // Identity is the occurrence, not its measurements or wording.
            let occurrence = entity! {
                metadata::tag: KIND_FD_EXHAUSTION,
                health_record::attrs::session: event.session,
                sequence: event.sequence as u128,
            };
            let occurrence_id = occurrence.root().expect("FD occurrence root");
            batch += occurrence;
            batch += entity! { ExclusiveId::force_ref(&occurrence_id) @
                telemetry::attrs::subject: subject_id,
                metadata::created_at?: wall_time(event.handler.wall_unix_ns),
                monotonic_ns?: event.handler.monotonic_ns,
                metadata::name: event.code.name(),
                error_site: event.error_site.name(),
                recent_sample_count: event.samples as u128,
                captures_dropped: event.dropped as u128,
            };
            for (label, snapshot) in [
                ("handler", Some(event.handler)),
                ("sampled-high-water", event.sampled_high_water),
            ] {
                let Some(snapshot) = snapshot else { continue };
                let observation = entity! {
                    telemetry::attrs::subject: occurrence_id,
                    metadata::name: label,
                };
                let observation_id = observation.root().expect("FD snapshot root");
                batch += observation;
                batch += entity! { ExclusiveId::force_ref(&observation_id) @
                    metadata::created_at?: wall_time(snapshot.wall_unix_ns),
                    monotonic_ns?: snapshot.monotonic_ns,
                    soft_limit?: snapshot.limits.map(|(soft, _)| soft as u128),
                    hard_limit?: snapshot.limits.map(|(_, hard)| hard as u128),
                    fd_count?: snapshot.census.map(|census| census.count as u128),
                    census_complete?: snapshot.census.map(|census| census.complete),
                    census_capacity: FD_CAPACITY as u128,
                    mcp_http_accepted_streams?: snapshot.mcp_http_accepted_streams.map(|count| count as u128),
                };
                if let Some(census) = snapshot.census {
                    for (label, count) in TYPE_NAMES.iter().zip(census.types) {
                        let aggregate = entity! {
                            metadata::name: *label,
                            type_count: count as u128,
                        };
                        let aggregate_id = aggregate.root().expect("FD type root");
                        batch += aggregate;
                        batch += entity! { ExclusiveId::force_ref(&observation_id) @
                            fd_type: aggregate_id,
                        };
                    }
                }
                batch += if label == "handler" {
                    entity! { ExclusiveId::force_ref(&occurrence_id) @ handler_snapshot: observation_id }
                } else {
                    entity! { ExclusiveId::force_ref(&occurrence_id) @ sampled_high_water: observation_id }
                };
            }
        }
        if pending.dropped > 0 {
            // A cumulative observation is also emitted when no later failure
            // fits the queue, so full-queue losses do not require another event.
            batch += entity! {
                metadata::tag: KIND_FD_CAPTURE_LOSS,
                health_record::attrs::session: pending.session,
                telemetry::attrs::subject: subject_id,
                captures_dropped: pending.dropped as u128,
                queue_capacity: CAPACITY as u128,
            };
        }
        self.pile
            .commit(self.metrics, &self.signer, batch)
            .context("append bounded FD diagnostics to node metrics")?;
        self.pile
            .flush()
            .context("flush bounded FD diagnostics to node metrics")?;
        Ok(())
    }
}
