use super::*;
use triblespace_core::{
    blob::IntoBlob, metadata,
    collection::{CollectionCommit, CollectionRecord, CollectionStore, CollectionStoreExt, HeldStore, private_policy},
};

#[test]
fn sparse_size_updates_keep_only_the_latest_authorized_measurement() -> Result<()> {
    let first = tempfile::tempdir()?;
    let (reader, mut pile) = crate::open_local(first.path())?;
    let root = pile.collection("known size", private_policy(reader.verifying_key()))?.handle();
    let snapshot = pile.snapshot()?;
    let previous = |bytes| ByteObservation { root: hex::encode(root.raw), resident_bytes: Some(bytes),
        resident_error: None, pending: false };
    let mut batch = IncrementalSample::new(snapshot, reader.verifying_key(), &[root], None);
    for value in [12_000_000_000, 0] {
        let pending = batch.pending_rows(&[previous(value)]);
        assert!(pending[0].pending);
        assert_eq!(pending[0].resident_bytes, Some(value));
    }
    let completed = batch.step(4096).unwrap().0;
    assert!(!completed[0].pending);
    assert!(completed[0].resident_bytes.is_none(), "an unknown held inventory supersedes the old measure");
    batch.fail("inventory unavailable".into());
    let failed = batch.pending_rows(&[previous(12_000_000_000)]);
    assert!(!failed[0].pending && failed[0].resident_bytes.is_none() && failed[0].resident_error.is_some());
    // An identical requested collection ID in a different replica carries no prior size or authority.
    let second = tempfile::tempdir()?;
    let (other, mut other_pile) = crate::open_local(second.path())?;
    let denied = IncrementalSample::new(other_pile.snapshot()?, other.verifying_key(), &[root], None)
        .pending_rows(&[previous(12_000_000_000)]);
    assert!(denied[0].resident_error.is_some() && denied[0].resident_bytes.is_none() && !denied[0].pending);
    pile.close()?;
    other_pile.close()?;
    Ok(())
}

#[test]
fn byte_failure_preserves_ready_records_without_publishing_a_byte_total() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, mut pile) = crate::open_local(dir.path())?;
    let root = pile.collection("byte failure", private_policy(key.verifying_key()))?.handle();
    let data = pile.put::<blobencodings::UTF8String, _>("direct")?;
    let meta = pile.put::<blobencodings::SimpleArchive, _>(TribleSet::new())?;
    pile.insert(CollectionRecord::Commit(CollectionCommit::sign(
        &key, root, *data.as_transmute(), meta,
    )))?;
    let snapshot = pile.snapshot()?;
    let page = crate::collections::project(&snapshot, key.verifying_key(), 0, 32)?;
    let counts = page.rows[0].counts.clone();
    let mut sample = IncrementalSample::new(snapshot.clone(), key.verifying_key(), &[root], None);
    assert_eq!(counts, Some(Counts { records: 1, direct_present: 1 }));
    assert_eq!(sample.observations()[0].resident_bytes, None);
    sample.fail("synthetic held-byte failure".into());
    let (rows, completed) = sample.step(1).unwrap();
    assert!(completed.is_none());
    assert_eq!(crate::collections::project(&snapshot, key.verifying_key(), 0, 32)?.rows[0].counts, counts);
    assert_eq!(rows[0].resident_bytes, None);
    assert_eq!(rows[0].resident_error.as_deref(), Some("synthetic held-byte failure"));
    pile.close()?;
    Ok(())
}

#[test]
fn late_direct_blobs_change_only_ready_count_and_duplicate_records_do_not_inflate() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, mut pile) = crate::open_local(dir.path())?;
    let root = pile.collection("storage", private_policy(key.verifying_key()))?.handle();
    let data = IntoBlob::<blobencodings::SimpleArchive>::to_blob(entity! { metadata::name: "sample" }.facts().clone());
    let metadata_blob = IntoBlob::<blobencodings::SimpleArchive>::to_blob(entity! { metadata::name: "metadata" }.facts().clone());
    let record = CollectionRecord::Commit(CollectionCommit::sign(&key, root,
        data.get_handle().as_transmute().clone(), metadata_blob.get_handle()));
    pile.insert(record)?;
    pile.insert(record)?;
    let before = pile.snapshot()?;
    assert_eq!(measure(&before, root)?, Counts { records: 1, direct_present: 0 });
    pile.put::<blobencodings::SimpleArchive, _>(data)?;
    assert_eq!(measure(&pile.snapshot()?, root)?, Counts { records: 1, direct_present: 0 });
    pile.put::<blobencodings::SimpleArchive, _>(metadata_blob)?;
    assert_eq!(measure(&pile.snapshot()?, root)?, Counts { records: 1, direct_present: 1 });
    assert_eq!(measure(&before, root)?, Counts { records: 1, direct_present: 0 });
    pile.flush()?; pile.close()?;
    // Actual duplicate physical frames, not only idempotent typed insertion.
    let path = dir.path().join("replica.pile");
    let bytes = std::fs::read(&path)?;
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).open(&path)?.write_all(&bytes)?;
    let (_, mut reopened) = crate::open_local(dir.path())?;
    assert_eq!(measure(&reopened.snapshot()?, root)?, Counts { records: 1, direct_present: 1 });
    reopened.close()?;
    Ok(())
}

#[test]
fn held_sizes_include_shared_children_once_per_root_and_sampling_is_bounded() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, mut pile) = crate::open_local(dir.path())?;
    let roots = [
        pile.collection("first", private_policy(key.verifying_key()))?.handle(),
        pile.collection("second", private_policy(key.verifying_key()))?.handle(),
    ];
    let child = IntoBlob::<blobencodings::UTF8String>::to_blob("x".repeat(2 * 1024 * 1024));
    let child_handle = child.get_handle();
    pile.put::<blobencodings::UTF8String, _>(child)?;
    let data = IntoBlob::<blobencodings::SimpleArchive>::to_blob(entity! { metadata::description: child_handle }.facts().clone());
    let data_handle = data.get_handle();
    let meta = IntoBlob::<blobencodings::SimpleArchive>::to_blob(TribleSet::new());
    let meta_handle = meta.get_handle();
    pile.put::<blobencodings::SimpleArchive, _>(data)?;
    pile.put::<blobencodings::SimpleArchive, _>(meta)?;
    for root in roots {
        let record = CollectionRecord::Commit(CollectionCommit::sign(&key, root,
            data_handle.as_transmute().clone(), meta_handle));
        pile.insert(record)?; pile.insert(record)?;
    }
    let mut unknown = Sample::new(pile.snapshot()?, key.verifying_key(), &roots);
    assert!(unknown.step(0).unwrap().iter().all(|row| row.resident_bytes.is_none()));
    pile.track_held(roots);
    assert!(pile.snapshot()?.held(roots[0]).is_none()); // Building is not zero.
    let mut work = pile.drive_held().unwrap();
    assert!(work.run_until_idle());
    let snapshot = pile.snapshot()?;
    let mut sample = Sample::new(snapshot.clone(), key.verifying_key(), &roots);
    assert!(sample.step(0).is_none());
    assert!(sample.step(1).is_none()); // No partial byte sum escapes.
    let rows = loop { if let Some(rows) = sample.step(1) { break rows; } };
    // Independently assemble the descriptor producer's embedded dependencies,
    // rather than defining the expected total by walking the held inventory.
    let policy = private_policy(key.verifying_key()).fragment();
    let encoding = <blobencodings::SimpleArchive as metadata::MetaDescribe>::describe();
    let mut dependencies = BTreeSet::new();
    for handle in find!(handle: triblespace_core::capability::CapabilityHandle,
        pattern!(policy.facts(), [{ triblespace_core::capability::policy::capability_handle: ?handle }])) {
        dependencies.insert(handle.raw);
    }
    for handle in find!(handle: Inline<Handle<blobencodings::UTF8String>>,
        pattern!(encoding.facts(), [{ metadata::name: ?handle }])) { dependencies.insert(handle.raw); }
    for handle in find!(handle: Inline<Handle<blobencodings::UTF8String>>,
        pattern!(encoding.facts(), [{ metadata::description: ?handle }])) { dependencies.insert(handle.raw); }
    let embedded_bytes = dependencies.into_iter().map(|raw|
        snapshot.blob_info(Inline::<Handle<blobencodings::UnknownBlob>>::new(raw)).unwrap().unwrap().length).sum::<u64>();
    for (index, root) in roots.into_iter().enumerate() {
        let held = snapshot.held(root).unwrap();
        assert!(held.has_prefix(&child_handle.raw));
        let expected = [root.raw, data_handle.raw, meta_handle.raw, child_handle.raw]
            .into_iter().collect::<BTreeSet<_>>().into_iter()
            .map(|raw| snapshot.blob_info(Inline::<Handle<blobencodings::UnknownBlob>>::new(raw)).unwrap().unwrap().length)
            .sum::<u64>();
        // Also include the descriptor's separate UTF-8 collection name.
        let descriptor_dependencies = ["first", "second"][index].len() as u64 + embedded_bytes;
        assert_eq!(rows[index].resident_bytes, Some(expected + descriptor_dependencies));
        assert_eq!(rows[index].counts.as_ref().unwrap().records, 1);
    }
    drop(work); drop(sample); drop(snapshot);
    pile.close()?;
    Ok(())
}

#[test]
fn late_held_arrival_changes_bytes_without_changing_direct_counts_or_old_snapshot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, mut pile) = crate::open_local(dir.path())?;
    let root = pile.collection("late child", private_policy(key.verifying_key()))?.handle();
    let child = IntoBlob::<blobencodings::UTF8String>::to_blob("arrives later");
    let child_handle = child.get_handle();
    let data = IntoBlob::<blobencodings::SimpleArchive>::to_blob(entity! { metadata::description: child_handle }.facts().clone());
    let meta = IntoBlob::<blobencodings::SimpleArchive>::to_blob(TribleSet::new());
    pile.insert(CollectionRecord::Commit(CollectionCommit::sign(&key, root, data.get_handle().as_transmute().clone(), meta.get_handle())))?;
    pile.put::<blobencodings::SimpleArchive, _>(data)?;
    pile.put::<blobencodings::SimpleArchive, _>(meta)?;
    pile.track_held([root]); pile.snapshot()?;
    let mut work = pile.drive_held().unwrap(); work.run_until_idle();
    let before = pile.snapshot()?;
    let mut old = Sample::new(before.clone(), key.verifying_key(), &[root]);
    assert!(old.step(1).is_none());
    pile.put::<blobencodings::UTF8String, _>(child)?;
    // Existing native peer evidence is how a late non-seed child becomes held;
    // the UI never triggers this or a payload traversal.
    pile.note_held(root, *child_handle.as_transmute()); pile.snapshot()?;
    work.run_until_idle();
    let after = pile.snapshot()?;
    assert_ne!(before.held_generation(), after.held_generation());
    let old_rows = old.step(4096).unwrap();
    let new_rows = Sample::new(after.clone(), key.verifying_key(), &[root]).step(4096).unwrap();
    assert_eq!(old_rows[0].counts, new_rows[0].counts);
    let child_bytes = after.blob_info(child_handle)?.unwrap().length;
    assert_eq!(new_rows[0].resident_bytes, old_rows[0].resident_bytes.map(|bytes| bytes + child_bytes));
    assert!(!before.contains_blob(child_handle)?);
    drop(work); drop(old); drop(before); drop(after);
    pile.close()?;
    Ok(())
}

#[test]
fn referenced_child_absence_is_not_recursive_completion_and_errors_are_not_zero() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, mut pile) = crate::open_local(dir.path())?;
    let root = pile.collection("direct-only", private_policy(key.verifying_key()))?.handle();
    let child = IntoBlob::<blobencodings::UTF8String>::to_blob("child deliberately absent");
    let child_handle = child.get_handle();
    let data = IntoBlob::<blobencodings::SimpleArchive>::to_blob(entity! { metadata::description: child_handle }.facts().clone());
    let metadata_blob = IntoBlob::<blobencodings::SimpleArchive>::to_blob(TribleSet::new());
    let record = CollectionRecord::Commit(CollectionCommit::sign(&key, root,
        data.get_handle().as_transmute().clone(), metadata_blob.get_handle()));
    pile.insert(record)?; pile.put::<blobencodings::SimpleArchive, _>(data)?; pile.put::<blobencodings::SimpleArchive, _>(metadata_blob)?;
    let snapshot = pile.snapshot()?;
    assert!(!snapshot.contains_blob(child_handle)?);
    assert_eq!(measure(&snapshot, root)?, Counts { records: 1, direct_present: 1 });
    assert_eq!(observe(&snapshot, key.verifying_key(), root).counts.unwrap().direct_present, 1);
    let unavailable = observe(&snapshot, key.verifying_key(), selection::config_handle(key.verifying_key()));
    assert!(unavailable.counts.is_none() && unavailable.error.is_some());
    pile.close()?;
    Ok(())
}

#[test]
fn unreadable_unselected_root_is_unavailable_like_the_collections_page() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (phone, mut pile) = crate::open_local(dir.path())?;
    let owner = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
    let root = pile.collection("unreadable", private_policy(owner.verifying_key()))?.handle();
    let config = pile.collection(selection::CONFIG_COLLECTION_NAME, private_policy(phone.verifying_key()))?;
    selection::write_sync_selection(&mut pile, config, &phone, root, false)?;
    assert!(crate::collections::page(&mut pile, &phone, 0)?.rows.is_empty());
    let observation = observe(&pile.snapshot()?, phone.verifying_key(), root);
    assert!(observation.counts.is_none() && observation.error.is_some());
    pile.close()?;
    Ok(())
}

#[test]
fn composition_merges_three_way_sharing_keeps_unknown_and_finishes_old_snapshot() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, mut pile) = crate::open_local(dir.path())?;
    let roots = ["first", "second", "third"].map(|name|
        pile.collection(name, private_policy(key.verifying_key())).unwrap().handle());
    let unknown = pile.collection("untracked", private_policy(key.verifying_key()))?.handle();
    let config = pile.collection(selection::CONFIG_COLLECTION_NAME, private_policy(key.verifying_key()))?;
    selection::write_sync_selection(&mut pile, config, &key, roots[1], false)?;
    // All three inventories share a directly described child which arrives later.
    let child = IntoBlob::<blobencodings::UTF8String>::to_blob("shared-three-ways".repeat(1024));
    let child_handle = child.get_handle();
    let child_length = "shared-three-ways".len() as u64 * 1024;
    let data = IntoBlob::<blobencodings::SimpleArchive>::to_blob(entity! { metadata::description: child_handle }.facts().clone());
    let meta = IntoBlob::<blobencodings::SimpleArchive>::to_blob(TribleSet::new());
    for root in roots.into_iter().chain([unknown]) {
        pile.insert(CollectionRecord::Commit(CollectionCommit::sign(&key, root,
            data.get_handle().as_transmute().clone(), meta.get_handle())))?;
    }
    pile.put::<blobencodings::SimpleArchive, _>(data)?;
    pile.put::<blobencodings::SimpleArchive, _>(meta)?;
    pile.track_held(roots); pile.snapshot()?;
    let mut work = pile.drive_held().unwrap(); assert!(work.run_until_idle());
    let before = pile.snapshot()?;
    assert!(before.held(unknown).is_none());
    let mut old = CompositionSample::new(before.clone(), key.verifying_key())?;
    assert!(old.step(0).is_none());
    assert!(old.step(1).is_none());
    let baseline = CompositionSample::new(before.clone(), key.verifying_key())?.step(4096).unwrap();
    assert!(baseline.error.is_none());
    assert_eq!((baseline.known_inventories, baseline.unknown_inventories), (3, 1));
    assert_eq!(baseline.segments.len(), 3);
    assert!(baseline.segments.iter().any(|s| s.root == hex::encode(roots[1].raw)));
    // Only descriptor and its distinct UTF-8 name are exclusive per root here;
    // shared archives, policy and encoding dependencies belong in Shared.
    for (root, name) in roots.into_iter().zip(["first", "second", "third"]) {
        let exclusive = before.blob_info(root)?.unwrap().length + name.len() as u64;
        assert_eq!(baseline.segments.iter().find(|s| s.root == hex::encode(root.raw)).unwrap().bytes, exclusive);
    }
    pile.put::<blobencodings::UTF8String, _>(child)?;
    for root in roots { pile.note_held(root, *child_handle.as_transmute()); }
    pile.snapshot()?; assert!(work.run_until_idle());
    let after = pile.snapshot()?;
    let old_result = loop { if let Some(value) = old.step(1) { break value; } };
    assert_eq!(old_result.known_bytes, baseline.known_bytes);
    assert_eq!(old_result.shared_bytes, baseline.shared_bytes);
    let current = CompositionSample::new(after.clone(), key.verifying_key())?.step(4096).unwrap();
    assert_eq!(current.known_bytes, baseline.known_bytes + child_length);
    assert_eq!(current.shared_bytes, baseline.shared_bytes + child_length); // Once, not three times.
    assert_eq!(current.segments.iter().map(|s| s.bytes).collect::<Vec<_>>(),
        baseline.segments.iter().map(|s| s.bytes).collect::<Vec<_>>());
    assert_eq!(current.known_bytes, current.shared_bytes + current.segments.iter().map(|s| s.bytes).sum::<u64>());
    let unknown_rows = Sample::new(after.clone(), key.verifying_key(), &[unknown]).step(0).unwrap();
    assert!(unknown_rows[0].resident_bytes.is_none());
    // Overflow produces only an unavailable result, never an accumulated prefix.
    let mut overflow = CompositionSample::new(after.clone(), key.verifying_key())?;
    overflow.result.known_bytes = u64::MAX;
    let invalid = overflow.step(4096).unwrap();
    assert!(invalid.error.as_ref().unwrap().contains("overflow"));
    assert!(invalid.segments.is_empty());
    drop(work); drop(old); drop(overflow); drop(before); drop(after);
    pile.flush()?; pile.close()?;
    let (_, mut reopened) = crate::open_local(dir.path())?;
    let restarted = CompositionSample::new(reopened.snapshot()?, key.verifying_key())?.step(0).unwrap();
    assert_eq!((restarted.known_inventories, restarted.unknown_inventories), (0, 4));
    assert!(restarted.segments.is_empty());
    reopened.close()?;
    Ok(())
}


#[test]
fn native_incremental_rows_match_oracle_and_keep_access_checks() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, mut pile) = crate::open_local(dir.path())?;
    let root = pile.collection("native rows", private_policy(key.verifying_key()))?.handle();
    let data = pile.put::<blobencodings::SimpleArchive, _>(TribleSet::new())?;
    pile.insert(CollectionRecord::Commit(CollectionCommit::sign(&key, root, *data.as_transmute(), data)))?;
    let snapshot = pile.snapshot()?;
    let roots = [root, selection::config_handle(key.verifying_key())];
    let mut initial = IncrementalSample::new(snapshot.clone(), key.verifying_key(), &roots, None);
    let (rows, summary) = initial.step(4096).unwrap();
    let page = crate::collections::project(&snapshot, key.verifying_key(), 0, 32)?;
    assert_eq!(page.rows[0].counts, Some(measure(&snapshot, root)?));
    assert!(rows[1].resident_error.is_some());
    let summary = summary.unwrap();
    let mut warm = IncrementalSample::new(snapshot, key.verifying_key(), &roots, Some(&summary));
    let (again, next) = warm.step(0).unwrap();
    assert_eq!(again[0].resident_bytes, rows[0].resident_bytes);
    assert!(again[1].resident_error.is_some());
    assert_eq!(next.unwrap().work(), triblespace_net::storage_snapshot::StorageWork::default());
    pile.close()?; Ok(())
}

#[test]
fn native_incremental_composition_matches_oracle_and_eligible_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (key, mut pile) = crate::open_local(dir.path())?;
    let roots = ["native chart one", "native chart two"].map(|name|
        pile.collection(name, private_policy(key.verifying_key())).unwrap().handle());
    let stranger = ed25519_dalek::SigningKey::from_bytes(&[73; 32]);
    let denied = pile.collection("not this reader", private_policy(stranger.verifying_key()))?.handle();
    let config = pile.collection(selection::CONFIG_COLLECTION_NAME, private_policy(key.verifying_key()))?;
    selection::write_sync_selection(&mut pile, config, &key, roots[1], false)?;
    selection::write_sync_selection(&mut pile, config, &key, denied, false)?;
    let data = pile.put::<blobencodings::SimpleArchive, _>(TribleSet::new())?;
    for root in roots {
        pile.insert(CollectionRecord::Commit(CollectionCommit::sign(&key, root, *data.as_transmute(), data)))?;
    }
    pile.track_held(roots); pile.snapshot()?;
    let mut work = pile.drive_held().unwrap(); assert!(work.run_until_idle());
    let snapshot = pile.snapshot()?;
    let expected = CompositionSample::new(snapshot.clone(), key.verifying_key())?.step(4096).unwrap();
    let mut initial = IncrementalCompositionSample::new(snapshot.clone(), key.verifying_key(), None)?;
    assert!(initial.step(1).is_none());
    let (actual, summary) = initial.step(4096).unwrap();
    assert_eq!(serde_json::to_value(&actual)?, serde_json::to_value(&expected)?);
    assert_eq!(actual.segments.len(), 2);
    assert!(actual.segments.iter().any(|s| s.root == hex::encode(roots[1].raw)));
    assert!(!actual.segments.iter().any(|s| s.root == hex::encode(denied.raw)));
    let summary = summary.unwrap();
    let mut warm = IncrementalCompositionSample::new(snapshot, key.verifying_key(), Some(&summary))?;
    let (again, next) = warm.step(0).unwrap();
    assert_eq!(serde_json::to_value(&again)?, serde_json::to_value(&actual)?);
    assert_eq!(next.unwrap().work(), triblespace_net::storage_snapshot::StorageWork::default());
    drop(work); pile.close()?; Ok(())
}
