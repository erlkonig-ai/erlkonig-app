use super::*;
use triblespace_core::{blob::IntoBlob, collection::{AdmissionPolicy, CollectionPolicy, grant_collection_read}, id::genid};

#[test]
fn legacy_join_keeps_listener_and_atlas_but_late_foreign_grants_stay_off() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (phone, mut pile) = crate::open_local(dir.path())?;
    let owner = SigningKey::from_bytes(&[41; 32]);
    let other = SigningKey::from_bytes(&[42; 32]);
    let atlas = pile.collection("atlas", private_policy(owner.verifying_key()))?;
    grant_collection_read(&mut pile, atlas.handle(), &owner, phone.verifying_key())?;
    let config = pile.collection(CONFIG_COLLECTION_NAME, private_policy(phone.verifying_key()))?;
    let archive = IntoBlob::<blobencodings::SimpleArchive>::to_blob(
        AdmissionPolicy::direct(owner.verifying_key()).fragment().facts().clone());
    pile.commit(config, &phone, entity! {
        attrs::config_anchor: attrs::NODE_SETTINGS, attrs::sync_enabled: true,
        metadata::archive: archive,
    })?;
    selection::write_sync_selection(&mut pile, config, &phone, atlas.handle(), true)?;
    assert!(listening(&mut pile, &phone)?);
    let later = pile.collection("later", private_policy(other.verifying_key()))?;
    assert!(!page(&mut pile, &phone, 0)?.rows.iter().any(|r| r.root == hex::encode(later.handle().raw)));
    grant_collection_read(&mut pile, later.handle(), &other, phone.verifying_key())?;
    let rows = page(&mut pile, &phone, 0)?.rows;
    let new = rows.iter().find(|r| r.root == hex::encode(later.handle().raw)).unwrap();
    assert!(new.readable && !new.selected && new.state == "unset");
    assert_eq!(crate::selected(&mut pile, &phone)?, vec![atlas.handle()]);
    choose(&mut pile, &phone, later.handle(), "unset", true)?;
    choose(&mut pile, &phone, later.handle(), "selected", false)?;
    assert!(choose(&mut pile, &phone, later.handle(), "selected", true).is_err());
    pile.flush()?; pile.close()?;
    let (same, mut pile) = crate::open_local(dir.path())?;
    assert_eq!(same.verifying_key(), phone.verifying_key());
    assert!(listening(&mut pile, &same)?);
    assert_eq!(crate::selected(&mut pile, &same)?, vec![atlas.handle()]);
    assert_eq!(page(&mut pile, &same, 0)?.rows.iter().find(|r| r.root == new.root).unwrap().state, "unselected");
    pile.close()?; Ok(())
}

#[test]
fn conflict_requires_explicit_observed_resolution_and_admission() -> Result<()> {
    let dir = tempfile::tempdir()?; let (phone, mut pile) = crate::open_local(dir.path())?;
    let owner = SigningKey::from_bytes(&[43; 32]);
    let root = pile.collection("conflict", private_policy(owner.verifying_key()))?.handle();
    let config = pile.collection(CONFIG_COLLECTION_NAME, private_policy(phone.verifying_key()))?;
    for value in [true, false] {
        pile.commit(config, &phone, entity! { &genid() @
            selection::sync_collection: root, selection::sync_selected: value,
        })?;
    }
    let row = page(&mut pile, &phone, 0)?.rows.pop().unwrap();
    assert!(row.conflicted && !row.selected && !row.readable);
    assert!(choose(&mut pile, &phone, root, "unset", true).is_err());
    assert!(choose(&mut pile, &phone, root, &row.state, true).is_err());
    assert!(crate::selected(&mut pile, &phone)?.is_empty());
    grant_collection_read(&mut pile, root, &owner, phone.verifying_key())?;
    choose(&mut pile, &phone, root, &row.state, true)?;
    assert_eq!(crate::selected(&mut pile, &phone)?, vec![root]);
    choose(&mut pile, &phone, root, "selected", false)?;
    assert!(crate::selected(&mut pile, &phone)?.is_empty());
    pile.close()?; Ok(())
}

#[test]
fn incomplete_quorum_does_not_offer_or_select() -> Result<()> {
    let dir = tempfile::tempdir()?; let (phone, mut pile) = crate::open_local(dir.path())?;
    let a = SigningKey::from_bytes(&[44; 32]); let b = SigningKey::from_bytes(&[45; 32]);
    let policy = AdmissionPolicy::quorum([a.verifying_key(), b.verifying_key()], 2, None)?;
    let root = pile.collection("quorum", CollectionPolicy::new(policy.clone(), policy))?.handle();
    start_discovery(&mut pile, &phone)?;
    grant_collection_read(&mut pile, root, &a, phone.verifying_key())?;
    assert!(page(&mut pile, &phone, 0)?.rows.is_empty());
    assert!(choose(&mut pile, &phone, root, "unset", true).is_err());
    grant_collection_read(&mut pile, root, &b, phone.verifying_key())?;
    assert_eq!(page(&mut pile, &phone, 0)?.rows.len(), 1);
    assert!(crate::selected(&mut pile, &phone)?.is_empty());
    pile.close()?; Ok(())
}

#[test]
fn metadata_pages_are_bounded_and_do_not_hide_later_roots() -> Result<()> {
    let dir = tempfile::tempdir()?; let (phone, mut pile) = crate::open_local(dir.path())?;
    let owner = SigningKey::from_bytes(&[46; 32]);
    for i in 0..PAGE_SIZE + 2 {
        let root = pile.collection(&format!("collection {i}"), private_policy(owner.verifying_key()))?.handle();
        grant_collection_read(&mut pile, root, &owner, phone.verifying_key())?;
    }
    let first = page(&mut pile, &phone, 0)?; let second = page(&mut pile, &phone, PAGE_SIZE)?;
    assert_eq!(first.total, PAGE_SIZE + 2); assert_eq!(first.rows.len(), PAGE_SIZE); assert!(first.has_more);
    assert_eq!(second.rows.len(), 2); assert!(!second.has_more);
    assert!(first.rows.iter().all(|a| second.rows.iter().all(|b| a.root != b.root)));
    assert!(crate::selected(&mut pile, &phone)?.is_empty());
    pile.close()?; Ok(())
}

#[test]
fn prefix_choices_distinguish_unseen_config_missing_payload_and_resident_corruption() -> Result<()> {
    use triblespace_core::{collection::CollectionRecord, repo::pile::{PileRecords, PileRecordContent}};
    let dir = tempfile::tempdir()?;
    let (phone, mut writer) = crate::open_local(dir.path())?;
    let root = writer.collection("prefix choices", private_policy(phone.verifying_key()))?;
    writer.commit(root, &phone, entity! { metadata::name: "known" })?;
    let config = writer.collection(CONFIG_COLLECTION_NAME, private_policy(phone.verifying_key()))?;
    let selected = selection::write_sync_selection(&mut writer, config, &phone, root.handle(), true)?;
    selection::write_sync_selection(&mut writer, config, &phone, root.handle(), false)?;
    writer.close()?;
    let file = dir.path().join("replica.pile");
    let original = std::fs::read(&file)?;
    let records = PileRecords::open(&file)?.collect::<Result<Vec<_>, _>>()?;
    let payload = records.iter().find(|record| matches!(record.content,
        PileRecordContent::Blob { hash, .. } if hash.raw == selected.data().raw)).unwrap();
    let data_offset = match payload.content { PileRecordContent::Blob { data_offset, .. } => data_offset, _ => unreachable!() };
    for corrupt in [false, true] {
        let mut delayed = original[payload.offset..payload.offset + payload.len].to_vec();
        if corrupt { delayed[data_offset - payload.offset] ^= 1; }
        // Append-only merges can place the assertion before its exact payload.
        let mut reordered = Vec::new();
        for record in &records {
            if record.offset == payload.offset { continue; }
            reordered.extend_from_slice(&original[record.offset..record.offset + record.len]);
            if matches!(record.content, PileRecordContent::Collection { record: CollectionRecord::Commit(commit) } if commit == selected) {
                reordered.extend_from_slice(&delayed);
            }
        }
        let path = dir.path().join(if corrupt { "corrupt-config.pile" } else { "ordered-prefix.pile" });
        std::fs::write(&path, &reordered)?;
        let mut reader = Pile::open_as(&path, phone.verifying_key())?;
        let mut states = Vec::new();
        let mut failed = false;
        let mut cached = None;
        loop {
            let (snapshot, complete) = reader.snapshot_step(1)?;
            let page = if complete { project(&snapshot, phone.verifying_key(), 0, PAGE_SIZE) }
                else { project_prefix(&snapshot, phone.verifying_key(), 0, PAGE_SIZE) };
            if !complete {
                let reused = PrefixPage::observe(&mut cached, &snapshot, phone.verifying_key(), 0);
                match (&page, reused) {
                    (Ok(expected), Ok(actual)) => assert_eq!(serde_json::to_value(expected)?, serde_json::to_value(actual)?),
                    (Err(_), Err(_)) => {}
                    _ => panic!("cached catalog disagreed with independent projection at byte {}", snapshot.prefix_len()),
                }
            }
            match page {
                Ok(page) => {
                    if let Some(row) = page.rows.iter().find(|row| row.root == hex::encode(root.handle().raw)) {
                        states.push(row.state.clone());
                    }
                }
                Err(_) => { failed = true; break; }
            }
            if complete { break; }
        }
        reader.close()?;
        if corrupt {
            assert!(failed, "resident corrupt config must not become an empty or merely unknown choice");
        } else {
            assert!(!failed);
            for state in ["unknown", "unset", "selected", "unselected"] {
                assert!(states.iter().any(|seen| seen == state), "missing observed state {state}: {states:?}");
            }
            let selected_at = states.iter().position(|state| state == "selected").unwrap();
            assert!(states[..selected_at].iter().rev().any(|state| state == "unknown"));
            assert_eq!(states.last().unwrap(), "unselected");
        }
        assert_eq!(std::fs::read(path)?, reordered);
    }
    Ok(())
}

#[test]
fn native_count_failure_is_local_and_prefix_zero_is_a_known_count() {
    use triblespace_core::collection::record_counts::DirectCounts;
    let failed = count_fields(RecordCountStatus::Failed("synthetic root failure".into()));
    let ready = count_fields(RecordCountStatus::Ready {
        counts: DirectCounts { records: 0, direct_present: 0 }, replay_complete: false,
    });
    assert!(failed.0.is_none());
    assert_eq!(failed.1.as_deref(), Some("synthetic root failure"));
    assert_eq!(ready, (Some(crate::storage::Counts { records: 0, direct_present: 0 }), None, Some(false)));
    assert_eq!(count_fields(RecordCountStatus::Untracked { replay_complete: true }), (None, None, None));
}
#[test]
fn unchanged_catalog_reuses_metadata_but_a_new_root_or_reader_requires_projection() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let (signer, mut pile) = crate::open_local(dir.path())?;
    let reader = signer.verifying_key();
    let first = pile.collection("first", private_policy(reader))?;
    pile.commit(first, &signer, entity! { metadata::name: "first row" })?;
    let mut observed = None;
    let before = pile.snapshot()?;
    let page = PrefixPage::observe(&mut observed, &before, reader, 0)?;
    assert_eq!(page.rows.len(), 1);
    pile.put::<blobencodings::UTF8String, _>("unrelated payload")?;
    let unrelated = pile.snapshot()?;
    let state = observed.as_ref().unwrap();
    assert!(state.metadata_current(&unrelated, reader, 0)?);
    assert!(!state.metadata_current(&unrelated, reader, 32)?);
    assert!(!state.metadata_current(&unrelated, SigningKey::from_bytes(&[74; 32]).verifying_key(), 0)?);
    let second = pile.collection("second", private_policy(reader))?;
    pile.commit(second, &signer, entity! { metadata::name: "another row" })?;
    let added = pile.snapshot()?;
    assert!(!state.metadata_current(&added, reader, 0)?);
    assert_eq!(PrefixPage::observe(&mut observed, &added, reader, 0)?.total, 2);
    pile.close()?;
    Ok(())
}
