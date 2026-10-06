use super::*;
use crate::node_metrics::{Sample, now_tai_ns};
use std::time::Duration;
use triblespace::core::collection::{CollectionRecord, CollectionStore};
use triblespace::core::repo::BlobStorePut;
use triblespace_net::{dashboard::CountMetric, telemetry::Metric};

fn scratch() -> (tempfile::TempDir, PathBuf, SigningKey, NodeStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.pile");
    std::fs::File::create(&path).unwrap();
    let signer = SigningKey::from_bytes(&[39; 32]);
    let node = NodeStore::open(&path, signer.clone()).unwrap();
    (dir, path, signer, node)
}

fn entry(label: &str) -> ManagedPile {
    ManagedPile::new(
        label,
        format!("/explicit/{label}.pile").into(),
        format!("/explicit/{label}.key").into(),
    )
}

#[test]
fn empty_configuration_is_known_and_off_and_store_is_send() {
    fn send<T: Send>() {}
    send::<NodeStore>();
    let (_dir, _path, _signer, mut node) = scratch();
    let view = node.poll_config().unwrap().unwrap();
    assert!(view.managed_ids().unwrap().is_empty());
    assert_eq!(view.settings().unwrap(), NodeSettings::default());
    assert!(view.managed(genid().id).unwrap().is_none());
    assert!(node.poll_config().unwrap().is_none());
    node.close().unwrap();
}

#[test]
fn persisted_reopen_explicit_disable_and_isolated_entries() {
    let (_dir, path, signer, mut node) = scratch();
    let mut first = entry("first");
    first.sync_enabled = true;
    first.peers = vec![SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes()];
    first.sync_collections = vec![CollectionHandle::new([8; 32])];
    let mut second = entry("second");
    second.maintenance_enabled = true;
    second.maintain_targets = vec![CollectionHandle::new([9; 32])];
    let view = node.config().unwrap();
    node.put_managed(&view, &first).unwrap();
    let view = node.config().unwrap();
    node.put_managed(&view, &second).unwrap();
    let view = node.config().unwrap();
    first.sync_enabled = false;
    node.put_managed(&view, &first).unwrap();
    node.close().unwrap();

    let mut reopened = NodeStore::open(&path, signer).unwrap();
    let view = reopened.config().unwrap();
    assert_eq!(view.managed_ids().unwrap().len(), 2);
    assert_eq!(view.managed(first.id).unwrap(), Some(first));
    assert_eq!(view.managed(second.id).unwrap(), Some(second));
    reopened.close().unwrap();
    // Merely configuring another pile/key never accesses either path.
    assert!(!Path::new("/explicit/first.pile").exists());
}

#[test]
fn stale_edit_is_refused_and_external_append_invalidates_only_the_observation() {
    let (_dir, path, signer, mut node) = scratch();
    let original = node.config().unwrap();
    let mut other = NodeStore::open(&path, signer).unwrap();
    let there = other.config().unwrap();
    other.put_managed(&there, &entry("other")).unwrap();
    other.close().unwrap();
    assert!(node.config_changed(&original).unwrap());
    assert!(node.put_managed(&original, &entry("stale")).is_err());
    assert!(node.put_mcp_port(&original, 0).is_err());
    let view = node.config().unwrap();
    node.put_mcp_port(&view, 0).unwrap();
    assert_eq!(node.config().unwrap().settings().unwrap().mcp_port, 0);
    node.close().unwrap();
}

#[test]
fn opaque_ids_extra_annotations_and_equal_concurrent_values_work() {
    let (_dir, _path, _signer, mut node) = scratch();
    let item = entry("opaque");
    for _ in 0..2 {
        let opaque = genid();
        let fragment = entity! { &opaque @
            metadata::tag: attrs::MANAGED_PILE,
            config::anchor: item.id,
            attrs::enrolled: true,
            metadata::name: "opaque",
            attrs::pile_path: "/explicit/opaque.pile",
            attrs::key_path: "/explicit/opaque.key",
            attrs::sync_enabled: false,
            attrs::maintenance_enabled: false,
            metadata::description: "An unmodeled annotation does not close the world.",
        };
        node.pile
            .commit(node.config, &node.signer, fragment)
            .unwrap();
    }
    assert_eq!(node.config().unwrap().managed(item.id).unwrap(), Some(item));
    node.close().unwrap();
}

#[test]
fn concurrent_different_selections_are_not_silently_unioned() {
    let (_dir, _path, _signer, mut node) = scratch();
    let item = entry("fork");
    for selected in [
        CollectionHandle::new([17; 32]),
        CollectionHandle::new([18; 32]),
    ] {
        let fragment = entity! {
            metadata::tag: attrs::MANAGED_PILE,
            config::anchor: item.id,
            attrs::enrolled: true,
            metadata::name: "fork",
            attrs::pile_path: "/explicit/fork.pile",
            attrs::key_path: "/explicit/fork.key",
            attrs::sync_enabled: true,
            attrs::maintenance_enabled: false,
            attrs::sync_collection: selected,
        };
        node.pile
            .commit(node.config, &node.signer, fragment)
            .unwrap();
    }
    let view = node.config().unwrap();
    let error = view.managed(item.id).unwrap_err().to_string();
    assert!(error.contains("conflicting"), "{error}");
    // A deliberate edit based on this observation reconciles both heads.
    node.put_managed(&view, &item).unwrap();
    assert_eq!(node.config().unwrap().managed(item.id).unwrap(), Some(item));
    node.close().unwrap();
}

#[test]
fn missing_flags_or_multivalued_scalar_do_not_implicitly_enable() {
    let (_dir, _path, _signer, mut node) = scratch();
    let good = entry("unaffected");
    let view = node.config().unwrap();
    node.put_managed(&view, &good).unwrap();
    let incomplete = genid().id;
    let ambiguous = genid().id;
    let missing = entity! {
        metadata::tag: attrs::MANAGED_PILE, config::anchor: incomplete,
        attrs::enrolled: true,
        metadata::name: "incomplete", attrs::pile_path: "/p", attrs::key_path: "/k",
        attrs::maintenance_enabled: false,
    };
    let repeated = entity! {
        metadata::tag: attrs::MANAGED_PILE, config::anchor: ambiguous,
        attrs::enrolled: true,
        metadata::name: "ambiguous", attrs::pile_path: "/p", attrs::key_path: "/k",
        attrs::sync_enabled*: [true, false], attrs::maintenance_enabled: false,
    };
    node.pile
        .commit(node.config, &node.signer, missing + repeated)
        .unwrap();
    let view = node.config().unwrap();
    assert_eq!(view.managed_ids().unwrap().len(), 3);
    assert!(
        view.managed(incomplete)
            .unwrap_err()
            .to_string()
            .contains("incomplete")
    );
    assert!(
        view.managed(ambiguous)
            .unwrap_err()
            .to_string()
            .contains("conflicting")
    );
    assert_eq!(view.managed(good.id).unwrap(), Some(good));
    node.close().unwrap();
}

#[test]
fn unknown_known_commit_bytes_are_not_a_known_empty_configuration() {
    let (_dir, _path, _signer, mut node) = scratch();
    assert!(node.poll_config().unwrap().is_some());
    let mut source = MemoryRepo::default();
    let remote = source
        .commit(
            node.config,
            &node.signer,
            entity! {
                config::anchor: attrs::NODE_SETTINGS, attrs::mcp_port: 0_u16,
            },
        )
        .unwrap();
    // The admitted record arrived, its payload did not. A resident-only
    // collection read can be partial; configuration intentionally cannot.
    node.pile.insert(CollectionRecord::Commit(remote)).unwrap();
    assert!(node.config().is_err());
    assert!(node.poll_config().is_err());
    assert!(
        node.last_config.is_none(),
        "a failed poll must re-emit configuration after recovery"
    );
    let payload: Blob<blobencodings::SimpleArchive> = source
        .snapshot()
        .unwrap()
        .get(Inline::new(remote.data().raw))
        .unwrap();
    node.pile
        .put::<blobencodings::SimpleArchive, _>(payload)
        .unwrap();
    assert_eq!(
        node.poll_config()
            .unwrap()
            .unwrap()
            .settings()
            .unwrap()
            .mcp_port,
        0
    );
    node.close().unwrap();
}

#[test]
fn missing_text_stays_unknown_and_arrival_invalidates_its_exact_read_interest() {
    let (_dir, _path, _signer, mut node) = scratch();
    let item = genid().id;
    let mut source = MemoryRepo::default();
    let name = source
        .put::<blobencodings::UTF8String, _>("arrived later")
        .unwrap();
    let fragment = entity! {
        metadata::tag: attrs::MANAGED_PILE, config::anchor: item,
        attrs::enrolled: true,
        metadata::name: name, attrs::pile_path: "/p", attrs::key_path: "/k",
        attrs::sync_enabled: false, attrs::maintenance_enabled: false,
    };
    node.pile
        .commit(node.config, &node.signer, fragment)
        .unwrap();
    let before = node.poll_config().unwrap().unwrap();
    assert!(before.managed(item).is_err());
    assert!(node.poll_config().unwrap().is_none());
    node.pile
        .put::<blobencodings::UTF8String, _>("arrived later")
        .unwrap();
    assert!(node.config_changed(&before).unwrap());
    let after = node.poll_config().unwrap().unwrap();
    assert_eq!(after.managed(item).unwrap().unwrap().label, "arrived later");
    node.close().unwrap();
}

#[test]
fn native_metrics_persist_without_configuration_invalidation_or_invented_zeros() {
    let (_dir, path, signer, mut node) = scratch();
    let item = entry("metrics");
    let view = node.config().unwrap();
    node.put_managed(&view, &item).unwrap();
    let before = node.poll_config().unwrap().unwrap();
    before.managed(item.id).unwrap();
    let mut sample = Sample::now(
        Some(item.id),
        "native",
        "sync",
        genid().id,
        Duration::from_secs(10),
    )
    .unwrap();
    let managed_key = SigningKey::from_bytes(&[71; 32]).verifying_key();
    sample.endpoint = Some(managed_key);
    sample.received_bytes = Some(123);
    sample.error = Some("observed test failure".into());
    node.append_metrics(&[sample]).unwrap();
    assert!(!node.config_changed(&before).unwrap());
    assert!(node.poll_config().unwrap().is_none());
    node.close().unwrap();

    let mut reopened = NodeStore::open(&path, signer).unwrap();
    let reports = reopened
        .metrics(now_tai_ns().unwrap(), Duration::from_secs(60))
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].managed, [item.id]);
    assert_eq!(reports[0].report.node, managed_key.to_bytes());
    assert_eq!(
        reports[0].collectors,
        [SigningKey::from_bytes(&[39; 32]).verifying_key().to_bytes()]
    );
    assert_eq!(reports[0].errors, ["observed test failure"]);
    let metric = |kind| {
        &reports[0]
            .report
            .metrics
            .iter()
            .find(|metric| metric.metric == kind)
            .unwrap()
            .value
    };
    assert_eq!(metric(Metric::ReceivedBytes), &CountMetric::Value(123));
    assert_eq!(metric(Metric::SentBytes), &CountMetric::Absent);
    assert!(
        reports[0]
            .report
            .metrics
            .iter()
            .all(|metric| metric.per_second.is_none())
    );
    reopened.close().unwrap();
}

#[test]
fn explicit_withdrawal_persists_and_concurrent_enrollment_is_not_hidden() {
    let (_dir, path, signer, mut node) = scratch();
    let item = entry("withdrawn");
    let initial = node.config().unwrap();
    node.put_managed(&initial, &item).unwrap();
    let before = node.config().unwrap();
    node.remove_managed(&before, item.id).unwrap();
    node.close().unwrap();
    let mut node = NodeStore::open(&path, signer).unwrap();
    assert!(node.config().unwrap().managed(item.id).unwrap().is_none());
    // An independent concurrent enrollment did not observe the withdrawal.
    let independent = genid();
    let revision = entity! { &independent @
        metadata::tag: attrs::MANAGED_PILE, config::anchor: item.id,
        attrs::enrolled: true, metadata::name: "withdrawn",
        attrs::pile_path: "/explicit/withdrawn.pile", attrs::key_path: "/explicit/withdrawn.key",
        attrs::sync_enabled: false, attrs::maintenance_enabled: false,
    };
    node.pile
        .commit(node.config, &node.signer, revision)
        .unwrap();
    assert!(node.config().unwrap().managed(item.id).is_err());
    node.close().unwrap();
}

#[test]
fn missing_or_concurrent_settings_are_not_arbitrary_port_choices() {
    let (_dir, _path, _signer, mut node) = scratch();
    for port in [41773_u16, 41774_u16] {
        let revision = entity! { config::anchor: attrs::NODE_SETTINGS, attrs::mcp_port: port };
        node.pile
            .commit(node.config, &node.signer, revision)
            .unwrap();
    }
    let view = node.config().unwrap();
    assert!(view.settings().is_err());
    node.put_mcp_port(&view, 0).unwrap();
    assert_eq!(node.config().unwrap().settings().unwrap().mcp_port, 0);
    node.close().unwrap();
}

#[test]
fn relative_paths_never_depend_on_process_working_directory() {
    let (_dir, _path, _signer, mut node) = scratch();
    let mut item = entry("relative");
    item.pile = "relative.pile".into();
    let view = node.config().unwrap();
    assert!(
        node.put_managed(&view, &item)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    let unknown = entity! {
        metadata::tag: attrs::MANAGED_PILE, config::anchor: item.id,
        attrs::enrolled: true, metadata::name: "relative",
        attrs::pile_path: "relative.pile", attrs::key_path: "/absolute.key",
        attrs::sync_enabled: true, attrs::maintenance_enabled: false,
    };
    node.pile
        .commit(node.config, &node.signer, unknown)
        .unwrap();
    assert!(
        node.config()
            .unwrap()
            .managed(item.id)
            .unwrap_err()
            .to_string()
            .contains("absolute")
    );
    node.close().unwrap();
}

#[test]
fn managed_measurements_cannot_impersonate_the_collector_identity() {
    let (_dir, _path, _signer, mut node) = scratch();
    let sample = Sample::now(
        Some(genid().id),
        "native",
        "sync",
        genid().id,
        Duration::ZERO,
    )
    .unwrap();
    assert!(
        node.append_metrics(&[sample])
            .unwrap_err()
            .to_string()
            .contains("worker signing identity")
    );
    assert!(
        node.metrics(now_tai_ns().unwrap(), Duration::from_secs(60))
            .unwrap()
            .is_empty()
    );
    node.close().unwrap();
}
