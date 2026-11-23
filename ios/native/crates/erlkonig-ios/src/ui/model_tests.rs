use super::*;
use crate::open_local;
use triblespace_core::{
    collection::{CollectionStoreExt, private_policy},
    metadata,
    prelude::*,
    repo::pile::PileRecords,
};

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
