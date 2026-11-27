//! Synthetic Wiki/browser seed for the acceptance helper only.
use anyhow::Result;
use erlkonig_core::{config::Bootstrap, node_store::NodeStore};
use faculties::{files::Files, storage::Storage, knowledge::{self, RevisionDraft, Knowledge}};
use hifitime::Epoch;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use triblespace::core::collection::CollectionCommit;
use triblespace::core::repo::{BlobStorePut, SnapshotSource, StoreSnapshot, pile::Pile};
use triblespace::prelude::*;

pub fn seed(bootstrap: &Bootstrap, first: Id, full_viewer: bool) -> Result<Value> {
    let signer = triblespace::core::signing_key_file::load_existing(&bootstrap.key)?;
    let public = signer.verifying_key();
    let mut node = NodeStore::open(&bootstrap.pile, signer)?;
    let view = node.config()?;
    let second = node.create_workspace(&view, if full_viewer { "BLUE fixture · partial access" } else { "Design" })?;
    let view = node.config()?;
    let choices = [view.workspace(first)?.unwrap(), view.workspace(second)?.unwrap()];
    node.close()?;

    let mut examples = Vec::new();
    for (index, choice) in choices.into_iter().enumerate() {
        let marker = if index == 0 { "AMBER" } else { "BLUE" };
        let storage = Storage::shared_local(bootstrap.pile.clone(), Some(bootstrap.key.clone()))
            .with_collections(choice.routes.clone())
            .with_expected_signer(public);
        let result = (|| -> Result<Value> {
            let files = Files::with_storage(storage.clone());
            let file = files.add_bytes(
                format!("PUBLIC SYNTHETIC {marker} BOM\npart,quantity\nwasher,4\n").into_bytes().into(),
                "bom.txt", "text/plain", &[],
            )?;
            let page = Knowledge::with_storage(storage.clone());
            let original = page.create(
                &format!("{marker} Design"),
                &format!("= {marker} original design\nPUBLIC SYNTHETIC old revision."),
                &[], false,
            )?;
            let changed = page.edit(
                &format!("{original:x}"),
                Some(&format!("= {marker} revised design\nPUBLIC SYNTHETIC current revision.")),
                None, &[], false,
            )?;
            // Native DAG constructors reproduce the existing fork unit fixture.
            // Sequential knowledge_edit calls would join the frontier, not fork it.
            let (fork_root, left, right) = storage.with_store(|store, signer, runtime| {
                let destination = storage.open_collection_write(
                    store, faculties::schemas::wiki::DEFAULT_SCOPE_ID,
                    signer.verifying_key(), runtime,
                )?;
                let (mut fragment, author) = knowledge::author_record(&signer.verifying_key());
                let instant = Epoch::from_tai_seconds(1.0);
                let authored_at = (instant, instant).try_to_inline()
                    .map_err(|_| anyhow::anyhow!("synthetic timestamp encoding"))?;
                let draft = |title: String, content: String, predecessors: BTreeSet<Id>| RevisionDraft {
                    title, content, predecessors, author, authored_at, tags: BTreeSet::new(),
                };
                let (root_fragment, root) = knowledge::revision_record(draft(
                    format!("{marker} concurrent BOM"), "= Common BOM\nBase.".into(), BTreeSet::new(),
                ))?;
                let (left_fragment, left) = knowledge::revision_record(draft(
                    format!("{marker} concurrent BOM left"), "= Left tip\nUse four washers.".into(), BTreeSet::from([root]),
                ))?;
                let (right_fragment, right) = knowledge::revision_record(draft(
                    format!("{marker} concurrent BOM right"), "= Right tip\nUse six washers.".into(), BTreeSet::from([root]),
                ))?;
                fragment += root_fragment;
                fragment += left_fragment;
                fragment += right_fragment;
                store.commit(destination, signer, fragment)?;
                Ok((root, left, right))
            })?;
            let landing = page.create(
                &format!("{marker} acceptance index"),
                &format!("= {marker} acceptance index\n#link(\"wiki:{original:x}\")[Exact old revision]\n\n#link(\"wiki:entry:{original:x}\")[Current design]\n\n#link(\"wiki:entry:{fork_root:x}\")[Both concurrent BOM tips]\n\n#link(\"files:{file:x}\")[Workspace BOM file]"),
                &[], false,
            )?;
            if full_viewer {
                faculties::relations::Relations::with_storage(storage.clone()).add(
                    faculties::relations::ProfileInput {
                        label: "fixture-operator".into(),
                        display_name: Some("Synthetic fixture author".into()),
                        ..Default::default()
                    }, None, &[],
                )?;
                let goals = faculties::goals::Goals::with_storage(storage.clone());
                for (title, status) in [("Inspect workspace graph", "doing"), ("Check file preview", "todo"), ("Seed synthetic content", "done")] {
                    goals.add(&format!("{marker} {title}"), faculties::goals::AddOptions {
                        status, persona: Some("fixture-operator"), note: Some("PUBLIC SYNTHETIC fixture only"),
                        ..Default::default()
                    })?;
                }
                let mut previous = landing;
                for number in 1..=40 {
                    previous = page.create(
                        &format!("{marker} graph note {number:02}"),
                        &format!("= {marker} graph note {number:02}\nPUBLIC SYNTHETIC graph fixture.\n#link(\"wiki:entry:{previous:x}\")[Previous note]\n#link(\"wiki:entry:{landing:x}\")[Acceptance index]"),
                        &[], false,
                    )?;
                }
            }
            // One exact body arrives later with no accompanying relation edit.
            // This is acceptance-only data; the running app keeps its one owner.
            let missing_body = if full_viewer && index == 0 {
                Some(storage.with_store(|store, signer, runtime| {
                    let destination = storage.open_collection_write(
                        store, faculties::schemas::wiki::DEFAULT_SCOPE_ID,
                        signer.verifying_key(), runtime,
                    )?;
                    let (mut fragment, author) = knowledge::author_record(&signer.verifying_key());
                    let instant = Epoch::from_tai_seconds(2.0);
                    let authored_at = (instant, instant).try_to_inline()
                        .map_err(|_| anyhow::anyhow!("synthetic timestamp encoding"))?;
                    let content = "= AMBER arrived body\nPUBLIC SYNTHETIC payload arrived without Refresh.";
                    let body: Blob<blobencodings::UTF8String> = content.to_blob();
                    let body_handle = body.get_handle();
                    let (native, revision) = knowledge::revision_record(RevisionDraft {
                        title: "AMBER delayed payload".into(), content: content.into(),
                        predecessors: BTreeSet::new(), tags: BTreeSet::new(), author, authored_at,
                    })?;
                    let (_, facts, metafacts, mut blobs) = native.into_parts();
                    let retained = blobs.snapshot()?.iter()
                        .filter(|(handle, _)| handle.raw != body_handle.raw)
                        .collect();
                    fragment += Fragment::rooted_from_parts(revision, facts, metafacts, retained);
                    store.commit(destination, signer, fragment)?;
                    anyhow::ensure!(!store.snapshot()?.contains_blob(body_handle)?, "body must start absent");
                    let segment = bootstrap.pile.with_file_name("pending-viewer-body.pile");
                    // Pile opens existing files; creation belongs to its caller.
                    // Exclusive creation also refuses an existing segment or link.
                    std::fs::File::create_new(&segment)?;
                    let mut pending = Pile::open_as(&segment, signer.verifying_key())?;
                    pending.put::<blobencodings::UTF8String, _>(body)?;
                    anyhow::ensure!(pending.snapshot()?.contains_blob(body_handle)?, "segment body missing");
                    pending.close()?;
                    let mut records = triblespace::core::repo::pile::PileRecords::open(&segment)?;
                    anyhow::ensure!(matches!(records.next().transpose()?.map(|record| record.content),
                        Some(triblespace::core::repo::pile::PileRecordContent::Blob { hash, data_len, .. })
                            if hash.raw == body_handle.raw && data_len == content.len()),
                        "segment must contain the exact delayed body");
                    anyhow::ensure!(records.next().is_none(), "segment must contain only one blob record");
                    anyhow::ensure!(!store.snapshot()?.contains_blob(body_handle)?, "main body must remain absent");
                    Ok(json!({"revision":format!("{revision:x}"), "segment":segment,
                        "body_handle":hex::encode(body_handle.raw), "expected_text":content,
                        "main_body_absent":true, "segment_only_exact_body":true}))
                })?)
            } else { None };
            Ok(json!({"workspace":format!("{:X}", choice.id), "label":choice.label,
                "marker":marker, "file":format!("{file:x}"), "filename":"bom.txt",
                "original":format!("{original:x}"), "changed":format!("{changed:x}"),
                "fork_root":format!("{fork_root:x}"), "frontier":[format!("{left:x}"),format!("{right:x}")],
                "landing":format!("{landing:x}"), "missing_body":missing_body}))
        })();
        let mut example = storage.finish(result)?;
        // Exercise the public snapshot contract in this app graph, without
        // importing the Faculty crate's private-session unit-test internals.
        let pile = bootstrap.pile.clone();
        let key = bootstrap.key.clone();
        let routes = choice.routes.clone();
        let handle = routes[&faculties::schemas::wiki::DEFAULT_SCOPE_ID];
        let (frozen, ui_clone) = std::thread::spawn(move || -> Result<_> {
            let owner = Storage::shared_local(pile, Some(key))
                .with_collections(routes).with_expected_signer(public);
            let frozen = owner.with_store(|store, _, _| Ok(store.snapshot()?))?;
            anyhow::ensure!(frozen.contains_blob(handle)?, "resident Wiki descriptor missing");
            let ui_clone = owner.clone();
            owner.close()?;
            Ok((frozen, ui_clone))
        }).join().map_err(|_| anyhow::anyhow!("snapshot worker panicked"))??;
        anyhow::ensure!(frozen.contains_blob(handle)?, "closed worker lost frozen bytes");
        drop(ui_clone);
        anyhow::ensure!(frozen.contains_blob(handle)?, "last UI clone lost frozen bytes");
        example["frozen_snapshot_after_worker_close"] = json!(true);
        if full_viewer && index == 1 {
            // Keep readable Wiki/Files/Compass while exact Messages READ is denied
            // and Mail is unconfigured. Never select a readable default instead.
            let signer = triblespace::core::signing_key_file::load_existing(&bootstrap.key)?;
            let storage = Storage::shared_local(bootstrap.pile.clone(), Some(bootstrap.key.clone()))
                .with_expected_signer(public);
            let denied = storage.with_store(|store, signer, _| {
                use triblespace::core::collection::{AdmissionPolicy, CollectionPolicy};
                let other = ed25519_dalek::SigningKey::from_bytes(&[71; 32]).verifying_key();
                Ok(store.collection("messages", CollectionPolicy::new(
                    AdmissionPolicy::direct(other), AdmissionPolicy::direct(signer.verifying_key()),
                ))?.handle())
            })?;
            storage.close()?;
            let mut node = NodeStore::open(&bootstrap.pile, signer)?;
            let view = node.config()?;
            let mut partial = view.workspace(choice.id)?.unwrap();
            partial.routes.remove(&faculties::schemas::mail::DEFAULT_SCOPE_ID);
            partial.routes.insert(faculties::schemas::message::DEFAULT_SCOPE_ID, denied);
            let revision = node.put_workspace(&view, &partial)?;
            node.close()?;
            example["workspace"] = json!(format!("{revision:X}"));
            example["unconfigured_role"] = json!("Mail");
            example["denied_role"] = json!("Messages");
        }
        examples.push(example);
    }
    let not_seeded = if full_viewer { vec!["configured Files descriptor/frontier absent"] }
        else { vec!["missing body bytes", "configured Files descriptor/frontier absent"] };
    Ok(json!({"examples":examples, "not_seeded":not_seeded}))
}
