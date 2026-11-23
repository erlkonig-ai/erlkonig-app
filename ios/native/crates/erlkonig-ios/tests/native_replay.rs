//! Public stepped-snapshot contract under the retained app dependency lock.
use triblespace_core::{
    collection::{CollectionStoreExt, HeldRead, HeldStore, private_policy},
    metadata, prelude::*, repo::{BlobStoreList, pile::Pile},
};

#[test]
fn stepped_prefixes_remain_immutable_and_held_waits_for_the_first_complete_observation() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
    let mut writer = Pile::open_as(file.path(), key.verifying_key()).unwrap();
    let collection = writer.collection("stepped", private_policy(key.verifying_key())).unwrap();
    writer.commit(collection, &key, entity! { metadata::name: "first" }).unwrap();
    let tail = writer.put::<blobencodings::UTF8String, _>("last blob").unwrap();
    writer.close().unwrap();

    let mut reader = Pile::open_as(file.path(), key.verifying_key()).unwrap();
    assert_eq!(reader.replayed_length(), 0);
    reader.track_held([collection.handle()]);
    let mut held = reader.drive_held().unwrap();
    let (first, complete) = reader.snapshot_step(1).unwrap();
    assert!(!complete);
    assert!(!first.contains_blob(tail).unwrap());
    assert!(first.held(collection.handle()).is_none());
    assert!(held.run_until_idle());
    let mut position = reader.replayed_length();
    let last = loop {
        let (next, complete) = reader.snapshot_step(1).unwrap();
        assert!(reader.replayed_length() > position);
        position = reader.replayed_length();
        if complete { break next; }
        assert!(held.run_until_idle());
        assert!(next.held(collection.handle()).is_none());
    };
    assert_eq!(position, file.as_file().metadata().unwrap().len());
    assert!(last.contains_blob(tail).unwrap());
    assert!(!first.contains_blob(tail).unwrap(), "old prefix advanced with its source");
    assert!(first.held(collection.handle()).is_none());
    assert!(held.run_until_idle());
    let (settled, complete) = reader.snapshot_step(1).unwrap();
    assert!(complete);
    assert!(settled.held(collection.handle()).is_some());
    drop(held);
    reader.close().unwrap();
}

#[test]
fn snapshot_prefix_stays_immutable_while_the_observed_pile_grows() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[63; 32]);
    let mut writer = Pile::open_as(file.path(), key.verifying_key()).unwrap();
    writer.put::<blobencodings::UTF8String, _>("first").unwrap();
    writer.put::<blobencodings::UTF8String, _>("second").unwrap();
    writer.flush().unwrap();
    let original_end = file.as_file().metadata().unwrap().len() as usize;
    let mut reader = Pile::open_as(file.path(), key.verifying_key()).unwrap();
    let (first, complete) = reader.snapshot_step(1).unwrap();
    assert!(!complete && first.prefix_len() > 0);
    assert!(first.prefix_len() < original_end);
    assert_eq!(reader.observed_length().unwrap(), original_end as u64);
    let original_prefix = first.prefix_len();
    writer.put::<blobencodings::UTF8String, _>("appended").unwrap();
    writer.flush().unwrap();
    let appended_end = file.as_file().metadata().unwrap().len() as usize;
    assert_eq!(reader.observed_length().unwrap(), appended_end as u64);
    assert_eq!(first.prefix_len(), original_prefix);
    let (whole, complete) = reader.snapshot_step(u64::MAX).unwrap();
    assert!(complete);
    assert_eq!(whole.prefix_len() as u64, reader.observed_length().unwrap());
    let bounded = triblespace_core::repo::pile::PileFile::open_prefix(file.path(), original_prefix as u64).unwrap();
    let snapshot = bounded.snapshot_replayed();
    assert_eq!(snapshot.prefix_len(), original_prefix);
    assert_eq!(bounded.observed_length().unwrap(), original_prefix as u64);
    bounded.close().unwrap();
    reader.close().unwrap(); writer.close().unwrap();
}

#[test]
fn empty_snapshot_progress_is_zero_and_complete() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[64; 32]);
    let mut reader = Pile::open_as(file.path(), key.verifying_key()).unwrap();
    let (snapshot, complete) = reader.snapshot_step(1).unwrap();
    assert!(complete);
    assert_eq!(snapshot.prefix_len(), 0);
    assert_eq!(reader.observed_length().unwrap(), 0);
    reader.close().unwrap();
}
