use super::*;
use crate::open_local;
use triblespace_core::{
    collection::{CollectionStoreExt, private_policy},
    metadata,
    prelude::*,
    repo::pile::PileRecords,
};

#[test]
fn sync_shows_the_last_observed_membership_until_its_host_is_retired() {
    use crate::network::Observation;
    use std::sync::Mutex;
    use triblespace_net::clock::mono_now;

    let (command, _commands) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Status {
        revision: 1, running: true, ready: true, replay_complete: true,
        listening: true, activity_serial: 1, ..Status::default()
    }));
    let mut model = Model::new(ModelSource::Error(String::new()));
    model.replica = Some(Replica {
        command, activity_serial: 1.into(), status: shared.clone(),
    });
    model.activity_serial = 1;
    model.set_screen(Screen::Sync);
    model.tick();
    assert_eq!(model.phase(), "Starting network");
    assert!(model.peer_observation().is_none());

    let first = Arc::new(Observation {
        host_epoch: 1, sampled_at: mono_now().as_nanos(), connections: vec![],
    });
    {
        let mut s = shared.lock().unwrap();
        s.revision += 1;
        s.network = Some(first.clone());
    }
    model.tick();
    assert_eq!(model.phase(), "Listening");
    let first_epoch = model.table_epoch;
    std::thread::sleep(Duration::from_millis(5100));
    {
        let _busy_writer = shared.lock().unwrap();
        model.tick();
        assert_eq!(model.phase(), "No connected nodes · observed over 5 s ago");
        assert!(Arc::ptr_eq(model.status.network.as_ref().unwrap(), &first));
    }
    model.tick();
    assert_eq!(model.table_epoch, first_epoch);
    assert!(!model.fresh_network());
    {
        let mut s = shared.lock().unwrap();
        s.revision += 1;
        s.network = Some(Arc::new(Observation {
            sampled_at: mono_now().as_nanos(), ..(*first).clone()
        }));
    }
    model.tick();
    assert!(model.fresh_network());
    assert_eq!(model.phase(), "Listening");
    assert_eq!(model.table_epoch, first_epoch, "only the freshness key changes");

    model.set_active(false);
    assert_eq!(model.phase(), "Paused");
    assert!(model.peer_observation().is_none());
    model.set_active(true);
    assert_eq!(model.phase(), "Resuming");
    assert!(model.peer_observation().is_none());
    {
        let mut s = shared.lock().unwrap();
        s.revision += 1;
        s.activity_serial = model.activity_serial;
        s.network = None;
    }
    model.tick();
    assert_eq!(model.phase(), "Starting network");
    assert!(model.peer_observation().is_none());
    {
        let mut s = shared.lock().unwrap();
        s.revision += 1;
        s.listening = false;
        s.network = Some(Arc::new(Observation {
            host_epoch: 2, sampled_at: mono_now().as_nanos(), connections: vec![],
        }));
    }
    model.tick();
    assert_eq!(model.peer_observation().unwrap().host_epoch, 2);
    assert_eq!(model.phase(), "No connected nodes");
    {
        let mut s = shared.lock().unwrap();
        s.error = Some("host failed".into());
        s.revision += 1;
    }
    model.tick();
    assert_eq!(model.phase(), "Needs attention");
    assert!(model.peer_observation().is_none());
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
