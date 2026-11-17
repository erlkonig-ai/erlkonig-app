//! Explicit, resident-only projection of previously published FD evidence.
//! No capture, owner, key loading, hydration, collection registration or writes.

use super::*;
use serde::Serialize;
use triblespace::core::collection::{Collection, CollectionHandle, records};
use triblespace::core::repo::pile::PileSnapshot;

const LIMIT: usize = 64;

/// Bounds every repeated output independently. Order is query order, never a
/// chronological ranking. Empty means no understood values, not numeric zero.
#[derive(Debug, Serialize)]
pub(crate) struct Values<T> {
    values: Vec<T>,
    truncated: bool,
}

fn bounded<T>(items: impl IntoIterator<Item = T>) -> Values<T> {
    let mut items = items.into_iter();
    let values = items.by_ref().take(LIMIT).collect();
    Values { values, truncated: items.next().is_some() }
}

#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum Report {
    Descriptors {
        collections: Values<String>,
        unavailable_descriptors_encountered: usize,
    },
    Events {
        collection: String,
        selection: &'static str,
        producer_contract: &'static str,
        events: Values<Event>,
        capture_losses: Values<Loss>,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct Event {
    event: String,
    session: String,
    sequence: u128,
    codes: Vec<&'static str>,
    error_site_subsystems: Vec<&'static str>,
    /// No descriptor provenance is inferred from a handler or separate gauge.
    pressure_owner: &'static str,
    // Exact decimal strings avoid JSON-client rounding of nanoseconds.
    wall_tai_ns_intervals: Values<(String, String)>,
    monotonic_ns: Values<u128>,
    recent_sample_count: Values<u128>,
    captures_dropped: Values<u128>,
    handler_snapshots: Values<Census>,
    sampled_high_water: Values<Census>,
}

#[derive(Debug, Serialize)]
struct Census {
    observation: String,
    wall_tai_ns_intervals: Values<(String, String)>,
    monotonic_ns: Values<u128>,
    soft_limit: Values<u128>,
    hard_limit: Values<u128>,
    fd_count: Values<u128>,
    census_complete: Values<bool>,
    census_capacity: Values<u128>,
    types: Vec<TypeCount>,
    mcp_http_accepted_streams: Values<u128>,
}

#[derive(Debug, Serialize)]
struct TypeCount {
    kind: &'static str,
    counts: Values<u128>,
}

#[derive(Debug, Serialize)]
pub(crate) struct Loss {
    observation: String,
    session: String,
    captures_dropped: Values<u128>,
    queue_capacity: Values<u128>,
}

/// Descriptor names are a type/selection hint, not identity or an exclusivity
/// assertion. Additional names/facts do not invalidate an understood match.
fn metrics_descriptor(snapshot: &PileSnapshot, handle: CollectionHandle) -> Result<bool> {
    let facts: TribleSet = snapshot.get(handle)?;
    Ok(exists!(pattern!(&facts, [{
        records::collection_name: "erlkonig-metrics",
    }])))
}

pub(crate) fn inspect(
    snapshot: &PileSnapshot,
    signer: VerifyingKey,
    selected: Option<CollectionHandle>,
) -> Result<Report> {
    let Some(handle) = selected else {
        let mut unavailable_descriptor_count = 0;
        let candidates = snapshot.collections()
            .map_err(|_| anyhow::anyhow!("collection metadata unavailable"))?;
        let collections = bounded(candidates.into_iter().filter_map(|handle| {
            let candidate = (|| -> Result<bool> {
                if !metrics_descriptor(snapshot, handle)? { return Ok(false); }
                Collection::<blobencodings::SimpleArchive>::open(snapshot, handle)?
                    .reader_is_admitted(snapshot, signer).map_err(Into::into)
            })();
            match candidate {
                Ok(true) => Some(hex::encode(handle.raw)),
                Ok(false) => None,
                Err(_) => { unavailable_descriptor_count += 1; None }
            }
        }));
        return Ok(Report::Descriptors { collections, unavailable_descriptors_encountered: unavailable_descriptor_count });
    };
    if !metrics_descriptor(snapshot, handle).map_err(|_| anyhow::anyhow!("metrics descriptor unavailable"))? {
        bail!("selected descriptor has no understood node metrics name");
    }
    let collection = Collection::<blobencodings::SimpleArchive>::open(snapshot, handle)
        .map_err(|_| anyhow::anyhow!("metrics descriptor unavailable or unsupported"))?;
    if !collection.reader_is_admitted(snapshot, signer)
        .map_err(|_| anyhow::anyhow!("metrics READ evidence unavailable"))? {
        bail!("supplied node identity is not admitted to metrics READ");
    }
    // PileSnapshot has resident local bytes only. Acquiring here requires all
    // known foundations; an unavailable member is an error, not empty evidence.
    let observed = snapshot.collection_acquiring(collection)
        .map_err(|_| anyhow::anyhow!("metrics observation unavailable"))?;
    let facts: TribleSet = observed.view()
        .map_err(|_| anyhow::anyhow!("metrics facts unavailable"))?;
    Ok(Report::Events {
        collection: hex::encode(handle.raw),
        selection: "at most 64 rows per list, query order; not latest or chronological; empty lists mean no understood facts",
        producer_contract: "maintenance handler after upstream unwinding; error site is not pressure owner; OS census has no subsystem ownership; MCP HTTP gauge counts only this endpoint's accepted stream wrappers, not listener/backlog/outbound sockets or requests; gauge and census are non-atomic, never subtract them; absent gauge is uninstrumented, not zero; best-effort 1s samples over 60 monotonic seconds; sampled high-water is not a peak; deferred publication; absent wall clock stays absent; incomplete census is a lower bound; capture losses are cumulative, not additive",
        events: bounded(find!((event: Id, session: Id, ordinal: u128), pattern!(&facts, [
            { ?event @ metadata::tag: KIND_FD_EXHAUSTION,
                health_record::attrs::session: ?session, sequence: ?ordinal,
                telemetry::attrs::subject: _?subject },
            { _?subject @ health_record::attrs::endpoint: signer,
                health_record::attrs::node: _?collector,
                telemetry::attrs::role: "fd-diagnostics" },
            { _?collector @ health_record::attrs::endpoint: signer },
        ])).map(|(event, session, ordinal)| Event {
            event: format!("{event:X}"), session: format!("{session:X}"), sequence: ordinal,
            codes: ["fd-limit-process", "fd-limit-system"].into_iter()
                .filter(|code| exists!(pattern!(&facts, [{ event @ metadata::name: *code }])))
                .collect(),
            error_site_subsystems: {
                let sites: Vec<_> = crate::fd_diagnostics::ErrorSite::ALL.into_iter()
                    .map(|site| site.name())
                    .filter(|site| exists!(pattern!(&facts, [{ event @ error_site: *site }])))
                    .collect();
                if sites.is_empty() { vec!["unknown"] } else { sites }
            },
            pressure_owner: "unknown",
            wall_tai_ns_intervals: bounded(find!(time: (i128, i128),
                pattern!(&facts, [{ event @ metadata::created_at: ?time }]))
                .map(|(start, end)| (start.to_string(), end.to_string()))),
            monotonic_ns: bounded(find!(value: u128, pattern!(&facts, [{ event @ monotonic_ns: ?value }]))),
            recent_sample_count: bounded(find!(value: u128, pattern!(&facts, [{ event @ recent_sample_count: ?value }]))),
            captures_dropped: bounded(find!(value: u128, pattern!(&facts, [{ event @ captures_dropped: ?value }]))),
            handler_snapshots: bounded(find!(observation: Id, pattern!(&facts, [
                { event @ handler_snapshot: ?observation },
                { ?observation @ telemetry::attrs::subject: event },
            ])).map(|observation| census(&facts, observation))),
            sampled_high_water: bounded(find!(observation: Id, pattern!(&facts, [
                { event @ sampled_high_water: ?observation },
                { ?observation @ telemetry::attrs::subject: event },
            ])).map(|observation| census(&facts, observation))),
        })),
        capture_losses: bounded(find!((loss: Id, session: Id), pattern!(&facts, [
            { ?loss @ metadata::tag: KIND_FD_CAPTURE_LOSS,
                health_record::attrs::session: ?session,
                telemetry::attrs::subject: _?subject },
            { _?subject @ health_record::attrs::endpoint: signer,
                health_record::attrs::node: _?collector,
                telemetry::attrs::role: "fd-diagnostics" },
            { _?collector @ health_record::attrs::endpoint: signer },
        ])).map(|(loss, session)| Loss {
            observation: format!("{loss:X}"), session: format!("{session:X}"),
            captures_dropped: bounded(find!(value: u128, pattern!(&facts, [{ loss @ captures_dropped: ?value }]))),
            queue_capacity: bounded(find!(value: u128, pattern!(&facts, [{ loss @ queue_capacity: ?value }]))),
        })),
    })
}

fn census(facts: &TribleSet, observation: Id) -> Census {
    Census {
        observation: format!("{observation:X}"),
        wall_tai_ns_intervals: bounded(find!(time: (i128, i128),
            pattern!(facts, [{ observation @ metadata::created_at: ?time }]))
            .map(|(start, end)| (start.to_string(), end.to_string()))),
        monotonic_ns: bounded(find!(value: u128, pattern!(facts, [{ observation @ monotonic_ns: ?value }]))),
        soft_limit: bounded(find!(value: u128, pattern!(facts, [{ observation @ soft_limit: ?value }]))),
        hard_limit: bounded(find!(value: u128, pattern!(facts, [{ observation @ hard_limit: ?value }]))),
        fd_count: bounded(find!(value: u128, pattern!(facts, [{ observation @ fd_count: ?value }]))),
        census_complete: bounded(find!(value: bool, pattern!(facts, [{ observation @ census_complete: ?value }]))),
        census_capacity: bounded(find!(value: u128, pattern!(facts, [{ observation @ census_capacity: ?value }]))),
        mcp_http_accepted_streams: bounded(find!(value: u128,
            pattern!(facts, [{ observation @ mcp_http_accepted_streams: ?value }]))),
        types: TYPE_NAMES.into_iter().map(|kind| TypeCount {
            kind,
            counts: bounded(find!(count: u128, pattern!(facts, [
                { observation @ fd_type: _?bucket },
                { _?bucket @ metadata::name: kind, type_count: ?count },
            ]))),
        }).collect(),
    }
}

#[cfg(test)]
mod tests;
