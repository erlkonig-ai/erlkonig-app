//! Synthetic late-grant discovery and explicit choices over loopback QUIC.
use crate::{collections, proof};
use anyhow::{Context, Result, ensure};
use ed25519_dalek::SigningKey;
use iroh::address_lookup::{EndpointInfo, MemoryLookup};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use triblespace_core::{
    blob::{
        Blob,
        encodings::{simplearchive::SimpleArchive, utf8string::UTF8String},
    },
    collection::{
        AdmissionPolicy, Collection, CollectionHandle, CollectionPolicy, CollectionRead,
        CollectionStoreExt, grant_collection_read,
    },
    metadata,
    prelude::*,
    repo::StorageClose,
};
use triblespace_net::{peer::Peer, reconcile::ReplicationMode};

fn payload() -> String {
    "Synthetic explicitly selected replica.\n".repeat(64 * 1024)
}

async fn drive(
    source: &mut Peer<Pile>,
    reader: &mut Peer<Pile>,
    key: &SigningKey,
    expected: CollectionHandle,
    deadline: Instant,
) -> Result<()> {
    loop {
        source.try_refresh()?;
        reader.try_refresh()?;
        let _ = tokio::time::timeout(Duration::from_millis(100), source.reconcile()).await;
        let _ = tokio::time::timeout(Duration::from_millis(100), reader.reconcile()).await;
        let available = collections::page(&mut reader.store(), key, 0)?;
        if let Some(row) = available.rows.iter().find(|r| r.root == hex::encode(expected.raw) && r.readable && r.state == "unset") {
            ensure!(!crate::selected(&mut reader.store(), key)?.contains(&expected), "grant auto-selected a root");
            ensure!(local_payload(&reader.store().snapshot()?, expected).is_err(), "unselected payload arrived");
            // Explicit fixture input chooses this exact root, as the UI command does.
            collections::choose(&mut reader.store(), key, expected, &row.state, true)?;
        }
        let selected = crate::selected(&mut reader.store(), key)?;
        reader.set_replication(ReplicationMode::Full, selected.iter().copied());
        if !selected.is_empty() {
            reader.activate_collections(selected.iter().copied());
        }
        reader.try_refresh()?;
        let snapshot = reader.store().snapshot()?;
        if selected.contains(&expected) && local_payload(&snapshot, expected).is_ok() {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "late-grant Full transfer timed out"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
fn local_payload(
    snapshot: &triblespace_core::repo::pile::PileSnapshot,
    root: CollectionHandle,
) -> Result<usize> {
    let collection = Collection::<SimpleArchive>::open(snapshot, root)?;
    let facts: TribleSet = collection.read(snapshot)?;
    let handle = find!(handle: Inline<inlineencodings::Handle<UTF8String>>,
        pattern!(&facts, [{ _?entity @ metadata::description: ?handle }]))
    .next()
    .context("native collection description")?;
    let value: Blob<UTF8String> = snapshot.get(handle)?;
    ensure!(
        std::str::from_utf8(&value.bytes)? == payload(),
        "referenced payload differs"
    );
    Ok(value.bytes.len())
}
fn data(
    pile: &mut Pile,
    key: &SigningKey,
    name: &str,
    policy: &CollectionPolicy,
) -> Result<CollectionHandle> {
    let collection = pile.collection(name, policy.clone())?;
    pile.commit(
        collection,
        key,
        entity! { metadata::name: name, metadata::description: payload().as_str() },
    )?;
    proof::select(pile, key, collection.handle())?;
    Ok(collection.handle())
}

pub fn run(root: &Path) -> Result<serde_json::Value> {
    ensure!(
        root.is_absolute() && root.is_dir() && std::fs::read_dir(root)?.next().is_none(),
        "provide a new empty absolute fixture directory"
    );
    let (owner, mut source) = crate::open_local(&root.join("source"))?;
    let (phone, mut reader) = crate::open_local(&root.join("reader"))?;
    let policy = CollectionPolicy::new(
        AdmissionPolicy::direct(owner.verifying_key()),
        AdmissionPolicy::direct(owner.verifying_key()),
    );
    let first = data(&mut source, &owner, "first", &policy)?;
    ensure!(
        reader.snapshot()?.collections()?.is_empty(),
        "reader must start empty"
    );
    collections::start_discovery(&mut reader, &phone)?;
    ensure!(
        crate::selected(&mut reader, &phone)?.is_empty(),
        "Discovery is not selection"
    );
    ensure!(
        !reader.snapshot()?.contains_blob(first)?,
        "reader must not preload descriptors"
    );
    source.flush()?;
    reader.flush()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let second = runtime.block_on(async {
        let source_endpoint = proof::endpoint(&owner).await?;
        let reader_endpoint = proof::endpoint(&phone).await?;
        // Only the SOURCE knows the receiver's synthetic address. The empty receiver
        // has no configured peer/descriptor/grant and must be reached by peering.
        source_endpoint
            .address_lookup()?
            .add(MemoryLookup::from_endpoint_info([EndpointInfo::from(
                iroh_base::EndpointAddr::from_parts(
                    reader_endpoint.id(),
                    reader_endpoint
                        .bound_sockets()
                        .into_iter()
                        .map(iroh_base::TransportAddr::Ip),
                ),
            )]));
        let (mut source, source_task) = proof::peer(source_endpoint.clone(), source).await;
        let (mut reader, reader_task) = proof::peer(reader_endpoint.clone(), reader).await;
        let result = async {
            source.set_replication(ReplicationMode::Full, [first]);
            source.activate_collection(first);
            reader.try_refresh()?;
            source.try_refresh()?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            ensure!(crate::selected(&mut reader.store(), &phone)?.is_empty());
            // Late grant: do not seed authority or a selection at the receiver.
            grant_collection_read(&mut *source.store(), first, &owner, phone.verifying_key())?;
            source.flush()?;
            let deadline = Instant::now() + Duration::from_secs(60);
            drive(&mut source, &mut reader, &phone, first, deadline).await?;
            let second = data(&mut source.store(), &owner, "later", &policy)?;
            grant_collection_read(&mut *source.store(), second, &owner, phone.verifying_key())?;
            source.set_replication(ReplicationMode::Full, [first, second]);
            source.activate_collection(second);
            drive(&mut source, &mut reader, &phone, second, deadline).await?;
            Ok::<_, anyhow::Error>(second)
        }
        .await;
        source_task.abort();
        reader_task.abort();
        let _ = source_task.await;
        let _ = reader_task.await;
        source_endpoint.close().await;
        reader_endpoint.close().await;
        let source_saved = source.flush();
        let reader_saved = reader.flush();
        let source_closed = source.close();
        let reader_closed = reader.close();
        source_saved?;
        reader_saved?;
        source_closed?;
        reader_closed?;
        result
    })?;
    drop(runtime);
    let report = serde_json::json!({
        "reader_node_id": hex::encode(phone.verifying_key().as_bytes()),
        "source_public_key": hex::encode(owner.verifying_key().as_bytes()),
        "collections": [hex::encode(first.raw), hex::encode(second.raw)],
        "empty_receiver_bootstrap": true, "reader_peer_seed": false,
        "late_grant": true, "each_root_explicitly_selected": true, "grant_did_not_select_or_transfer_payload": true,
        "transport": "synthetic loopback QUIC; source-only explicit address hint",
    });
    std::fs::write(
        root.join("collections-proof-receipt.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    // No peer/runtime exists during the persistent source-off read.
    let verified = reopen(root)?;
    std::fs::write(
        root.join("collections-proof-receipt.json"),
        serde_json::to_vec_pretty(&verified)?,
    )?;
    Ok(verified)
}

pub fn reopen(root: &Path) -> Result<serde_json::Value> {
    ensure!(
        root.join("reader/node.key").is_file() && root.join("reader/replica.pile").is_file(),
        "existing joined replica required; reopen must not bootstrap a new identity"
    );
    let mut report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("collections-proof-receipt.json"))?)?;
    let (phone, mut pile) = crate::open_local(&root.join("reader"))?;
    ensure!(
        hex::encode(phone.verifying_key().as_bytes())
            == report["reader_node_id"].as_str().unwrap_or(""),
        "phone identity changed"
    );
    ensure!(
        collections::listening(&mut pile, &phone)?,
        "standing intent lost"
    );
    let mut expected = report["collections"]
        .as_array()
        .context("receipt collections")?
        .iter()
        .map(|value| {
            let bytes: [u8; 32] = hex::decode(value.as_str().context("collection handle")?)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid receipt handle"))?;
            Ok(CollectionHandle::new(bytes))
        })
        .collect::<Result<Vec<_>>>()?;
    expected.sort_unstable();
    ensure!(
        crate::selected(&mut pile, &phone)? == expected,
        "native selection lost"
    );
    let snapshot = pile.snapshot()?;
    let mut bytes = 0;
    for root in expected {
        bytes += local_payload(&snapshot, root)?;
    }
    drop(snapshot);
    pile.close()?;
    report["source_off_read"] = true.into();
    report["verified_payload_bytes"] = bytes.into();
    report["standing_intent_reopened"] = true.into();
    Ok(report)
}
pub fn run_owned(root: std::path::PathBuf, reopening: bool) -> Result<serde_json::Value> {
    std::thread::Builder::new()
        .name("ios-collections-proof".into())
        .stack_size(crate::WORKER_STACK_BYTES)
        .spawn(move || if reopening { reopen(&root) } else { run(&root) })?
        .join()
        .map_err(|_| anyhow::anyhow!("join proof thread panicked"))?
}
