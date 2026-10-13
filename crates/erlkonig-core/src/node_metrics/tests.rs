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
