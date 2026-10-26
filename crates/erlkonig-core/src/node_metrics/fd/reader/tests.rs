use super::*;
use crate::fd_diagnostics::{Batch, CAPACITY, Exhaustion, Occurrence, Snapshot};
use ed25519_dalek::SigningKey;
use std::{ffi::OsString, path::PathBuf};
use triblespace::core::collection::{CollectionRecord, CollectionStore};
use triblespace::core::repo::pile::{Pile, PileFile};

fn fixture() -> (tempfile::TempDir, PathBuf, NodeStore) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.pile");
    std::fs::File::create(&path).unwrap();
    let node = NodeStore::open(&path, SigningKey::from_bytes(&[73; 32])).unwrap();
    (dir, path, node)
}

fn read(path: &std::path::Path, signer: VerifyingKey, selected: Option<CollectionHandle>) -> Result<Report> {
    let mut pile = Pile::with_host(PileFile::open_read_only(path)?, Some(signer));
    inspect(&pile.snapshot()?, signer, selected)
}

fn append(node: &mut NodeStore, count: usize) {
    let session = genid().id;
    for ordinal in 0..count {
        let handler = Snapshot {
            wall_unix_ns: Some(1_790_000_000_123_456_789), monotonic_ns: Some(500),
            limits: Some((256, u64::MAX)),
            census: Some(crate::fd_diagnostics::Census { count: 8,
                types: [0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], complete: false }),
            // Deliberately greater than the separately observed census.
            mcp_http_accepted_streams: Some(12),
        };
        let mut batch = Batch { session, events: [None; CAPACITY], dropped: 3 };
        batch.events[0] = Some(Occurrence { session, sequence: ordinal as u64,
            code: Exhaustion::Process, handler, sampled_high_water: Some(handler),
            error_site: crate::fd_diagnostics::ErrorSite::Maintenance,
            samples: 5, dropped: 3 });
        node.append_fd_events(&batch).unwrap();
    }
}

#[test]
fn fd_reader_existing_owner_read_only_discovery_and_sparse_numeric_evidence() {
    let (dir, path, mut node) = fixture();
    append(&mut node, 1);
    let signer = node.signer.verifying_key();
    let selected = node.metrics.handle();
    let before = std::fs::read(&path).unwrap();
    let files_before = std::fs::read_dir(dir.path()).unwrap().count();
    let Report::Descriptors { collections, unavailable_descriptors_encountered } = read(&path, signer, None).unwrap() else { panic!() };
    assert_eq!(collections.values, vec![hex::encode(selected.raw)]);
    assert!(!collections.truncated);
    assert_eq!(unavailable_descriptors_encountered, 0);
    let Report::Events { events, capture_losses, .. } = read(&path, signer, Some(selected)).unwrap() else { panic!() };
    assert_eq!(events.values.len(), 1);
    let event = &events.values[0];
    assert_eq!(event.codes, ["fd-limit-process"]);
    assert_eq!(event.error_site_subsystems, ["maintenance"]);
    assert_eq!(event.pressure_owner, "unknown");
    assert_eq!(event.monotonic_ns.values, [500]);
    assert_eq!(event.recent_sample_count.values, [5]);
    assert_eq!(event.wall_tai_ns_intervals.values.len(), 1);
    let census = &event.handler_snapshots.values[0];
    assert_eq!(census.soft_limit.values, [256]);
    assert_eq!(census.hard_limit.values, [u64::MAX as u128]);
    assert_eq!(census.fd_count.values, [8]);
    assert_eq!(census.census_complete.values, [false]);
    assert_eq!(census.census_capacity.values, [FD_CAPACITY as u128]);
    assert_eq!(census.types[1].counts.values, [7]);
    assert_eq!(census.mcp_http_accepted_streams.values, [12]);
    assert_eq!(event.sampled_high_water.values.len(), 1);
    assert_eq!(capture_losses.values[0].captures_dropped.values, [3]);
    assert_eq!(capture_losses.values[0].queue_capacity.values, [CAPACITY as u128]);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), files_before);
    node.close().unwrap();
}

#[test]
fn fd_reader_open_world_random_ids_plural_values_and_allowlist_only() {
    let (_dir, path, mut node) = fixture();
    let signer = node.signer.verifying_key();
    let collector = genid();
    let subject = genid();
    let event = genid();
    let observation = genid();
    let unknown_type = genid();
    let session = genid().id;
    let mut fragment = entity! { &collector @ health_record::attrs::endpoint: signer };
    fragment += entity! { &subject @ health_record::attrs::endpoint: signer,
        health_record::attrs::node: &collector, telemetry::attrs::role: "fd-diagnostics" };
    fragment += entity! { &event @ metadata::tag: KIND_FD_EXHAUSTION,
        health_record::attrs::session: session, sequence: 42u128,
        telemetry::attrs::subject: &subject, handler_snapshot: &observation,
        metadata::name*: ["fd-limit-system", "/private/credential-canary"],
        error_site: "/private/credential-canary",
        metadata::description: "credential-canary" };
    fragment += entity! { &observation @ telemetry::attrs::subject: &event,
        fd_count*: [8u128, 9], soft_limit: 256u128, fd_type: &unknown_type };
    fragment += entity! { &observation @ fd_count: Inline::<inlineencodings::U256BE>::new([255; 32]),
        census_complete: Inline::<inlineencodings::Boolean>::new([2; 32]) };
    fragment += entity! { &unknown_type @ metadata::name: "/private/credential-canary", type_count: 999u128 };
    // A different endpoint on either join must not leak into this report.
    let other = SigningKey::from_bytes(&[74; 32]).verifying_key();
    for (endpoint, collector_endpoint) in [(other, signer), (signer, other)] {
        let collector = entity! { health_record::attrs::endpoint: collector_endpoint };
        let subject = entity! { health_record::attrs::endpoint: endpoint,
            health_record::attrs::node: collector.root().unwrap(), telemetry::attrs::role: "fd-diagnostics" };
        fragment += entity! { metadata::tag: KIND_FD_EXHAUSTION,
            health_record::attrs::session: session, sequence: 999u128,
            telemetry::attrs::subject: subject.root().unwrap() };
        fragment += collector;
        fragment += subject;
    }
    node.pile.commit(node.metrics, &node.signer, fragment).unwrap();
    node.pile.flush().unwrap();
    let report = read(&path, signer, Some(node.metrics.handle())).unwrap();
    assert!(!serde_json::to_string(&report).unwrap().contains("credential-canary"));
    let Report::Events { events, .. } = report else { panic!() };
    assert_eq!(events.values.len(), 1);
    assert_eq!(events.values[0].event, format!("{:X}", event.id));
    assert_eq!(events.values[0].codes, ["fd-limit-system"]);
    assert_eq!(events.values[0].error_site_subsystems, ["unknown"]);
    assert_eq!(events.values[0].pressure_owner, "unknown");
    assert!(events.values[0].wall_tai_ns_intervals.values.is_empty());
    assert!(events.values[0].sampled_high_water.values.is_empty());
    let census = &events.values[0].handler_snapshots.values[0];
    assert_eq!(census.fd_count.values, [8, 9]);
    assert!(census.census_complete.values.is_empty());
    assert!(census.mcp_http_accepted_streams.values.is_empty());
    assert!(census.types.iter().all(|kind| kind.counts.values.is_empty()));
    node.close().unwrap();
}

#[test]
fn fd_reader_bounds_are_explicit_not_a_latest_event_claim() {
    let (_dir, path, mut node) = fixture();
    append(&mut node, LIMIT + 1);
    let Report::Events { events, selection, .. } = read(&path, node.signer.verifying_key(), Some(node.metrics.handle())).unwrap() else { panic!() };
    assert_eq!(events.values.len(), LIMIT);
    assert!(events.truncated);
    assert!(selection.contains("not latest"));
    assert!(!bounded(0..LIMIT).truncated);
    assert!(bounded(0..LIMIT + 1).truncated);
    node.close().unwrap();
}

#[test]
fn fd_reader_empty_evidence_is_not_unavailable_foundations() {
    let (_dir, path, mut node) = fixture();
    let signer = node.signer.verifying_key();
    let selected = node.metrics.handle();
    let Report::Events { events, .. } = read(&path, signer, Some(selected)).unwrap() else { panic!() };
    assert!(events.values.is_empty());
    let mut source = MemoryRepo::default();
    let commit = source.commit(node.metrics, &node.signer,
        entity! { metadata::description: "private missing-payload canary" }).unwrap();
    node.pile.insert(CollectionRecord::Commit(commit)).unwrap();
    node.pile.flush().unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = read(&path, signer, Some(selected)).unwrap_err();
    assert!(error.to_string().contains("unavailable"));
    assert!(!format!("{error:#}").contains(&hex::encode(commit.data().raw)));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    node.close().unwrap();
}

#[test]
fn fd_reader_cli_never_creates_missing_files_or_accepts_key_option() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent.pile");
    let mut args = vec![OsString::from("inspect-fd-events"), "--pile".into(), path.clone().into_os_string(),
        "--public".into(), hex::encode(SigningKey::from_bytes(&[73; 32]).verifying_key().to_bytes()).into()];
    assert!(crate::node_cli::run(&args).is_err());
    assert!(!path.exists());
    args.extend([OsString::from("--key"), dir.path().join("absent.key").into_os_string()]);
    assert!(crate::node_cli::run(&args).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
