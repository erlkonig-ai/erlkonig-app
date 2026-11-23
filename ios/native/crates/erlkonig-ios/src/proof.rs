//! Opt-in synthetic proof: two persistent piles over real loopback iroh QUIC.
//! No relays, DNS, multicast, private inputs, model, MCP, or ambient identity.
use anyhow::{Context, Result, bail, ensure};
use ed25519_dalek::SigningKey;
use iroh::{
    Endpoint,
    address_lookup::{EndpointInfo, MemoryLookup},
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use triblespace_core::{
    blob::{Blob, encodings::utf8string::UTF8String},
    capability::{CapabilityProof, CapabilityResource},
    collection::{
        AdmissionPolicy, CollectionHandle, CollectionPolicy, CollectionRead, CollectionRecord,
        CollectionStoreExt, private_policy, read_capability,
        selection::{CONFIG_COLLECTION_NAME, write_sync_selection},
    },
    inline::{Inline, encodings::hash::Handle},
    metadata,
    prelude::{entity, find, pattern},
    repo::pile::Pile,
    repo::{
        BlobStoreGet, BlobStoreList, CapabilityProofStore, SnapshotSource, StorageClose,
        StorageFlush,
    },
    trible::TribleSet,
};
use triblespace_net::{
    host::{self, PeerConfig},
    peer::Peer,
    reconcile::ReplicationMode,
};

pub(crate) async fn endpoint(key: &SigningKey) -> Result<Endpoint> {
    Ok(Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(triblespace_net::identity::iroh_secret(key))
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0".parse::<std::net::SocketAddr>()?)?
        .bind()
        .await?)
}
pub(crate) async fn peer(endpoint: Endpoint, pile: Pile) -> (Peer<Pile>, tokio::task::JoinHandle<()>) {
    let (sender, receiver, wiring) = host::wire(endpoint.id());
    let harness = triblespace_net::transport::iroh::bind_with_endpoint(endpoint).await;
    let owner = tokio::spawn(host::run_host(
        harness,
        PeerConfig {
            daemon: None,
            provider_publication_budget: Some(0),
            bind: None,
        },
        wiring,
    ));
    (Peer::with_wiring(pile, sender, receiver), owner)
}
pub(crate) fn select(pile: &mut Pile, key: &SigningKey, collection: CollectionHandle) -> Result<()> {
    let config = pile.collection(CONFIG_COLLECTION_NAME, private_policy(key.verifying_key()))?;
    write_sync_selection(pile, config, key, collection, true)?;
    Ok(())
}

/// The root must be a new, empty, absolute fixture directory. It is retained.
/// Returns only public IDs, handles and counts, never signing material.
pub fn run(root: &Path) -> Result<serde_json::Value> {
    ensure!(
        root.is_absolute() && root.is_dir(),
        "provide an existing absolute empty fixture directory"
    );
    ensure!(
        std::fs::read_dir(root)?.next().is_none(),
        "refuse nonempty fixture directory"
    );
    let (source_key, mut source) = crate::open_local(&root.join("source"))?;
    let (reader_key, mut reader) = crate::open_local(&root.join("reader"))?;
    let reader_id = reader_key.verifying_key();
    let policy = CollectionPolicy::new(
        AdmissionPolicy::direct(source_key.verifying_key()),
        AdmissionPolicy::direct(source_key.verifying_key()),
    );
    let source_collection = source.collection("ios-synthetic-transfer", policy.clone())?;
    let collection = source_collection.handle();
    ensure!(
        reader
            .collection("ios-synthetic-transfer", policy)?
            .handle()
            == collection
    );
    // Explicit fixture authorization, independent of endpoint discovery and selection.
    let grant = CapabilityProof::new(
        CapabilityResource::from(collection),
        &source_key,
        read_capability(),
        reader_id,
    );
    source.insert_proof(grant.clone())?;
    reader.insert_proof(grant)?;
    select(&mut source, &source_key, collection)?;
    select(&mut reader, &reader_key, collection)?;
    let payload = "Synthetic Erlkonig replica payload.\n".repeat(64 * 1024);
    let fragment = entity! { _ @
        metadata::name: "Synthetic replica",
        metadata::description: payload.as_str(),
    };
    let handle = find!(handle: Inline<Handle<UTF8String>>,
        pattern!(fragment.facts(), [{ _?entity @ metadata::description: ?handle }]))
    .next()
    .context("fixture description handle")?;
    let record =
        CollectionRecord::Commit(source.commit(source_collection, &source_key, fragment)?);
    source.flush()?;
    reader.flush()?;
    ensure!(
        !reader.snapshot()?.contains_blob(handle)?,
        "fixture reader unexpectedly contains payload"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let source_endpoint = endpoint(&source_key).await?;
        let reader_endpoint = endpoint(&reader_key).await?;
        source_endpoint.address_lookup()?.add(MemoryLookup::from_endpoint_info([
            EndpointInfo::from(iroh_base::EndpointAddr::from_parts(reader_endpoint.id(),
                reader_endpoint.bound_sockets().into_iter().map(iroh_base::TransportAddr::Ip)))]));
        reader_endpoint.address_lookup()?.add(MemoryLookup::from_endpoint_info([
            EndpointInfo::from(iroh_base::EndpointAddr::from_parts(source_endpoint.id(),
                source_endpoint.bound_sockets().into_iter().map(iroh_base::TransportAddr::Ip)))]));
        let (mut source, source_owner) = peer(source_endpoint.clone(), source).await;
        let (mut reader, reader_owner) = peer(reader_endpoint.clone(), reader).await;
        let health = reader.health_reader();
        let mut observed_source = false;
        for peer in [&mut source, &mut reader] {
            peer.set_replication(ReplicationMode::Full, [collection]);
            peer.activate_collection(collection);
            peer.try_refresh()?;
        }
        let deadline = Instant::now() + Duration::from_secs(45);
        let transferred = loop {
            source.try_refresh()?; reader.try_refresh()?;
            let _ = tokio::time::timeout(Duration::from_millis(100), source.reconcile()).await;
            let _ = tokio::time::timeout(Duration::from_millis(100), reader.reconcile()).await;
            let snapshot = reader.snapshot()?;
            let has_record = snapshot.records()?.any(|value| value.is_ok_and(|value| value == record));
            observed_source |= health.snapshot().is_some_and(|h| {
                h.observed_at.is_some()
                    && h.connections.get(source_key.verifying_key().as_bytes())
                        .and_then(|c| c.traffic).is_some_and(|t| t.received_bytes > 0)
            });
            if observed_source && has_record && snapshot.contains_blob(handle)? { break true; }
            if Instant::now() >= deadline { break false; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        // Stop both owners and endpoints before consuming stores or offline verification.
        source_owner.abort(); reader_owner.abort();
        let _ = source_owner.await; let _ = reader_owner.await;
        source_endpoint.close().await; reader_endpoint.close().await;
        source.flush()?; reader.flush()?;
        source.close()?; reader.close()?;
        ensure!(observed_source, "passive peer health did not observe the authenticated loopback source");
        ensure!(health.snapshot().is_none(), "closed reader still publishes health");
        if !transferred { bail!("Full replication did not land record and large referenced UTF-8 blob within 45 seconds"); }
        Ok::<_, anyhow::Error>(())
    })?;
    drop(runtime);
    // No Peer or lazy acquisition exists here. The source is closed and offline.
    let (reopened_key, mut reopened) = crate::open_local(&root.join("reader"))?;
    ensure!(
        reopened_key.verifying_key() == reader_id,
        "reader identity changed on reopen"
    );
    ensure!(
        crate::selected(&mut reopened, &reopened_key)? == vec![collection],
        "selection did not persist"
    );
    let snapshot = reopened.snapshot()?;
    ensure!(
        snapshot
            .records()?
            .any(|value| value.is_ok_and(|value| value == record)),
        "record missing after reopen"
    );
    let facts: TribleSet = source_collection.read(&snapshot)?;
    ensure!(
        find!(description: Inline<Handle<UTF8String>>,
        pattern!(&facts, [{ _?entity @ metadata::description: ?description }]))
        .any(|description| description == handle),
        "native collection read lost description reference"
    );
    let restored: Blob<UTF8String> = snapshot.get(handle)?;
    ensure!(
        std::str::from_utf8(&restored.bytes)? == payload,
        "source-off payload read differs"
    );
    drop(restored);
    drop(snapshot);
    reopened.close()?;
    let report = serde_json::json!({"source_off_read":true,"identity_reopened":true,"observed_source_peer":true,
        "explicit_read_grant":true,"selection_reopened":true,"payload_bytes":payload.len(),
        "reader_node_id":hex::encode(reader_id.as_bytes()),"collection":hex::encode(collection.raw),
        "payload":hex::encode(handle.raw)});
    std::fs::write(
        root.join("proof-receipt.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(report)
}

/// Run on an owned stack, including when invoked from a small iOS dispatch thread.
/// The caller must already be off the UI thread; it waits for completion here.
pub fn run_owned(root: std::path::PathBuf, reopening: bool) -> Result<serde_json::Value> {
    std::thread::Builder::new()
        .name("ios-synthetic-proof".into())
        .stack_size(crate::WORKER_STACK_BYTES)
        .spawn(move || if reopening { reopen(&root) } else { run(&root) })
        .context("start synthetic proof worker")?
        .join()
        .map_err(|panic| {
            let message = panic.downcast_ref::<String>().map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            anyhow::anyhow!("synthetic proof worker panicked: {message}")
        })?
}


/// A separate process can repeat this local-only check without rerunning transfer.
pub fn reopen(root: &Path) -> Result<serde_json::Value> {
    ensure!(
        root.join("reader/node.key").is_file() && root.join("reader/replica.pile").is_file(),
        "existing synthetic reader required"
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("proof-receipt.json"))?)?;
    let (key, mut pile) = crate::open_local(&root.join("reader"))?;
    ensure!(
        hex::encode(key.verifying_key().as_bytes())
            == receipt["reader_node_id"].as_str().unwrap_or(""),
        "identity differs from completed transfer"
    );
    let roots = crate::selected(&mut pile, &key)?;
    ensure!(roots.len() == 1, "synthetic fixture selection differs");
    ensure!(
        hex::encode(roots[0].raw) == receipt["collection"].as_str().unwrap_or(""),
        "fixture collection differs"
    );
    let snapshot = pile.snapshot()?;
    let collection = triblespace_core::collection::Collection::<
        triblespace_core::blob::encodings::simplearchive::SimpleArchive,
    >::open(&snapshot, roots[0])?;
    let facts: TribleSet = collection.read(&snapshot)?;
    let handle = find!(handle: Inline<Handle<UTF8String>>,
        pattern!(&facts, [{ _?entity @ metadata::description: ?handle }]))
    .next()
    .context("reopened description")?;
    let payload: Blob<UTF8String> = snapshot.get(handle)?;
    ensure!(
        std::str::from_utf8(&payload.bytes)?
            == "Synthetic Erlkonig replica payload.\n".repeat(64 * 1024),
        "reopened payload differs"
    );
    drop(snapshot);
    pile.close()?;
    Ok(
        serde_json::json!({"separate_process_reopen":true,"reader_node_id":receipt["reader_node_id"],
        "source_off_read":true,"payload_bytes":payload.bytes.len()}),
    )
}
