//! One application lifetime: a native replication/maintenance worker and an
//! authenticated HTTP faculty worker. No resident service is a child process;
//! individual legacy Faculty commands can still invoke external programs.

use crate::{
    config::{NodeConfig, Resolved},
    credentials, init,
    paths::{Layout, ensure_private_dir},
    status::Status,
};
use anyhow::{Context, Result};
use faculties::mcp::{catalog, http};
use faculties::storage::Storage;
use std::fs::{File, OpenOptions};
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tokio::sync::watch;
use trible::maintenance::{Driver, Event, SuccinctBackend, Target};
use triblespace::core::{collection::CollectionHandle, repo::StorageClose};
use triblespace_net::peer::{Peer, PeerConfig, ReconcileDirection, ReconcileQos};
use triblespace_net::reconcile::ReplicationMode;

pub struct Runtime {
    status: Arc<Mutex<Status>>,
    stop: watch::Sender<bool>,
    workers: Vec<JoinHandle<Result<()>>>,
    // The app holds the same lock as older nodes, so it fails visibly rather
    // than competing with an owner left running during a migration.
    lock: Option<File>,
}

fn update(status: &Arc<Mutex<Status>>, f: impl FnOnce(&mut Status)) {
    f(&mut status.lock().unwrap_or_else(|e| e.into_inner()));
}

async fn stopping(mut stop: watch::Receiver<bool>) {
    if !*stop.borrow() {
        let _ = stop.changed().await;
    }
}

impl Runtime {
    pub fn start(layout: &Layout) -> Result<Self> {
        ensure_private_dir(&layout.dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(layout.lock())?;
        lock.try_lock().map_err(|_| anyhow::anyhow!("another Erlkönig owner is using this data directory; stop it before starting this app"))?;
        let resolved = NodeConfig::load(&layout.config())
            .map_err(anyhow::Error::msg)?
            .resolve(&layout.dir);
        let signer = init::prepare(&resolved).map_err(anyhow::Error::msg)?;
        let token = credentials::load_or_create(&layout.dir.join("mcp.token"))
            .map_err(anyhow::Error::msg)?;
        let mut config = http::Config::new(
            SocketAddr::from((Ipv4Addr::LOCALHOST, resolved.mcp_port)),
            http::BearerToken::new(&token)?,
        );
        config.body_timeout = Duration::from_secs(5);
        let endpoint = http::Endpoint::bind(config)
            .context("cannot open the local authenticated MCP service")?;
        let url = format!("http://{}/", endpoint.local_addr()?);
        let status = Arc::new(Mutex::new(Status {
            running: true,
            node_id: Some(hex::encode(signer.verifying_key().as_bytes())),
            mcp_url: Some(url),
            sync_collections: resolved.sync_collections.len(),
            maintenance_targets: resolved.maintain_targets.len(),
            warnings: std::env::var_os("HOME")
                .map(|home| resolved.protected_folder_warnings(std::path::Path::new(&home)))
                .unwrap_or_default(),
            ..Status::default()
        }));
        let (stop, _) = watch::channel(false);
        let mut app = Self {
            status,
            stop,
            workers: Vec::new(),
            lock: Some(lock),
        };
        let http_stop = app.stop.subscribe();
        let state = app.status.clone();
        let faculty_config = resolved.clone();
        app.workers.push(
            thread::Builder::new()
                .name("erlkonig-faculties".into())
                .spawn(move || {
                    let result = (|| {
                        let peers = routes(&faculty_config)?;
                        let storage = Storage::shared_with_peers(
                            faculty_config.pile.clone(),
                            Some(faculty_config.key.clone()),
                            peers,
                        );
                        let mut config = catalog::Config::new(&faculty_config.pile);
                        config.key = Some(faculty_config.key);
                        // Adapters are deliberately constructed here: they need not be Send.
                        let catalog = catalog::Catalog::with_storage(config, storage);
                        let registrations = catalog.registrations();
                        catalog.finish(endpoint.serve_until(&registrations, stopping(http_stop)))
                    })();
                    if let Err(error) = &result {
                        update(&state, |s| {
                            s.problem = Some(format!("MCP service: {error:#}"))
                        });
                    }
                    result
                })?,
        );
        if !resolved.sync_collections.is_empty() || !resolved.maintain_targets.is_empty() {
            let native_stop = app.stop.subscribe();
            let state = app.status.clone();
            app.workers.push(
                thread::Builder::new()
                    .name("erlkonig-native".into())
                    .spawn(move || {
                        let result = native(resolved, signer, native_stop, &state);
                        if let Err(error) = &result {
                            update(&state, |s| {
                                s.problem = Some(format!("Native service: {error:#}"))
                            });
                        }
                        result
                    })?,
            );
        }
        Ok(app)
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Stop accepting HTTP work and cancel native async work. The UI remains
    /// responsive while already-admitted, non-cancellable faculty writes drain.
    pub fn begin_shutdown(&self) {
        self.stop.send_replace(true);
    }

    pub fn is_finished(&self) -> bool {
        self.workers.iter().all(JoinHandle::is_finished)
    }

    /// Join before releasing the data lock. Call after is_finished in the UI.
    pub fn finish(&mut self) -> Result<()> {
        self.begin_shutdown();
        let mut failures = Vec::new();
        for worker in self.workers.drain(..) {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => failures.push(format!("{e:#}")),
                Err(_) => failures.push("native worker panicked".into()),
            }
        }
        self.lock.take();
        update(&self.status, |s| {
            s.running = false;
            s.mcp_url = None;
        });
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(failures.join("; "))
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Err(error) = self.finish() {
            eprintln!("Erlkönig shutdown: {error:#}");
        }
    }
}

fn routes(config: &Resolved) -> Result<Vec<iroh_base::EndpointAddr>> {
    config
        .peers
        .iter()
        .map(|s| {
            s.parse::<iroh_base::EndpointId>()
                .map(iroh_base::EndpointAddr::from)
                .map_err(Into::into)
        })
        .collect()
}

fn handle(text: &str) -> Result<CollectionHandle> {
    let raw: [u8; 32] = hex::decode(text.strip_prefix("blake3:").unwrap_or(text))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("collection handle must contain 32 bytes"))?;
    Ok(CollectionHandle::new(raw))
}

fn native(
    config: Resolved,
    signer: ed25519_dalek::SigningKey,
    stop: watch::Receiver<bool>,
    status: &Arc<Mutex<Status>>,
) -> Result<()> {
    let store = faculties::storage::open_pile_strict_as(&config.pile, signer.verifying_key())?;
    let mut peer = Peer::new(
        store,
        signer.clone(),
        PeerConfig {
            peers: routes(&config)?,
            qos: ReconcileQos {
                direction: ReconcileDirection::Bidirectional,
            },
            provider_publication_budget: None,
            bind: None,
        },
    )
    .context("cannot start the native network host")?;
    let collections = config
        .sync_collections
        .iter()
        .map(|s| handle(s))
        .collect::<Result<Vec<_>>>()?;
    peer.set_replication(ReplicationMode::Full, collections.iter().copied());
    peer.activate_collections(collections);
    let targets = config
        .maintain_targets
        .iter()
        .map(|s| handle(s).map(Target::Handle))
        .collect::<Result<Vec<_>>>()?;
    let mut driver = Driver::new(targets, true, SuccinctBackend::Cpu)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut received_bytes = 0u64;
    let result: Result<()> = runtime.block_on(async {
        let stopped = stopping(stop);
        tokio::pin!(stopped);
        loop {
            tokio::select! {
                biased;
                _ = &mut stopped => break,
                result = async {
                    peer.try_refresh()?;
                    let progress = peer.reconcile().await;
                    received_bytes = received_bytes.saturating_add(progress.received_bytes);
                    let sent = peer.health().blob_serving.sent_bytes;
                    update(status, |s| s.transfer_bytes = received_bytes.saturating_add(sent));
                    let mut observer = |event| match event {
                        Event::HopFinished { publications, error, .. } => update(status, |s| {
                            let count = publications.merges.saturating_add(publications.derives).saturating_add(publications.maps).min(u64::MAX as u128) as u64;
                            s.maintenance_publications = s.maintenance_publications.saturating_add(count);
                            if let Some(error) = error { s.problem = Some(format!("Maintenance: {error}")); }
                        }),
                        Event::Failure { error, .. } => update(status, |s| s.problem = Some(format!("Maintenance: {error}"))),
                        _ => {}
                    };
                    driver.tick(&mut peer, &signer, &mut observer).await?;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    Ok::<(), anyhow::Error>(())
                } => result?,
            }
        }
        Ok(())
    });
    // The driver's invalidation snapshots must not retain network ownership.
    drop(driver);
    let shutdown = peer
        .shutdown_and_join()
        .context("stop native application network host");
    let closed = peer.close().context("close native application store");
    let failures: Vec<_> = [result, shutdown, closed]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(failures.join("; "))
    }
}
