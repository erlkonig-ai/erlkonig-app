//! Synthetic Wiki/browser seed for the acceptance helper only.
use anyhow::Result;
use erlkonig_core::{config::Bootstrap, node_store::NodeStore};
use faculties::{files::Files, storage::Storage, wiki::{self, RevisionDraft, Wiki}};
use hifitime::Epoch;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use triblespace::core::collection::CollectionCommit;
use triblespace::core::repo::StoreSnapshot;
use triblespace::prelude::*;

pub fn seed(bootstrap: &Bootstrap, first: Id) -> Result<Value> {
    let signer = triblespace::core::signing_key_file::load_existing(&bootstrap.key)?;
    let public = signer.verifying_key();
    let mut node = NodeStore::open(&bootstrap.pile, signer)?;
    let view = node.config()?;
    let second = node.create_workspace(&view, "Design")?;
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
            let page = Wiki::with_storage(storage.clone());
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
            // Sequential wiki_edit calls would join the frontier, not fork it.
            let (fork_root, left, right) = storage.with_store(|store, signer, runtime| {
                let destination = storage.open_collection_write(
                    store, faculties::schemas::wiki::DEFAULT_SCOPE_ID,
                    signer.verifying_key(), runtime,
                )?;
                let (mut fragment, author) = wiki::author_record(&signer.verifying_key());
                let instant = Epoch::from_tai_seconds(1.0);
                let authored_at = (instant, instant).try_to_inline()
                    .map_err(|_| anyhow::anyhow!("synthetic timestamp encoding"))?;
                let draft = |title: String, content: String, predecessors: BTreeSet<Id>| RevisionDraft {
                    title, content, predecessors, author, authored_at, tags: BTreeSet::new(),
                };
                let (root_fragment, root) = wiki::revision_record(draft(
                    format!("{marker} concurrent BOM"), "= Common BOM\nBase.".into(), BTreeSet::new(),
                ))?;
                let (left_fragment, left) = wiki::revision_record(draft(
                    format!("{marker} concurrent BOM left"), "= Left tip\nUse four washers.".into(), BTreeSet::from([root]),
                ))?;
                let (right_fragment, right) = wiki::revision_record(draft(
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
            Ok(json!({"workspace":format!("{:X}", choice.id), "label":choice.label,
                "marker":marker, "file":format!("{file:x}"), "filename":"bom.txt",
                "original":format!("{original:x}"), "changed":format!("{changed:x}"),
                "fork_root":format!("{fork_root:x}"), "frontier":[format!("{left:x}"),format!("{right:x}")],
                "landing":format!("{landing:x}")}))
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
        examples.push(example);
    }
    Ok(json!({"examples":examples,
        "not_seeded":["missing body bytes", "configured Files descriptor/frontier absent"]}))
}
