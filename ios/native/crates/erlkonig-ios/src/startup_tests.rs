use super::*;
use triblespace_core::{
    collection::{CollectionRead, CollectionRecordSelector, CollectionStoreExt,
        RecordViewChanges, grant_collection_read, private_policy},
    metadata, prelude::*, repo::pile::PileRecords,
};

fn fixture() -> (tempfile::TempDir, SigningKey, [CollectionHandle; 2], usize) {
    let dir = tempfile::tempdir().unwrap();
    let (key, mut pile) = open_local(dir.path()).unwrap();
    let early = pile.collection("early local name", private_policy(key.verifying_key())).unwrap();
    pile.commit(early, &key, entity! { metadata::name: "early" }).unwrap();
    let owner = SigningKey::from_bytes(&[61; 32]);
    let late = pile.collection("late grant", private_policy(owner.verifying_key())).unwrap();
    pile.commit(late, &owner, entity! { metadata::name: "late" }).unwrap();
    // Prefixes must not turn an absent/later configuration into an off choice.
    let config = pile.collection(selection::CONFIG_COLLECTION_NAME, private_policy(key.verifying_key())).unwrap();
    selection::write_sync_selection(&mut pile, config, &key, early.handle(), true).unwrap();
    grant_collection_read(&mut pile, late.handle(), &owner, key.verifying_key()).unwrap();
    selection::write_sync_selection(&mut pile, config, &key, early.handle(), false).unwrap();
    pile.close().unwrap();
    let steps = PileRecords::open(&dir.path().join("replica.pile")).unwrap().count() + 2;
    (dir, key, [early.handle(), late.handle()], steps)
}

fn run_script(root: PathBuf, commands: mpsc::Receiver<Command>, status: Arc<Mutex<Status>>) {
    let (finished, completion) = mpsc::channel();
    let worker = std::thread::Builder::new().stack_size(WORKER_STACK_BYTES).spawn(move || {
        worker_with_cleanup(root, commands, status, pause);
        let _ = finished.send(());
    }).unwrap();
    // Every caller queued Stop already; all result assertions happen after
    // settlement. The retained phase runner also bounds the process group.
    let done = completion.recv_timeout(Duration::from_secs(30));
    worker.join().unwrap();
    done.expect("startup worker did not settle");
}

#[test]
fn local_replay_meter_measures_complete_physical_records() {
    let (dir, key, _, _) = fixture();
    let path = dir.path().join("replica.pile");
    let expected = PileRecords::open(&path).unwrap().count() as u64;
    let mut pile = Pile::open_as(&path, key.verifying_key()).unwrap();
    assert_eq!(pile.replayed_records(), 0);
    assert!(!pile.snapshot_step(1).unwrap().1);
    assert_eq!(pile.replayed_records(), 1);
    assert!(pile.snapshot_step(u64::MAX).unwrap().1);
    assert_eq!(pile.replayed_records(), expected);
    assert!(pile.snapshot_step(1).unwrap().1);
    assert_eq!(pile.replayed_records(), expected);
    pile.close().unwrap();
}

#[test]
fn local_replay_rate_has_a_monotonic_window_and_expires() {
    let start = Instant::now() - Duration::from_secs(20);
    let at = |millis| start + Duration::from_millis(millis);
    let mut meter = ReplayMeter::default();
    assert!(meter.observe(at(0), 40).is_none());
    assert!(meter.observe(at(250), 100).is_none());
    let measured = meter.observe(at(1500), 100).unwrap();
    assert_eq!(measured.records_per_second, 40);
    assert_eq!(meter.observe(at(1550), 100), Some(measured));
    assert_eq!(meter.observe(at(3000), 100).unwrap().records_per_second, 0);
    assert_eq!(serde_json::to_value(measured).unwrap(), serde_json::Value::Null);
    assert!(meter.observe(at(3100), 2).is_none());
    assert!(ReplayMeter::default().observe(at(9900), 200).is_none());
}

#[test]
fn opening_worker_publishes_prefixes_and_moves_one_store_after_complete_local_page() {
    let (dir, key, roots, steps) = fixture();
    let file = dir.path().join("replica.pile");
    let original = fs::read(&file).unwrap();
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    let (observations, observed) = mpsc::channel();
    let origin = Arc::new(Mutex::new(None));
    let first_origin = origin.clone();
    let first_observation = observations.clone();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, peer, status, budget| {
        *budget = 1; // One complete native record per turn, no wall-clock race.
        let pile = pile.unwrap();
        *first_origin.lock().unwrap() = Some(pile.snapshot_replayed());
        first_observation.send(("identity", peer, pile.replayed_length(), serde_json::to_value(&*status.lock().unwrap()).unwrap())).unwrap();
    }))).unwrap();
    let before_origin = origin.clone();
    let before_observation = observations.clone();
    sender.send(Command::BeforeForeground(Box::new(move |pile, status| {
        let changes = pile.snapshot_replayed().select_record_view_changes(
            before_origin.lock().unwrap().as_ref().unwrap(),
            &std::collections::BTreeSet::from([CollectionRecordSelector::Foundations(roots[0])]),
        ).unwrap();
        before_observation.send(("before peer", matches!(changes, RecordViewChanges::Rebuild),
            pile.replayed_length(), serde_json::to_value(&*status.lock().unwrap()).unwrap())).unwrap();
    }))).unwrap();
    // A direct caller cannot mutate incomplete local state even if it bypasses UI.
    sender.send(Command::Choose(roots[0], "unset".into(), false)).unwrap();
    sender.send(Command::Discover).unwrap();
    sender.send(Command::Active(true, 0)).unwrap();
    for _ in 0..steps {
        sender.send(Command::Page(0)).unwrap();
        let observations = observations.clone();
        sender.send(Command::OpeningCheckpoint(Box::new(move |pile, peer, status, _| {
            observations.send(("page", peer, pile.map_or(0, |p| p.replayed_length()),
                serde_json::to_value(&*status.lock().unwrap()).unwrap())).unwrap();
        }))).unwrap();
    }
    let transferred_origin = origin.clone();
    let transferred_observation = observations.clone();
    sender.send(Command::StorageCheckpoint(Box::new(move |peer, _, status| {
        let pile = peer.store();
        let changes = pile.snapshot_replayed().select_record_view_changes(
            transferred_origin.lock().unwrap().as_ref().unwrap(),
            &std::collections::BTreeSet::from([CollectionRecordSelector::Foundations(roots[0])]),
        ).unwrap();
        transferred_observation.send(("after peer", matches!(changes, RecordViewChanges::Rebuild),
            pile.replayed_length(), serde_json::to_value(&*status.lock().unwrap()).unwrap())).unwrap();
    }))).unwrap();
    sender.send(Command::Stop).unwrap();
    drop(sender); drop(observations);
    run_script(dir.path().to_owned(), receiver, status);
    let samples: Vec<_> = observed.try_iter().collect();
    let first = &samples[0];
    assert_eq!(first.0, "identity");
    assert!(!first.1 && first.2 == 0);
    assert_eq!(first.3["node_id"], hex::encode(key.verifying_key().as_bytes()));
    assert_eq!(first.3["replay_complete"], false);
    let prefixes: Vec<_> = samples.iter().filter(|s| s.0 == "page" && s.3["replay_complete"] == false).collect();
    assert!(prefixes.iter().any(|s| !s.3["collections"]["rows"].as_array().unwrap().is_empty()), "no useful prefix reached shared UI status");
    for sample in prefixes {
        assert!(!sample.1);
        assert_eq!(sample.3["network_started"], false);
        assert_eq!(sample.3["storage"], serde_json::json!([]));
        assert!(sample.3["composition"].is_null());
        let progress = &sample.3["replay_progress"];
        assert_eq!(progress["prefix_bytes"], sample.2);
        assert_eq!(progress["observed_bytes"], original.len() as u64);
        assert_eq!(progress["activity_serial"], 0);
        assert!(sample.3["collections"]["rows"].as_array().unwrap().iter()
            .all(|r| ["unknown", "unset", "selected", "unselected"].contains(&r["state"].as_str().unwrap())));
    }
    for name in ["before peer", "after peer"] {
        let sample = samples.iter().find(|s| s.0 == name).unwrap();
        assert!(!sample.1, "store was reopened across {name}");
        assert_eq!(sample.2, original.len() as u64);
        assert_eq!(sample.3["replay_complete"], true);
        assert_eq!(sample.3["collections"]["total"], 2);
        assert_eq!(sample.3["network_started"], false);
        assert_eq!(sample.3["action_serial"], 1);
        assert!(sample.3["action_error"].as_str().unwrap().contains("still loading"));
        let early = sample.3["collections"]["rows"].as_array().unwrap().iter()
            .find(|r| r["root"] == hex::encode(roots[0].raw)).unwrap();
        assert_eq!(early["state"], "unselected");
    }
    assert_eq!(fs::read(file).unwrap(), original, "replay/blocked mutations wrote the pile");
}

#[test]
fn opening_pause_retains_partial_reader_and_stop_closes_without_replacing_identity() {
    for pause in [false, true] {
        let (dir, key, _, _) = fixture();
        let original = fs::read(dir.path().join("replica.pile")).unwrap();
        let status = Arc::new(Mutex::new(Status::default()));
        let (sender, receiver) = mpsc::channel();
        sender.send(Command::OpeningCheckpoint(Box::new(|_, _, _, budget| *budget = 1))).unwrap();
        sender.send(Command::Active(true, 0)).unwrap();
        if pause { sender.send(Command::Active(false, 0)).unwrap(); }
        let (state, states) = mpsc::channel();
        sender.send(Command::OpeningCheckpoint(Box::new(move |pile, peer, status, _| {
            state.send((pile.map(|p| p.replayed_length()), peer,
                serde_json::to_value(&*status.lock().unwrap()).unwrap())).unwrap();
        }))).unwrap();
        sender.send(Command::Stop).unwrap(); drop(sender);
        run_script(dir.path().to_owned(), receiver, status);
        let (position, peer, value) = states.try_recv().unwrap();
        assert!(!peer);
        assert_eq!(value["replay_complete"], false);
        assert_eq!(value["network_started"], false);
        assert!(position.unwrap() > 0 && position.unwrap() < original.len() as u64);
        if pause { assert_eq!(value["message"], "Replica paused"); }
        let (reopened, pile) = open_local(dir.path()).unwrap();
        assert_eq!(reopened.verifying_key(), key.verifying_key());
        pile.close().unwrap();
        assert_eq!(fs::read(dir.path().join("replica.pile")).unwrap(), original);
    }
}

#[test]
fn opening_replay_error_keeps_identity_and_never_truncates_the_file() {
    use std::io::Write;
    let (dir, key, _, steps) = fixture();
    let path = dir.path().join("replica.pile");
    OpenOptions::new().append(true).open(&path).unwrap().write_all(&[0xAA; 256]).unwrap();
    let original = fs::read(&path).unwrap();
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    sender.send(Command::OpeningCheckpoint(Box::new(|_, _, _, budget| *budget = 1))).unwrap();
    sender.send(Command::Active(true, 0)).unwrap();
    for _ in 0..steps { sender.send(Command::Page(0)).unwrap(); }
    sender.send(Command::Stop).unwrap(); drop(sender);
    run_script(dir.path().to_owned(), receiver, status.clone());
    let value = status.lock().unwrap();
    assert_eq!(value.node_id, hex::encode(key.verifying_key().as_bytes()));
    assert!(!value.replay_complete && !value.network_started);
    assert!(value.error.as_ref().unwrap().contains("replica replay failed"));
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn offline_foreground_replays_resumes_and_pages_without_starting_a_peer() {
    let (dir, _, _, _) = fixture();
    let (key, mut pile) = open_local(dir.path()).unwrap();
    for i in 0..33 {
        let root = pile.collection(&format!("offline page {i}"), private_policy(key.verifying_key())).unwrap();
        pile.commit(root, &key, entity! { metadata::name: "offline page" }).unwrap();
    }
    pile.close().unwrap();
    let original = fs::read(dir.path().join("replica.pile")).unwrap();
    let file_bytes = original.len() as u64;
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    let (observations, observed) = mpsc::channel();
    let checkpoint = || {
        let observations = observations.clone();
        Command::OpeningCheckpoint(Box::new(move |pile, peer, status, _| {
            observations.send((pile.is_some(), peer, serde_json::to_value(&*status.lock().unwrap()).unwrap())).unwrap();
        }))
    };
    sender.send(Command::OpeningCheckpoint(Box::new(|_, _, _, budget| *budget = 1))).unwrap();
    sender.send(Command::LocalActive(1)).unwrap();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, peer, status, budget| {
        let replayed = pile.expect("local replay owner").replayed_length();
        assert!(replayed > 0 && replayed < file_bytes);
        assert!(!peer && !status.lock().unwrap().replay_complete);
        *budget = file_bytes;
    }))).unwrap();
    sender.send(Command::Page(0)).unwrap();
    sender.send(checkpoint()).unwrap();
    sender.send(Command::Active(false, 2)).unwrap();
    sender.send(checkpoint()).unwrap();
    sender.send(Command::LocalActive(3)).unwrap();
    sender.send(checkpoint()).unwrap();
    // This request is after EOF: the offline reader has no Peer to project it.
    sender.send(Command::Page(32)).unwrap();
    sender.send(checkpoint()).unwrap();
    sender.send(Command::Stop).unwrap(); drop(sender); drop(observations);
    run_script(dir.path().to_owned(), receiver, status);
    let samples: Vec<_> = observed.try_iter().collect();
    assert_eq!(samples.len(), 4);
    assert_eq!(samples[0].2["activity_serial"], 1);
    assert_eq!(samples[0].2["replay_complete"], true);
    assert!(samples[1].0 && !samples[1].1, "pause dropped the local owner");
    assert_eq!(samples[1].2["activity_serial"], 2);
    assert!(samples[2].0 && !samples[2].1);
    assert_eq!(samples[2].2["activity_serial"], 3, "unchanged retained reader needed another replay");
    assert_eq!(samples[2].2["replay_complete"], true);
    assert!(samples[3].0 && !samples[3].1, "offline reader must retain its store without a peer");
    assert_eq!(samples[3].2["activity_serial"], 3);
    assert_eq!(samples[3].2["replay_complete"], true);
    assert_eq!(samples[3].2["collections"]["total"], 35);
    assert_eq!(samples[3].2["collections"]["offset"], 32);
    assert_eq!(samples[3].2["collections"]["rows"].as_array().unwrap().len(), 3);
    assert!(samples.iter().all(|s| s.2["replay_progress"]["records_per_second"].is_null()));
    assert!(samples.iter().all(|s| s.2["network_started"] == false));
    assert_eq!(fs::read(dir.path().join("replica.pile")).unwrap(), original);
}

#[test]
fn activity_serial_acknowledges_only_completed_cleanup_even_when_not_running() {
    let (dir, _, _, _) = fixture();
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    sender.send(Command::OpeningCheckpoint(Box::new(|_, _, _, budget| *budget = 1))).unwrap();
    sender.send(Command::LocalActive(0)).unwrap();
    sender.send(Command::Active(false, 1)).unwrap();
    sender.send(Command::Active(false, 2)).unwrap();
    sender.send(Command::Stop).unwrap(); drop(sender);
    let mut before_cleanup = Vec::new();
    worker_with_cleanup(dir.path().to_owned(), receiver, status.clone(), |peer, local| {
        let value = status.lock().unwrap();
        before_cleanup.push((value.activity_serial, value.ready, value.running, value.replay_complete));
        drop(value);
        pause(peer, local)
    });
    assert_eq!(before_cleanup[0], (0, true, false, false));
    // A second pause of the already-checkpointed reader acknowledges immediately.
    // Stop performs final close, not another injected pause.
    assert_eq!(before_cleanup.len(), 1);
    assert_eq!(status.lock().unwrap().activity_serial, 2);
}

#[test]
fn actual_replay_publications_keep_useful_counts_during_continuous_append() {
    use triblespace_core::collection::{CollectionCommit, CollectionRecord, CollectionStore};
    let dir = tempfile::tempdir().unwrap();
    let (key, mut writer) = open_local(dir.path()).unwrap();
    let root = writer.collection("growing prefix", private_policy(key.verifying_key())).unwrap().handle();
    let meta = writer.put::<blobencodings::SimpleArchive, _>(TribleSet::new()).unwrap();
    let data = writer.put::<blobencodings::UTF8String, _>("initial").unwrap();
    writer.insert(CollectionRecord::Commit(CollectionCommit::sign(&key, root, *data.as_transmute(), meta))).unwrap();
    writer.flush().unwrap();
    let writer = Arc::new(Mutex::new(Some(writer)));
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    let (observations, observed) = mpsc::channel();
    sender.send(Command::OpeningCheckpoint(Box::new(|_, _, _, budget| *budget = 1))).unwrap();
    sender.send(Command::LocalActive(1)).unwrap();
    for index in 0..48 {
        let writer = writer.clone(); let key = key.clone();
        sender.send(Command::OpeningCheckpoint(Box::new(move |_, _, _, _| {
            let mut guard = writer.lock().unwrap(); let writer = guard.as_mut().unwrap();
            let data = writer.put::<blobencodings::UTF8String, _>(format!("append {index}")).unwrap();
            writer.insert(CollectionRecord::Commit(CollectionCommit::sign(&key, root, *data.as_transmute(), meta))).unwrap();
            writer.flush().unwrap();
        }))).unwrap();
        sender.send(Command::Page(0)).unwrap();
        let observations = observations.clone();
        sender.send(Command::OpeningCheckpoint(Box::new(move |pile, peer, status, _| {
            assert!(!peer);
            let snapshot = pile.unwrap().snapshot_replayed();
            let counts = snapshot.record_count_status(root);
            observations.send((serde_json::to_value(&*status.lock().unwrap()).unwrap(), snapshot, counts)).unwrap();
        }))).unwrap();
    }
    sender.send(Command::Stop).unwrap(); drop(sender); drop(observations);
    run_script(dir.path().to_owned(), receiver, status);
    writer.lock().unwrap().take().unwrap().close().unwrap();
    let samples: Vec<_> = observed.try_iter().collect();
    assert_eq!(samples.len(), 48);
    let mut published = Vec::new();
    for (status, source, old_counts) in &samples {
        assert!(status["error"].is_null());
        assert_eq!(status["network_started"], false);
        assert_eq!(source.record_count_status(root), *old_counts, "old native snapshot counts advanced with later appends");
        if let Some(row) = status["collections"]["rows"].as_array().unwrap().iter()
            .find(|row| row["root"] == hex::encode(root.raw)) {
            let RecordCountStatus::Ready { counts, .. } = old_counts else { panic!("native prefix count unavailable") };
            assert_eq!(row["counts"]["records"], counts.records);
            assert_eq!(row["counts"]["direct_present"], counts.direct_present);
            assert_eq!(row["counts_replay_complete"], status["replay_complete"]);
            assert_eq!(status["replay_progress"]["prefix_bytes"], source.prefix_len() as u64);
            assert!(status["replay_progress"]["observed_bytes"].as_u64().unwrap() > source.prefix_len() as u64);
            published.push(counts.records);
        }
    }
    assert!(published.len() > 4, "continuous append produced no useful app row publications");
    assert!(published.windows(2).any(|pair| pair[1] > pair[0]), "published counts never advanced during continuous growth");
}

#[test]
fn blob_only_arrival_republishes_actual_peer_rows_and_local_selected_aggregate() {
    use triblespace_core::{blob::IntoBlob, collection::{CollectionCommit, CollectionRecord, CollectionStore}};
    for local_selected in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (key, mut writer) = open_local(dir.path()).unwrap();
        let root = writer.collection("late direct blob", private_policy(key.verifying_key())).unwrap().handle();
        let data = IntoBlob::<blobencodings::SimpleArchive>::to_blob(entity! { metadata::name: "late" }.facts().clone());
        let meta = writer.put::<blobencodings::SimpleArchive, _>(TribleSet::new()).unwrap();
        writer.insert(CollectionRecord::Commit(CollectionCommit::sign(&key, root, *data.get_handle().as_transmute(), meta))).unwrap();
        if local_selected {
            let config = writer.collection(selection::CONFIG_COLLECTION_NAME, private_policy(key.verifying_key())).unwrap();
            selection::write_sync_selection(&mut writer, config, &key, root, true).unwrap();
        }
        writer.flush().unwrap();
        let before = writer.snapshot().unwrap();
        let status = Arc::new(Mutex::new(Status::default()));
        let (sender, receiver) = mpsc::channel();
        let (observations, observed) = mpsc::channel();
        sender.send(if local_selected { Command::LocalActive(1) } else { Command::Active(true, 1) }).unwrap();
        let capture = |observations: mpsc::Sender<serde_json::Value>| {
            Command::OpeningCheckpoint(Box::new(move |_, _, status, _| {
                observations.send(serde_json::to_value(&*status.lock().unwrap()).unwrap()).unwrap();
            }))
        };
        sender.send(capture(observations.clone())).unwrap();
        sender.send(Command::OpeningCheckpoint(Box::new(move |_, _, _, _| {
            writer.put::<blobencodings::SimpleArchive, _>(data).unwrap();
            writer.flush().unwrap(); writer.close().unwrap();
        }))).unwrap();
        if local_selected {
            sender.send(Command::Page(0)).unwrap();
        } else {
            // Changes Storage visibility only; leaves previous snapshot/content
            // masks intact. Actual Peer refresh must notice the blob-only delta.
            sender.send(Command::StorageActive(true)).unwrap();
        }
        sender.send(capture(observations.clone())).unwrap();
        sender.send(Command::Active(false, 2)).unwrap();
        sender.send(if local_selected { Command::LocalActive(3) } else { Command::Active(true, 3) }).unwrap();
        sender.send(capture(observations.clone())).unwrap();
        sender.send(Command::Stop).unwrap(); drop(sender); drop(observations);
        run_script(dir.path().to_owned(), receiver, status);
        let samples: Vec<_> = observed.try_iter().collect();
        assert_eq!(samples.len(), 3);
        for (index, sample) in samples.iter().enumerate() {
            assert!(sample["error"].is_null(), "{sample}");
            assert_eq!(sample["network_started"], false);
            assert_eq!(sample["storage"], serde_json::json!([]), "counts must not require a viewport byte request");
            let row = sample["collections"]["rows"].as_array().unwrap().iter()
                .find(|row| row["root"] == hex::encode(root.raw)).unwrap();
            assert_eq!(row["counts"]["records"], 1);
            assert_eq!(row["counts"]["direct_present"], u64::from(index != 0));
            if local_selected {
                assert_eq!(sample["sync_progress"]["records"], 1);
                assert_eq!(sample["sync_progress"]["direct_present"], u64::from(index != 0));
                assert_eq!(sample["sync_progress"]["unknown_roots"], 0);
            }
        }
        assert!(samples[1]["replay_progress"]["prefix_bytes"].as_u64().unwrap() > samples[0]["replay_progress"]["prefix_bytes"].as_u64().unwrap());
        assert_eq!(samples[2]["activity_serial"], 3);
        let RecordCountStatus::Ready { counts, .. } = before.record_count_status(root) else { panic!("old count unavailable") };
        assert_eq!((counts.records, counts.direct_present), (1, 0));
    }
}

#[test]
fn selected_aggregate_keeps_unknown_roots_unknown_and_checks_overflow() {
    use triblespace_core::collection::record_counts::DirectCounts;
    let mut progress = SyncProgress::default();
    progress.observe(RecordCountStatus::Ready {
        counts: DirectCounts { records: 7, direct_present: 3 }, replay_complete: true,
    });
    progress.observe(RecordCountStatus::Failed("root failure".into()));
    progress.observe(RecordCountStatus::Untracked { replay_complete: true });
    assert_eq!(progress, SyncProgress { records: 7, direct_present: 3, unknown_roots: 2 });
    progress.observe(RecordCountStatus::Ready {
        counts: DirectCounts { records: u64::MAX, direct_present: 0 }, replay_complete: true,
    });
    assert_eq!(progress, SyncProgress { records: 7, direct_present: 3, unknown_roots: 3 });
}

#[test]
fn retained_peer_pause_resume_keeps_native_lineage_and_stop_releases_owner() {
    use triblespace_core::collection::HeldStore;
    let (dir, _, roots, steps) = fixture();
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    let baseline = Arc::new(Mutex::new(None));
    let held = Arc::new(Mutex::new(None));
    sender.send(Command::Active(true, 1)).unwrap();
    for _ in 0..steps { sender.send(Command::Page(0)).unwrap(); }
    let first = baseline.clone();
    let driver = held.clone();
    sender.send(Command::StorageCheckpoint(Box::new(move |peer, _, status| {
        assert!(!status.lock().unwrap().network_started); // Fixture selects nothing.
        *first.lock().unwrap() = Some(peer.store().snapshot_replayed());
        *driver.lock().unwrap() = peer.store().drive_held();
    }))).unwrap();
    for serial in [2, 4, 6] {
        sender.send(Command::Active(false, serial)).unwrap();
        let before = baseline.clone();
        sender.send(Command::OpeningCheckpoint(Box::new(move |pile, peer, status, _| {
            assert!(!peer);
            let pile = pile.expect("pause discarded its native owner");
            let snapshot = pile.snapshot_replayed();
            let changes = snapshot.select_record_view_changes(before.lock().unwrap().as_ref().unwrap(),
                &std::collections::BTreeSet::from([CollectionRecordSelector::Foundations(roots[0])])).unwrap();
            assert!(matches!(changes, RecordViewChanges::Append(ref rows) if rows.is_empty()), "pause replaced the replay lineage");
            let status = status.lock().unwrap();
            assert_eq!(status.activity_serial, serial);
            assert!(status.replay_complete && !status.running && !status.network_started);
            assert_eq!(status.collections.total, 2);
        }))).unwrap();
        sender.send(Command::Active(true, serial + 1)).unwrap();
        let before = baseline.clone();
        sender.send(Command::StorageCheckpoint(Box::new(move |peer, _, status| {
            let snapshot = peer.store().snapshot_replayed();
            let changes = snapshot.select_record_view_changes(before.lock().unwrap().as_ref().unwrap(),
                &std::collections::BTreeSet::from([CollectionRecordSelector::Foundations(roots[0])])).unwrap();
            assert!(matches!(changes, RecordViewChanges::Append(ref rows) if rows.is_empty()), "resume replayed a new owner");
            assert_eq!(status.lock().unwrap().activity_serial, serial + 1);
            assert!(peer.store().drive_held().is_none(), "a second held driver attached to the same store");
        }))).unwrap();
    }
    sender.send(Command::Stop).unwrap(); drop(sender);
    run_script(dir.path().to_owned(), receiver, status.clone());
    assert!(status.lock().unwrap().error.is_none());
    assert!(!held.lock().unwrap().as_mut().expect("held driver witness").run_until_idle(), "Stop kept the native owner alive");
}

#[test]
fn partial_pause_stays_put_then_resume_consumes_only_appended_suffix() {
    let (dir, key, roots, steps) = fixture();
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    let retained = Arc::new(Mutex::new(None));
    let position = Arc::new(Mutex::new(0));
    sender.send(Command::OpeningCheckpoint(Box::new(|_, _, _, budget| *budget = 1))).unwrap();
    sender.send(Command::LocalActive(1)).unwrap();
    sender.send(Command::Active(false, 2)).unwrap();
    let before = retained.clone(); let offset = position.clone();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, _, status, _| {
        let pile = pile.unwrap();
        *before.lock().unwrap() = Some(pile.snapshot_replayed());
        *offset.lock().unwrap() = pile.replayed_length();
        assert!(pile.replayed_length() > 0 && pile.replayed_length() < pile.observed_length().unwrap());
        assert_eq!(status.lock().unwrap().activity_serial, 2);
    }))).unwrap();
    // Real queued commands must not advance a paused partial replay or mutate it.
    sender.send(Command::Page(32)).unwrap();
    sender.send(Command::Choose(roots[0], "unselected".into(), true)).unwrap();
    sender.send(Command::Discover).unwrap();
    sender.send(Command::Active(false, 3)).unwrap();
    let offset = position.clone();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, _, status, _| {
        assert_eq!(pile.unwrap().replayed_length(), *offset.lock().unwrap());
        let status = status.lock().unwrap();
        assert_eq!(status.activity_serial, 3);
        assert_eq!(status.action_serial, 1);
        assert!(!status.replay_complete);
    }))).unwrap();
    sender.send(Command::LocalActive(4)).unwrap();
    let before = retained.clone(); let offset = position.clone();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, _, status, _| {
        let pile = pile.unwrap();
        assert!(pile.replayed_length() > *offset.lock().unwrap());
        let changes = pile.snapshot_replayed().select_record_view_changes(before.lock().unwrap().as_ref().unwrap(),
            &std::collections::BTreeSet::from([CollectionRecordSelector::Foundations(roots[0])])).unwrap();
        assert!(matches!(changes, RecordViewChanges::Append(_)), "partial resume reopened from zero");
        assert_eq!(status.lock().unwrap().activity_serial, 3, "partial resume acknowledged before EOF");
    }))).unwrap();
    for _ in 0..steps { sender.send(Command::Page(0)).unwrap(); }
    sender.send(Command::Active(false, 5)).unwrap();
    let path = dir.path().to_owned(); let before = retained.clone(); let offset = position.clone();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, _, _, _| {
        let pile = pile.unwrap();
        *before.lock().unwrap() = Some(pile.snapshot_replayed());
        *offset.lock().unwrap() = pile.replayed_length();
        let (_, mut writer) = open_local(&path).unwrap();
        let added = writer.collection("appended while suspended", private_policy(key.verifying_key())).unwrap();
        writer.commit(added, &key, entity! { metadata::name: "suffix" }).unwrap();
        writer.close().unwrap();
        assert!(pile.observed_length().unwrap() > pile.replayed_length());
    }))).unwrap();
    sender.send(Command::Page(0)).unwrap();
    let offset = position.clone();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, _, _, _| {
        assert_eq!(pile.unwrap().replayed_length(), *offset.lock().unwrap(), "background page replayed the appended tail");
    }))).unwrap();
    sender.send(Command::LocalActive(6)).unwrap();
    for _ in 0..steps { sender.send(Command::Page(0)).unwrap(); }
    let before = retained.clone(); let offset = position.clone();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, peer, status, _| {
        assert!(!peer);
        let pile = pile.unwrap();
        assert!(pile.replayed_length() > *offset.lock().unwrap());
        assert_eq!(pile.replayed_length(), pile.observed_length().unwrap());
        let changes = pile.snapshot_replayed().select_record_view_changes(before.lock().unwrap().as_ref().unwrap(),
            &std::collections::BTreeSet::from([CollectionRecordSelector::Foundations(roots[0])])).unwrap();
        assert!(matches!(changes, RecordViewChanges::Append(ref rows) if rows.is_empty()), "suffix resume rebuilt preexisting records");
        let status = status.lock().unwrap();
        assert_eq!(status.activity_serial, 6);
        assert_eq!(status.collections.total, 3);
        assert!(status.replay_complete);
    }))).unwrap();
    sender.send(Command::Stop).unwrap(); drop(sender);
    run_script(dir.path().to_owned(), receiver, status.clone());
    assert!(status.lock().unwrap().error.is_none());
}

#[test]
fn queued_inactive_choices_and_discovery_use_retained_owner_and_resume_in_order() {
    let (dir, _, roots, steps) = fixture();
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    let baseline = Arc::new(Mutex::new(None));
    sender.send(Command::LocalActive(1)).unwrap();
    for _ in 0..steps { sender.send(Command::Page(0)).unwrap(); }
    sender.send(Command::Active(false, 2)).unwrap();
    let before = baseline.clone();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, _, _, _| {
        *before.lock().unwrap() = Some(pile.unwrap().snapshot_replayed());
    }))).unwrap();
    sender.send(Command::Choose(roots[0], "unselected".into(), true)).unwrap();
    sender.send(Command::Discover).unwrap();
    sender.send(Command::Choose(roots[0], "selected".into(), false)).unwrap();
    sender.send(Command::LocalActive(3)).unwrap();
    sender.send(Command::Active(false, 4)).unwrap();
    sender.send(Command::LocalActive(5)).unwrap();
    sender.send(Command::OpeningCheckpoint(Box::new(move |pile, peer, status, _| {
        assert!(!peer);
        let changes = pile.unwrap().snapshot_replayed().select_record_view_changes(baseline.lock().unwrap().as_ref().unwrap(),
            &std::collections::BTreeSet::from([CollectionRecordSelector::Foundations(roots[0])])).unwrap();
        assert!(matches!(changes, RecordViewChanges::Append(ref rows) if rows.is_empty()), "choice or discovery reopened the store");
        let status = status.lock().unwrap();
        assert_eq!(status.activity_serial, 5);
        assert_eq!(status.action_serial, 2);
        assert!(status.action_error.is_none());
        assert!(status.listening && status.replay_complete && !status.network_started);
        assert_eq!(status.selected, 0);
        assert!(status.collections.rows.iter().any(|row| row.root == hex::encode(roots[0].raw) && row.state == "unselected"));
    }))).unwrap();
    sender.send(Command::Stop).unwrap(); drop(sender);
    run_script(dir.path().to_owned(), receiver, status.clone());
    assert!(status.lock().unwrap().error.is_none());
    let (key, mut reopened) = open_local(dir.path()).unwrap();
    assert!(collections::listening(&mut reopened, &key).unwrap());
    assert!(selected(&mut reopened, &key).unwrap().is_empty());
    reopened.close().unwrap();
}

#[test]
fn failed_pause_is_not_acknowledged_and_final_cleanup_releases_retained_owner() {
    use triblespace_core::collection::HeldStore;
    let (dir, _, _, steps) = fixture();
    let status = Arc::new(Mutex::new(Status::default()));
    let (sender, receiver) = mpsc::channel();
    sender.send(Command::LocalActive(1)).unwrap();
    for _ in 0..steps { sender.send(Command::Page(0)).unwrap(); }
    sender.send(Command::Active(false, 2)).unwrap();
    sender.send(Command::LocalActive(3)).unwrap();
    drop(sender);
    let mut witness = None;
    worker_with_cleanup(dir.path().to_owned(), receiver, status.clone(), |peer, local| {
        pause(peer, local)?;
        witness = local.as_mut().unwrap().drive_held();
        Err(CleanupFailure { shutdown: None, flush: Some("injected checkpoint failure".into()), close: None }.into())
    });
    assert!(!witness.as_mut().unwrap().run_until_idle(), "fatal cleanup retained its local owner");
    let status = status.lock().unwrap();
    assert_eq!(status.activity_serial, 1, "failed pause or queued activation was acknowledged");
    assert!(status.error.as_ref().unwrap().contains("injected checkpoint failure"));
    assert!(!status.running && status.network.is_none());
    assert!(!status.replay_complete && status.collections.rows.is_empty(), "fatal owner close left live local readiness");
}
