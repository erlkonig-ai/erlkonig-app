use super::*;
use crate::open_local;
use triblespace_core::{
    collection::{CollectionStoreExt, private_policy},
    metadata,
    prelude::*,
    repo::pile::PileRecords,
};

#[test]
fn replay_heading_keeps_the_native_prefix_and_the_real_measurement_age() {
    let mut model = Model::new(ModelSource::Error(String::new()));
    model.status = Status::default();
    model.activity_serial = 3;
    model.status.ready = true;
    let mut progress = crate::ReplayProgress {
        prefix_bytes: 2_000_000_000, observed_bytes: 6_800_000_000,
        activity_serial: 3, records_per_second: None,
    };
    model.status.replay_progress = Some(progress);
    assert_eq!(model.phase(), "Reading local data · 2.0 GB of 6.8 GB");
    assert_eq!(model.progress(), Some((2_000_000_000, 6_800_000_000)));
    model.status.replay_progress.as_mut().unwrap().prefix_bytes = 6_799_999_984;
    assert_eq!(model.phase(), "Reading local data · 6.8 GB of 6.8 GB · 16 B left");
    progress.records_per_second = Some(crate::ReplayRate { records_per_second: 0, measured_at: Instant::now() });
    model.status.replay_progress = Some(progress);
    assert_eq!(model.phase(), "Reading local data · 2.0 GB of 6.8 GB\n0 local pile records/s");
    progress.prefix_bytes = 3_000_000_000;
    progress.records_per_second = Some(crate::ReplayRate {
        records_per_second: 4200, measured_at: Instant::now() - Duration::from_secs(8),
    });
    model.status.replay_progress = Some(progress);
    assert_eq!(model.phase(), "Reading local data · 3.0 GB of 6.8 GB\n4200 local pile records/s · measured 8 s ago");
    model.set_active(false);
    assert_eq!(model.phase(), "Paused");
    model.set_active(true);
    model.activity_serial = 4; // A new foreground request has no observations yet.
    assert_eq!(model.phase(), "Reading local data");
    assert_eq!(model.progress(), None);
}

#[test]
fn eof_is_visible_while_preparation_holds_back_choices_and_network_startup() {
    let dir = tempfile::tempdir().unwrap();
    let (key, mut pile) = open_local(dir.path()).unwrap();
    crate::collections::start_discovery(&mut pile, &key).unwrap();
    pile.close().unwrap();

    let (command, commands) = mpsc::channel();
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let status = Arc::new(std::sync::Mutex::new(Status::default()));
    let sessions = Arc::new(arc_swap::ArcSwapOption::empty());
    let replica = Replica { command, activity_serial: 0.into(), status: status.clone(), peer: sessions.clone() };
    replica.command.send(crate::Command::BeforePreparation(Box::new(move |_| {
        entered.send(()).unwrap();
        let _ = blocked.recv_timeout(Duration::from_secs(10));
    }))).unwrap();
    let mut model = Model::new(ModelSource::Error(String::new()));
    model.activity_serial = replica.set_active(true);
    model.replica = Some(replica);
    model.set_screen(Screen::Sync);
    let root = dir.path().to_owned();
    let worker = std::thread::Builder::new().stack_size(crate::WORKER_STACK_BYTES)
        .spawn(move || crate::worker(root, commands, status, sessions)).unwrap();
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    model.tick();
    let cut = model.status.replay_progress.unwrap();
    assert_eq!(cut.prefix_bytes, cut.observed_bytes);
    assert!(cut.prefix_bytes > 0);
    assert_eq!(model.phase(), format!("Preparing local collections · {} read", bytes(cut.prefix_bytes)));
    assert!(!model.accepts_intent() && !model.can_choose());
    assert!(model.peer_observation().is_none());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while model.phase() != "Listening" {
        assert!(Instant::now() < deadline);
        model.tick();
        std::thread::yield_now();
    }
    assert!(model.can_choose());
    drop(model);
    worker.join().unwrap();
}

#[test]
fn sync_reads_the_peer_while_the_status_writer_and_first_durable_turn_are_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let (key, mut pile) = open_local(dir.path()).unwrap();
    crate::collections::start_discovery(&mut pile, &key).unwrap();
    pile.close().unwrap();

    let (command, commands) = mpsc::channel();
    let (entered, waiting) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let shared = Arc::new(std::sync::Mutex::new(Status::default()));
    let sessions = Arc::new(arc_swap::ArcSwapOption::empty());
    let replica = Replica { command, activity_serial: 0.into(), status: shared.clone(), peer: sessions.clone() };
    replica.command.send(crate::Command::BeforeFirstPeerTurn(Box::new(move |status| {
        let _busy = status.lock().unwrap();
        entered.send(()).unwrap();
        let _ = blocked.recv_timeout(Duration::from_secs(20));
    }))).unwrap();
    let serial = replica.set_active(true);
    let root = dir.path().to_owned();
    let worker = std::thread::Builder::new().stack_size(crate::WORKER_STACK_BYTES)
        .spawn(move || crate::worker(root, commands, shared, sessions)).unwrap();
    let mut model = Model::new(ModelSource::Error(String::new()));
    model.status = Status::default();
    model.activity_serial = serial;
    model.replica = Some(replica);
    waiting.recv_timeout(Duration::from_secs(10)).unwrap();

    model.tick(); // Crown does not sample the peer.
    assert!(model.peer_reader.is_none() && model.peer_view.is_none());
    model.set_screen(Screen::Sync);
    let deadline = Instant::now() + Duration::from_secs(5);
    while model.peer_observation().is_none() {
        assert!(Instant::now() < deadline, "actual empty host sample unavailable");
        model.tick();
        std::thread::yield_now();
    }
    assert_eq!(model.phase(), "No connected nodes");
    assert!(!model.status.ready, "the worker status could not be acquired");
    let reader = model.peer_reader.clone().unwrap();
    let pinned = reader.snapshot().unwrap();
    let stamp = pinned.observed_at.unwrap();
    let epoch = model.table_epoch;

    while model.peer_observation().unwrap().sampled_at <= stamp {
        assert!(Instant::now() < deadline, "host evidence waited for a disk/worker turn");
        model.tick();
        std::thread::yield_now();
    }
    assert_eq!(pinned.observed_at, Some(stamp));
    assert_eq!(pinned.connections.len(), 0);
    assert_eq!(model.table_epoch, epoch, "empty heartbeat did not change table content");
    let start = Instant::now();
    for _ in 0..10_000 { model.tick(); }
    let visible = start.elapsed();
    assert_eq!(model.table_epoch, epoch);
    model.set_screen(Screen::Crown);
    let start = Instant::now();
    for _ in 0..10_000 { model.tick(); }
    eprintln!("10k empty peer frame polls with worker lock held: visible {visible:?}, hidden {:?}", start.elapsed());
    assert!(model.peer_reader.is_none() && model.peer_view.is_none());
    model.set_screen(Screen::Sync);
    model.tick();

    // A pinned screen does not become fresh merely because the host keeps running.
    std::thread::sleep(Duration::from_millis(5100));
    assert_eq!(model.phase(), "No connected nodes · observed over 5 s ago");
    model.tick();
    assert_eq!(model.phase(), "No connected nodes");
    model.set_screen(Screen::Crown);
    assert!(model.peer_reader.is_none() && model.peer_view.is_none());
    model.set_screen(Screen::Sync);
    model.tick();
    assert!(model.peer_observation().is_some());

    model.set_active(false);
    assert_eq!(model.phase(), "Paused");
    assert!(model.peer_observation().is_none());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while reader.snapshot().is_some() {
        assert!(Instant::now() < deadline, "paused peer retained live evidence");
        std::thread::yield_now();
    }
    assert_eq!(pinned.observed_at, Some(stamp));
    model.set_active(true);
    assert_eq!(model.phase(), "Reading local data");
    let deadline = Instant::now() + Duration::from_secs(10);
    while model.phase() != "Listening" {
        assert!(Instant::now() < deadline, "new listener did not establish a current zero");
        model.tick();
        std::thread::yield_now();
    }
    assert!(model.peer_reader.as_ref().is_some_and(|new| new != &reader));
    model.status.error = Some("host failed".into());
    assert_eq!(model.phase(), "Needs attention");
    assert!(model.peer_observation().is_none());
    drop(model);
    worker.join().unwrap();
}

#[test]
fn completed_visible_query_survives_new_prefixes_but_cannot_cross_owners() {
    let dir = tempfile::tempdir().unwrap();
    let (key, mut pile) = open_local(dir.path()).unwrap();
    let reader = key.verifying_key();
    let root = pile.collection("visible", private_policy(reader)).unwrap();
    pile.commit(root, &key, entity! {metadata::name:"first"})
        .unwrap();
    let mut initial = Status::default();
    initial.offer_snapshot(&pile.snapshot().unwrap(), reader, true);
    let basis = initial.list_snapshot.clone().unwrap();
    let mut engine = CollectionPages::default();
    let old_page = engine.read(&basis, 0).unwrap();
    let mut model = Model::new(ModelSource::Error(String::new()));
    model.status = initial.clone();
    model.foreground = true;
    model.set_screen(Screen::Collections);
    let (tx, rx) = mpsc::channel();
    model.reading = Some(rx);
    for name in ["second", "third", "fourth"] {
        pile.commit(root, &key, entity! {metadata::name:name})
            .unwrap();
        model
            .status
            .offer_snapshot(&pile.snapshot().unwrap(), reader, true);
        *model.latest_list.lock().unwrap() = model.status.list_snapshot.clone();
        model.pages_tick();
        assert!(
            model.reading.is_some(),
            "a new prefix must not cancel the current query"
        );
    }
    let latest = model.status.list_snapshot.clone().unwrap();
    assert!(old_page.metadata_current(&latest));
    tx.send(PageRead {
        basis: initial.clone(),
        verified: Some(latest.clone()),
        result: Ok(vec![old_page.clone()]),
        readers: vec![(0, engine)],
    })
    .unwrap();
    model.pages_tick();
    assert_eq!(model.row(0).unwrap().counts.as_ref().unwrap().records, 1);
    assert_eq!(
        model.list_status().list_snapshot.as_ref().unwrap().revision,
        basis.revision
    );
    assert!(
        model.reading.is_some(),
        "catch up without clearing the completed page"
    );
    let until = Instant::now() + Duration::from_secs(10);
    while model.list_status().list_snapshot.as_ref().unwrap().revision != latest.revision {
        assert!(Instant::now() < until, "reader did not catch up");
        std::thread::yield_now();
        model.pages_tick();
    }
    assert_eq!(model.row(0).unwrap().counts.as_ref().unwrap().records, 4);
    let two = tempfile::tempdir().unwrap();
    let (k, mut other) = open_local(two.path()).unwrap();
    let mut replacement = Status::default();
    replacement.offer_snapshot(&other.snapshot().unwrap(), k.verifying_key(), true);
    assert_eq!(
        replacement.list_snapshot.as_ref().unwrap().revision,
        basis.revision
    );
    model.invalidate_window();
    model.status = replacement;
    *model.latest_list.lock().unwrap() = model.status.list_snapshot.clone();
    let (tx, rx) = mpsc::channel();
    model.reading = Some(rx);
    tx.send(PageRead {
        basis: initial,
        verified: Some(latest),
        result: Ok(vec![old_page]),
        readers: Vec::new(),
    })
    .unwrap();
    model.pages_tick();
    assert!(
        model.row(0).is_none(),
        "a delayed result for another owner cannot show up"
    );
}

#[test]
fn hidden_views_do_not_start_queries_and_disconnected_reads_settle() {
    let mut model = Model::new(ModelSource::Error("not opening a store".into()));
    model.demand(64);
    model.pages_tick();
    assert!(model.reading.is_none());
    model.set_screen(Screen::Collections);
    let (tx, rx) = mpsc::channel();
    drop(tx);
    model.reading = Some(rx);
    model.pages_tick();
    assert!(model.reading.is_none());
    assert_eq!(
        model.list_error.as_deref(),
        Some("Collection reader stopped")
    );
}
