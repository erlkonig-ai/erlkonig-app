//! CPU-only in-memory tests of the driver shared by both service modes.
//! No Runtime startup, peer, filesystem, environment, credentials or keychain.

use super::{maintenance_driver, AllAccessible, Event, Target};
use ed25519_dalek::SigningKey;
use faculties::archive_bm25::ArchiveTextBm25Mapping;
use faculties::schemas::blockdag::content_fact;
use triblespace::core::blob::encodings::succinctarchive::SuccinctArchiveBlob;
use triblespace::core::collection::{
    registered_targets, AdmissionPolicy, CollectionPolicy, CollectionSnapshotExt, CollectionStoreExt,
};
use triblespace::core::metadata;
use triblespace::core::repo::{memoryrepo::MemoryRepo, SnapshotSource};
use triblespace::macros::entity;

#[test]
fn registered_archive_mapping_retargets_with_ordinary_app_targets() {
    let host = SigningKey::from_bytes(&[81; 32]);
    let author = SigningKey::from_bytes(&[82; 32]);
    let mut store = MemoryRepo::for_host(host.verifying_key());
    let archive = store.collection("readable Archive", CollectionPolicy::new(
        AdmissionPolicy::Open, AdmissionPolicy::direct(author.verifying_key()),
    )).unwrap();
    let ordinary = store.collection("ordinary app root",
        faculties::collection_names::private_policy(host.verifying_key())).unwrap();
    let index = store.attach_with(archive, ArchiveTextBm25Mapping).unwrap();
    let ordinary_index = store.attach::<SuccinctArchiveBlob>(ordinary, ()).unwrap();
    store.commit(archive, &author, entity! { content_fact::payload: "first comet" }).unwrap();
    store.commit(ordinary, &host, entity! { metadata::name: "first ordinary" }).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    runtime.block_on(async {
        // Initial MAPs name the attachments. Registration supplies executable
        // mappings; the app's discovery does not invent index declarations.
        store.maintain_attached_with::<ArchiveTextBm25Mapping>(index, &host).await.unwrap();
        store.maintain_attached(ordinary_index, &host).await.unwrap();
        store.commit(archive, &author, entity! { content_fact::payload: "second comet" }).unwrap();
        store.commit(ordinary, &host, entity! { metadata::name: "second ordinary" }).unwrap();
        let snapshot = store.snapshot().unwrap();
        assert!(!snapshot.attached(index).unwrap().residual().is_empty());
        assert!(!snapshot.attached(ordinary_index).unwrap().residual().is_empty());
        let selected = AllAccessible::discover(&snapshot, host.verifying_key(), []).unwrap();
        assert!(selected.maintenance.contains(&index.handle()));
        assert!(selected.maintenance.contains(&ordinary.handle()));
        assert!(selected.maintenance.contains(&ordinary_index.handle()));
        let mut driver = maintenance_driver(selected.maintenance.into_iter().map(Target::Handle)).unwrap();
        assert_eq!(registered_targets(&snapshot, driver.registered()).unwrap().len(), 1);
        drop(snapshot);
        let mut events = Vec::new();
        let pass = driver.tick(&mut store, &host, &mut |event| events.push(event)).await.unwrap().unwrap();
        assert_eq!(pass.failures, 0, "{events:?}");
        assert!(events.iter().any(|event| matches!(event,
            Event::HopFinished { collection, publications, .. }
                if *collection == index.handle() && publications.maps > 0)));
        assert!(store.snapshot().unwrap().attached(index).unwrap().residual().is_empty());
        assert!(store.snapshot().unwrap().attached(ordinary_index).unwrap().residual().is_empty());

        // A newly saved root changes the target set. Retarget keeps the
        // original ordinary root and attachment alongside the Faculty index.
        let saved = store.collection("new saved app root",
            faculties::collection_names::private_policy(host.verifying_key())).unwrap();
        store.commit(archive, &author, entity! { content_fact::payload: "third comet" }).unwrap();
        store.commit(ordinary, &host, entity! { metadata::name: "third ordinary" }).unwrap();
        let selected = AllAccessible::discover(&store.snapshot().unwrap(), host.verifying_key(), [saved.handle()]).unwrap();
        assert!(selected.maintenance.contains(&saved.handle()));
        assert!(selected.maintenance.contains(&ordinary.handle()));
        assert!(selected.maintenance.contains(&ordinary_index.handle()));
        assert!(selected.maintenance.contains(&index.handle()));
        driver.retarget(selected.maintenance.into_iter().map(Target::Handle));
        let pass = driver.tick(&mut store, &host, &mut |_| {}).await.unwrap().unwrap();
        assert_eq!(pass.failures, 0);
        assert!(store.snapshot().unwrap().attached(index).unwrap().residual().is_empty());
        assert!(store.snapshot().unwrap().attached(ordinary_index).unwrap().residual().is_empty());
    });
}

#[test]
fn registered_archive_mapping_does_not_bypass_parent_read_admission() {
    let owner = SigningKey::from_bytes(&[83; 32]);
    let stranger = SigningKey::from_bytes(&[84; 32]);
    let mut store = MemoryRepo::for_host(owner.verifying_key());
    let archive = store.collection("private Archive",
        faculties::collection_names::private_policy(owner.verifying_key())).unwrap();
    let index = store.attach_with(archive, ArchiveTextBm25Mapping).unwrap();
    store.commit(archive, &owner, entity! { content_fact::payload: "private comet" }).unwrap();
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(
        store.maintain_attached_with::<ArchiveTextBm25Mapping>(index, &owner),
    ).unwrap();
    let snapshot = store.snapshot().unwrap();
    assert_eq!(registered_targets(&snapshot, &faculties::maintenance::mappings()).unwrap().len(), 1);
    let selected = AllAccessible::discover(&snapshot, stranger.verifying_key(), []).unwrap();
    assert!(selected.denied.contains(&archive.handle()));
    assert!(!selected.maintenance.contains(&archive.handle()));
    assert!(!selected.maintenance.contains(&index.handle()));
}
