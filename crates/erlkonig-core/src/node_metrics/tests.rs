use super::*;
use ed25519_dalek::SigningKey;
use std::path::PathBuf;
use triblespace_net::{dashboard::CountMetric, telemetry::Metric};

fn scratch() -> (tempfile::TempDir, PathBuf, SigningKey, NodeStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.pile");
    std::fs::File::create(&path).unwrap();
    let signer = SigningKey::from_bytes(&[73; 32]);
    let node = NodeStore::open(&path, signer.clone()).unwrap();
    (dir, path, signer, node)
}

#[test]
fn fd_diagnostics_retry_preserves_identity_and_typed_sparse_evidence() {
    use crate::fd_diagnostics::{Batch, CAPACITY, Census, Exhaustion, Occurrence, Snapshot};
    let (_dir, path, signer, mut node) = scratch();
    let before_config = node.poll_config().unwrap().unwrap();
    let mut types = [0; 12];
    types[1] = 7;
    types[11] = 1;
    let event = Occurrence {
        session: genid().id, sequence: 7, code: Exhaustion::Process,
        error_site: crate::fd_diagnostics::ErrorSite::Maintenance,
        handler: Snapshot { wall_unix_ns: Some(1_790_000_000_123_456_789),
            monotonic_ns: Some(500), limits: Some((256, u64::MAX)),
            census: Some(Census { count: 8, types, complete: false }),
            mcp_http_accepted_streams: Some(12) },
        sampled_high_water: None, samples: 0, dropped: 3,
    };
    let mut pending = Batch { session: genid().id, events: [None; CAPACITY], dropped: 3 };
    pending.events[0] = Some(event);
    // Emulate a commit reaching the pile before a caller-visible flush error:
    // retrying exactly this batch must only reassert its occurrence identity.
    node.append_fd_events(&pending).unwrap();
    node.append_fd_events(&pending).unwrap();
    assert!(!node.config_changed(&before_config).unwrap());
    assert!(node.metrics(now_tai_ns().unwrap(), Duration::from_secs(60)).unwrap().is_empty());
    node.close().unwrap();
    let mut node = NodeStore::open(&path, signer).unwrap();
    let snapshot = node.pile.snapshot().unwrap();
    let observed = snapshot.collection_acquiring(node.metrics).unwrap();
    let facts: TribleSet = observed.view().unwrap();
    let rows: Vec<_> = find!((event_id: Id, observation: Id, count: u128, complete: bool,
        soft: u128, hard: u128, unknown: u128, monotonic: u128), pattern!(&facts, [
        { ?event_id @ metadata::tag: fd::KIND_FD_EXHAUSTION,
            health_record::attrs::session: event.session,
            fd::sequence: 7u128, fd::handler_snapshot: ?observation,
            metadata::name: "fd-limit-process", fd::monotonic_ns: ?monotonic },
        { ?observation @ fd::fd_count: ?count, fd::census_complete: ?complete,
            fd::soft_limit: ?soft, fd::hard_limit: ?hard, fd::fd_type: _?bucket },
        { _?bucket @ metadata::name: "unknown", fd::type_count: ?unknown },
    ])).collect();
    assert_eq!(rows.len(), 1);
    let (event_id, observation, count, complete, soft, hard, unknown, monotonic) = rows[0];
    assert_eq!((count, complete, soft, hard, unknown, monotonic), (8, false, 256, u64::MAX as u128, 1, 500));
    assert!(!exists!(pattern!(&facts, [{ event_id @ fd::sampled_high_water: _?high_water }])));
    assert!(!exists!(pattern!(&facts, [{ event_id @ metadata::description: _?text }])));
    assert!(exists!(pattern!(&facts, [{ event_id @ fd::error_site: "maintenance" }])));
    assert!(exists!(pattern!(&facts, [{ observation @ fd::mcp_http_accepted_streams: 12u128 }])));
    assert!(exists!(pattern!(&facts, [{ metadata::tag: fd::KIND_FD_CAPTURE_LOSS,
        fd::captures_dropped: 3u128, fd::queue_capacity: CAPACITY as u128 }])));
    let types: Vec<_> = find!(count: u128, pattern!(&facts, [
        { observation @ fd::fd_type: _?bucket }, { _?bucket @ fd::type_count: ?count },
    ])).collect();
    assert_eq!(types.len(), 12);
    assert_eq!(types.into_iter().sum::<u128>(), 8);
    drop(observed);
    drop(snapshot);
    node.close().unwrap();
}

#[test]
fn fd_unknown_census_is_absent_not_zero_and_clocks_survive_deferred_publish() {
    use crate::fd_diagnostics::{Batch, CAPACITY, Exhaustion, Occurrence, Snapshot};
    let (_dir, _path, _signer, mut node) = scratch();
    let occurrence = Occurrence {
        session: genid().id, sequence: 1, code: Exhaustion::System,
        error_site: crate::fd_diagnostics::ErrorSite::Maintenance,
        handler: Snapshot::default(), sampled_high_water: None, samples: 0, dropped: 0,
    };
    let mut batch = Batch { session: genid().id, events: [None; CAPACITY], dropped: 0 };
    batch.events[0] = Some(occurrence);
    node.append_fd_events(&batch).unwrap();
    let snapshot = node.pile.snapshot().unwrap();
    let observed = snapshot.collection_acquiring(node.metrics).unwrap();
    let facts: TribleSet = observed.view().unwrap();
    let (event, handler) = find!((event: Id, handler: Id), pattern!(&facts, [
        { ?event @ metadata::tag: fd::KIND_FD_EXHAUSTION,
            metadata::name: "fd-limit-system", fd::handler_snapshot: ?handler },
    ])).next().unwrap();
    assert!(!exists!(pattern!(&facts, [{ handler @ fd::fd_count: _?count }])));
    assert!(!exists!(pattern!(&facts, [{ handler @ fd::soft_limit: _?limit }])));
    assert!(!exists!(pattern!(&facts, [{ handler @ fd::mcp_http_accepted_streams: _?count }])));
    assert!(!exists!(pattern!(&facts, [{ event @ metadata::created_at: _?time }])));
    assert!(!exists!(pattern!(&facts, [{ event @ fd::monotonic_ns: _?time }])));
    drop(observed);
    drop(snapshot);
    node.close().unwrap();
}

#[test]
fn fd_real_commit_then_simulated_failure_retries_one_occurrence_after_reopen() {
    let (_dir, path, signer, mut node) = scratch();
    let diagnostics = crate::fd_diagnostics::Diagnostics::new();
    let session = genid().id;
    diagnostics.maintenance_failure(session, "synthetic maintenance: Too many open files (os error 24)");
    let mut first = None;
    let result = diagnostics.publish_pending(|batch| -> Result<()> {
        first = batch.events[0];
        node.append_fd_events(batch)?;
        // Simulated unknown commit/flush outcome, not an actual disk fault.
        bail!("synthetic failure after successful actual append");
    });
    assert!(result.is_err());
    diagnostics.publish_pending(|batch| {
        assert_eq!(batch.events[0], first);
        node.append_fd_events(batch)
    }).unwrap();
    node.close().unwrap();
    let mut node = NodeStore::open(&path, signer).unwrap();
    let snapshot = node.pile.snapshot().unwrap();
    let observed = snapshot.collection_acquiring(node.metrics).unwrap();
    let facts: TribleSet = observed.view().unwrap();
    let events: Vec<_> = find!((event: Id, monotonic: u128), pattern!(&facts, [{
        ?event @ metadata::tag: fd::KIND_FD_EXHAUSTION,
        health_record::attrs::session: session, fd::sequence: 1u128,
        fd::monotonic_ns: ?monotonic,
    }])).collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1, first.unwrap().handler.monotonic_ns.unwrap());
    drop(observed);
    drop(snapshot);
    node.close().unwrap();
}

#[test]
fn service_causes_and_cleanup_events_survive_reopen_independently() {
    let (_dir, path, signer, mut node) = scratch();
    let attempt = genid().id;
    let cause = ServiceEvent::now(
        signer.verifying_key(),
        attempt,
        Duration::from_secs(8),
        ServicePhase::Stop,
        ServiceCode::ResourceHeadroomLimit,
    )
    .unwrap();
    let cleanup = ServiceEvent::now(
        signer.verifying_key(),
        attempt,
        Duration::from_secs(13),
        ServicePhase::Cleanup,
        ServiceCode::EndpointShutdownTimeout,
    )
    .unwrap();
    node.append_service_events(&[cause.clone()]).unwrap();
    node.append_service_events(&[cleanup]).unwrap();
    // Re-submitting one immutable occurrence must not manufacture another one.
    node.append_service_events(&[cause.clone()]).unwrap();
    node.close().unwrap();

    let mut node = NodeStore::open(&path, signer.clone()).unwrap();
    let snapshot = node.pile.snapshot().unwrap();
    let observed = snapshot.collection_acquiring(node.metrics).unwrap();
    let facts: TribleSet = observed.view().unwrap();
    let mut events = Vec::new();
    for (event, stage, code, description, elapsed, endpoint, collector) in find!(
        (event: Id, stage: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
            code: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
            description: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
            elapsed: u128, endpoint: VerifyingKey, collector: VerifyingKey),
        pattern!(&facts, [
            { ?event @ metadata::tag: KIND_SERVICE_EVENT,
                health_record::attrs::session: attempt,
                telemetry::attrs::subject: _?subject,
                telemetry::attrs::elapsed_ns: ?elapsed,
                metadata::name: ?code, metadata::description: ?description },
            { _?subject @ telemetry::attrs::stage: ?stage,
                health_record::attrs::endpoint: ?endpoint,
                health_record::attrs::node: _?collector },
            { _?collector @ health_record::attrs::endpoint: ?collector },
        ])
    ) {
        let stage: View<str> = snapshot.get(stage).unwrap();
        let code: View<str> = snapshot.get(code).unwrap();
        let description: View<str> = snapshot.get(description).unwrap();
        assert_eq!(endpoint, signer.verifying_key());
        assert_eq!(collector, signer.verifying_key());
        events.push((
            stage.to_string(),
            code.to_string(),
            description.to_string(),
            elapsed,
            event,
        ));
    }
    events.sort_unstable();
    assert_eq!(events.len(), 2);
    assert_eq!(
        (&*events[0].0, &*events[0].1, &*events[0].2, events[0].3),
        (
            "cleanup",
            ServiceCode::EndpointShutdownTimeout.as_str(),
            ServiceCode::EndpointShutdownTimeout.description(),
            13_000_000_000,
        )
    );
    assert_eq!(
        (&*events[1].0, &*events[1].1, &*events[1].2, events[1].3),
        (
            "stop",
            ServiceCode::ResourceHeadroomLimit.as_str(),
            ServiceCode::ResourceHeadroomLimit.description(),
            8_000_000_000,
        )
    );
    assert_ne!(events[0].4, events[1].4);
    assert!(exists!(pattern!(&facts, [{ events[1].4 @
        metadata::created_at: cause.observed_at,
    }])));
    assert!(
        node.metrics(now_tai_ns().unwrap(), Duration::from_secs(60))
            .unwrap()
            .is_empty()
    );
    drop(observed);
    drop(snapshot);
    node.close().unwrap();
}

#[test]
fn telemetry_labels_are_utf8_blobs_not_bounded_inline_strings() {
    let (_dir, path, signer, mut node) = scratch();
    let worker = "native worker with a label longer than thirty-two bytes 🦋";
    let role = "custom\0role with an unbounded UTF-8 label";
    let mut sample =
        Sample::now(None, worker, role, genid().id, Duration::from_secs(1)).unwrap();
    sample.received_bytes = Some(7);
    node.append_metrics(&[sample]).unwrap();
    node.close().unwrap();

    let mut node = NodeStore::open(&path, signer).unwrap();
    let reports = node
        .metrics(now_tai_ns().unwrap(), Duration::from_secs(60))
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].report.worker, worker);
    assert_eq!(reports[0].report.role, role);
    node.close().unwrap();
}

#[test]
fn service_events_do_not_change_sample_rates_or_configuration_observations() {
    let (_dir, _path, signer, mut node) = scratch();
    let before_config = node.poll_config().unwrap().unwrap();
    let observation = before_config.observation().unwrap();
    let session = genid().id;
    for (seconds, received) in [(100_u64, 100_u128), (110, 300)] {
        let mut sample = Sample::now(
            None,
            "native",
            "sync",
            session,
            Duration::from_secs(seconds),
        )
        .unwrap();
        let epoch = Epoch::from_tai_duration(hifitime::Duration::from_total_nanoseconds(
            i128::from(seconds) * 1_000_000_000,
        ));
        sample.observed_at = (epoch, epoch).try_to_inline().unwrap();
        sample.received_bytes = Some(received);
        node.append_metrics(&[sample]).unwrap();
    }
    let before = node
        .metrics(111_000_000_000, Duration::from_secs(60))
        .unwrap();
    let received = before[0]
        .report
        .metrics
        .iter()
        .find(|value| value.metric == Metric::ReceivedBytes)
        .unwrap();
    assert_eq!(received.value, CountMetric::Value(300));
    assert_eq!(received.per_second, Some(20.0));

    node.append_service_events(&[ServiceEvent::now(
        signer.verifying_key(),
        session,
        Duration::from_secs(111),
        ServicePhase::Session,
        ServiceCode::SessionFailed,
    )
    .unwrap()])
        .unwrap();
    assert_eq!(
        node.metrics(111_000_000_000, Duration::from_secs(60))
            .unwrap(),
        before
    );
    assert!(!node.config_changed(&before_config).unwrap());
    assert!(node.poll_config().unwrap().is_none());
    assert_eq!(node.config().unwrap().observation().unwrap(), observation);
    node.close().unwrap();
}
