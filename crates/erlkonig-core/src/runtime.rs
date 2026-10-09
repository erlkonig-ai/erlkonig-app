//! One node identity, one logical store and one app-owned network endpoint.

use crate::{
    config::NodeBinding,
    credentials, init,
    node_metrics::Sample,
    node_store::{ConfigView, NodeSettings, NodeStore},
    paths::{Layout, ensure_private_dir},
    status::{NodeStatus, Status},
    workspace::{AllAccessible, Workspace},
    workspace_dispatch::WorkspaceDispatch,
};
use anyhow::{Context, Result, bail};
use faculties::mcp::http;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use trible::maintenance::{Driver, Event, SuccinctBackend, Target};
use triblespace::core::{
    id::{Id, genid},
    repo::{SnapshotSource, StorageClose, StoreChanges, StoreSnapshot},
    signing_key_file,
};
use triblespace_net::peer::{Peer, PeerConfig, ReconcileDirection, ReconcileQos};
use triblespace_net::reconcile::ReplicationMode;

type SharedNode = Arc<Mutex<Option<NodeStore>>>;

pub struct Runtime {
    binding: NodeBinding,
    status: Arc<Mutex<Status>>,
    node: SharedNode,
    stop: watch::Sender<bool>,
    workers: Vec<JoinHandle<Result<()>>>,
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

fn with_node<T>(node: &SharedNode, f: impl FnOnce(&mut NodeStore) -> Result<T>) -> Result<T> {
    let mut guard = node.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.as_mut().context("node store is closed")?)
}

impl Runtime {
    pub fn start(layout: &Layout) -> Result<Self> {
        ensure_private_dir(&layout.dir)?;
        let lock = lease(&layout.lock()).context(
            "another Erlkönig owner is using this data directory; stop it before starting this app",
        )?;
        let (bootstrap, expected_signer) = layout.selected_node()?;
        let signer = if let Some(expected) = expected_signer {
            init::ensure_pile(&bootstrap.pile, false).map_err(anyhow::Error::msg)?;
            let signer = signing_key_file::load_existing(&bootstrap.key)?;
            if signer.verifying_key().to_bytes() != expected {
                bail!("node locator signing identity mismatch");
            }
            signer
        } else {
            ensure_private_dir(bootstrap.pile.parent().context("node pile parent")?)?;
            init::prepare(&bootstrap).map_err(anyhow::Error::msg)?
        };
        let binding = NodeBinding {
            pile: bootstrap.pile,
            key: bootstrap.key,
            signer: signer.verifying_key().to_bytes(),
        };
        let mut node = NodeStore::open(&binding.pile, signer.clone())?;
        let port = node.config()?.settings()?.mcp_port;
        let token = credentials::load_or_create(&layout.dir.join("mcp.token"))
            .map_err(anyhow::Error::msg)?;
        let mut config = http::Config::new(
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            http::BearerToken::new(&token)?,
        );
        config.body_timeout = Duration::from_secs(5);
        let endpoint = http::Endpoint::bind(config)
            .context("cannot open the local authenticated MCP service")?;
        let status = Arc::new(Mutex::new(Status {
            running: true,
            node_id: Some(hex::encode(signer.verifying_key().as_bytes())),
            mcp_url: Some(format!("http://{}/", endpoint.local_addr()?)),
            node: NodeStatus {
                pile: binding.pile.display().to_string(),
                key: binding.key.display().to_string(),
                node_id: Some(hex::encode(binding.signer)),
                ..NodeStatus::default()
            },
            ..Status::default()
        }));
        let node = Arc::new(Mutex::new(Some(node)));
        let (stop, _) = watch::channel(false);
        let mut app = Self {
            binding,
            status,
            node,
            stop,
            workers: Vec::new(),
            lock: Some(lock),
        };
        let http_stop = app.stop.subscribe();
        let state = app.status.clone();
        let node = app.node.clone();
        let binding = app.binding.clone();
        app.workers.push(
            thread::Builder::new()
                .name("erlkonig-faculties".into())
                .spawn(move || {
                    let result = (|| {
                        let dispatch = WorkspaceDispatch::new(binding, move || {
                            with_node(&node, NodeStore::config)
                        })?;
                        dispatch.finish(endpoint.serve_until(&[&dispatch], stopping(http_stop)))
                    })();
                    if let Err(error) = &result {
                        update(&state, |s| {
                            s.problem = Some(format!("MCP service: {error:#}"))
                        });
                    }
                    result
                })?,
        );
        let state = app.status.clone();
        let node = app.node.clone();
        let control_stop = app.stop.subscribe();
        let binding = app.binding.clone();
        let identity_lock = std::env::temp_dir().join(format!(
            "erlkonig-endpoint-{}.lock",
            hex::encode(binding.signer)
        ));
        app.workers.push(
            thread::Builder::new()
                .name("erlkonig-controller".into())
                .spawn(move || {
                    let result = controller(
                        binding,
                        signer,
                        identity_lock,
                        node,
                        control_stop,
                        state.clone(),
                        port,
                    );
                    if let Err(error) = &result {
                        update(&state, |s| {
                            s.config_problem = Some(format!("controller: {error:#}"))
                        });
                    }
                    result
                })?,
        );
        Ok(app)
    }

    pub fn config(&self) -> Result<ConfigView> {
        with_node(&self.node, NodeStore::config)
    }

    pub fn binding(&self) -> &NodeBinding {
        &self.binding
    }

    pub fn put_settings(&self, expected: &ConfigView, value: &NodeSettings) -> Result<()> {
        if *self.stop.borrow() {
            bail!("application is shutting down");
        }
        with_node(&self.node, |node| {
            node.put_settings(expected, value).map(|_| ())
        })
    }

    pub fn create_workspace(&self, expected: &ConfigView, label: &str) -> Result<Id> {
        if *self.stop.borrow() {
            bail!("application is shutting down");
        }
        with_node(&self.node, |node| node.create_workspace(expected, label))
    }

    pub fn put_workspace(&self, expected: &ConfigView, workspace: &Workspace) -> Result<Id> {
        if *self.stop.borrow() {
            bail!("application is shutting down");
        }
        with_node(&self.node, |node| node.put_workspace(expected, workspace))
    }

    pub fn remove_workspace(&self, expected: &ConfigView, anchor: Id) -> Result<Id> {
        if *self.stop.borrow() {
            bail!("application is shutting down");
        }
        with_node(&self.node, |node| node.remove_workspace(expected, anchor))
    }

    pub fn remove_workspace_revision(&self, expected: &ConfigView, revision: Id) -> Result<Id> {
        if *self.stop.borrow() {
            bail!("application is shutting down");
        }
        with_node(&self.node, |node| {
            node.remove_workspace_revision(expected, revision)
        })
    }

    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn begin_shutdown(&self) {
        self.stop.send_replace(true);
    }
    pub fn is_finished(&self) -> bool {
        self.workers.iter().all(JoinHandle::is_finished)
    }

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
        if let Some(node) = self.node.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if let Err(e) = node.close() {
                failures.push(format!("close node pile: {e:#}"));
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
            bail!(failures.join("; "))
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

fn lease(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.try_lock()
        .context("another application session owns this resource")?;
    Ok(file)
}

struct Task {
    settings: NodeSettings,
    stop: watch::Sender<bool>,
    worker: JoinHandle<Result<()>>,
}

fn join_task(task: Task, status: &Arc<Mutex<Status>>) {
    task.stop.send_replace(true);
    let error = match task.worker.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("{error:#}")),
        Err(_) => Some("node worker panicked".into()),
    };
    update(status, |s| {
        s.node.sync_running = false;
        s.node.maintenance_running = false;
        s.node.bound_sockets.clear();
        if error.is_some() {
            s.node.problem = error;
        }
    });
}

fn controller(
    binding: NodeBinding,
    signer: ed25519_dalek::SigningKey,
    identity_lock: std::path::PathBuf,
    node: SharedNode,
    stop: watch::Receiver<bool>,
    status: Arc<Mutex<Status>>,
    initial_port: u16,
) -> Result<()> {
    let (reports, receiver) = mpsc::channel();
    let mut task: Option<Task> = None;
    let mut pending: HashMap<(Id, String), Sample> = HashMap::new();
    let mut failures = Vec::new();
    let mut last_metrics = Instant::now();
    let mut last_guard = Instant::now() - Duration::from_secs(1);
    while !*stop.borrow() {
        if last_guard.elapsed() >= Duration::from_secs(1) {
            if let Err(error) = crate::resource_guard::sample() {
                if let Some(task) = &task {
                    task.stop.send_replace(true);
                }
                update(&status, |s| s.node.problem = Some(format!("{error:#}")));
            }
            last_guard = Instant::now();
        }
        match with_node(&node, |node| {
            node.poll_config()?.map(|view| view.settings()).transpose()
        }) {
            Ok(Some(settings)) => {
                // Workspaces select views: changing their facts must not reopen
                // the node endpoint or alter what it is allowed to synchronize.
                let unchanged = task.as_ref().is_some_and(|task| {
                    task.settings.sync_enabled == settings.sync_enabled
                        && task.settings.maintenance_enabled == settings.maintenance_enabled
                        && task.settings.peers == settings.peers
                });
                if !unchanged {
                    if let Some(old) = task.take() {
                        join_task(old, &status);
                    }
                }
                update(&status, |s| {
                    s.config_generation += 1;
                    s.config_problem = None;
                    s.node.sync_enabled = settings.sync_enabled;
                    s.node.maintenance_enabled = settings.maintenance_enabled;
                    s.warnings = if settings.mcp_port != initial_port {
                        vec!["Restart Erlkönig to use the changed MCP port.".into()]
                    } else {
                        Vec::new()
                    };
                });
                if task.is_none() && (settings.sync_enabled || settings.maintenance_enabled) {
                    // Clear only the previous session's error, before the new
                    // worker can race with a controller guard cancellation.
                    update(&status, |s| s.node.problem = None);
                    let (cancel, cancel_rx) = watch::channel(false);
                    let worker_settings = settings.clone();
                    let worker_binding = binding.clone();
                    let worker_signer = signer.clone();
                    let lock = identity_lock.clone();
                    let state = status.clone();
                    let metrics = reports.clone();
                    let shared = node.clone();
                    let worker =
                        thread::Builder::new()
                            .name("erlkonig-node".into())
                            .spawn(move || {
                                let result = (|| {
                                    crate::resource_guard::sample()?;
                                    let pool = rayon::ThreadPoolBuilder::new()
                                        .num_threads(4)
                                        .start_handler(|_| crate::resource_guard::background())
                                        .build()?;
                                    pool.install(|| {
                                        node_session(
                                            &worker_binding,
                                            &worker_signer,
                                            &worker_settings,
                                            &lock,
                                            shared,
                                            cancel_rx,
                                            &state,
                                            &metrics,
                                        )
                                    })
                                })();
                                update(&state, |s| {
                                    s.node.sync_running = false;
                                    s.node.maintenance_running = false;
                                    s.node.bound_sockets.clear();
                                    if let Err(error) = &result {
                                        s.node.problem = Some(format!("{error:#}"));
                                    }
                                });
                                result
                            })?;
                    task = Some(Task {
                        settings,
                        stop: cancel,
                        worker,
                    });
                }
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(old) = task.take() {
                    join_task(old, &status);
                }
                update(&status, |s| {
                    s.config_problem = Some(format!("{error:#}; node services stopped"))
                });
            }
        }
        for sample in receiver.try_iter() {
            if sample.error.is_some() {
                failures.push(sample.clone());
            }
            pending.insert((sample.session, sample.role.clone()), sample);
        }
        if (!pending.is_empty() && last_metrics.elapsed() >= Duration::from_secs(10))
            || !failures.is_empty()
        {
            let mut batch: Vec<_> = pending.drain().map(|(_, sample)| sample).collect();
            batch.append(&mut failures);
            let result = with_node(&node, |node| node.append_metrics(&batch));
            update(&status, |s| {
                s.metrics_problem = result.err().map(|error| format!("{error:#}"))
            });
            last_metrics = Instant::now();
        }
        thread::sleep(Duration::from_millis(200));
    }
    if let Some(old) = task.take() {
        join_task(old, &status);
    }
    for sample in receiver.try_iter() {
        pending.insert((sample.session, sample.role.clone()), sample);
    }
    let mut batch: Vec<_> = pending.into_values().collect();
    batch.append(&mut failures);
    with_node(&node, |node| node.append_metrics(&batch))?;
    Ok(())
}

fn sample(
    endpoint: ed25519_dalek::VerifyingKey,
    session: Id,
    role: &str,
    elapsed: Duration,
) -> Option<Sample> {
    Sample::now(None, "native", role, session, elapsed)
        .ok()
        .map(|mut value| {
            value.endpoint = Some(endpoint);
            value
        })
}

fn maintenance_observer<'a>(
    endpoint: ed25519_dalek::VerifyingKey,
    session: Id,
    status: &'a Arc<Mutex<Status>>,
    metrics: &'a mpsc::Sender<Sample>,
) -> impl FnMut(Event) + Send + 'a {
    let started = Instant::now();
    let mut published = 0u128;
    let mut failed = 0u128;
    move |event| {
        let (count, error) = match event {
            Event::HopFinished {
                publications,
                error,
                ..
            } => (
                publications
                    .merges
                    .saturating_add(publications.derives)
                    .saturating_add(publications.maps),
                error,
            ),
            Event::Failure { error, .. } => (0, Some(error)),
            _ => return,
        };
        published = published.saturating_add(count);
        failed += u128::from(error.is_some());
        update(status, |s| {
            s.maintenance_publications = s
                .maintenance_publications
                .saturating_add(count.min(u64::MAX as u128) as u64);
            s.node.maintenance_publications = s.maintenance_publications;
            if let Some(error) = &error {
                s.node.problem = Some(format!("Maintenance: {error}"));
            }
        });
        if let Some(mut value) = sample(endpoint, session, "maintenance", started.elapsed()) {
            value.publications = Some(published);
            value.failed = Some(failed);
            value.error = error;
            let _ = metrics.send(value);
        }
    }
}

fn node_session(
    binding: &NodeBinding,
    signer: &ed25519_dalek::SigningKey,
    settings: &NodeSettings,
    identity_lock: &Path,
    node: SharedNode,
    stop: watch::Receiver<bool>,
    status: &Arc<Mutex<Status>>,
    metrics: &mpsc::Sender<Sample>,
) -> Result<()> {
    // This lock guards endpoint identity, NEVER exclusive access to the pile.
    // Other native appenders and readers are legitimate concurrent users.
    let _endpoint_lease = if settings.sync_enabled {
        Some(lease(identity_lock)?)
    } else {
        None
    };
    let current = signing_key_file::load_existing(&binding.key)?;
    if current.verifying_key().to_bytes() != binding.signer {
        bail!("node identity changed");
    }
    let session = genid().id;
    let started = Instant::now();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let store = faculties::storage::open_pile_strict_as(&binding.pile, signer.verifying_key())?;
    let mut observer = maintenance_observer(signer.verifying_key(), session, status, metrics);
    let result = if settings.sync_enabled {
        // Same startup and bounded periodic backstop as the native CLI.
        // Its RAII handle joins only this app's walker during shutdown.
        let held_walker = store.start_held_walker(Default::default());
        let peers = settings
            .peers
            .iter()
            .map(|bytes| {
                iroh_base::EndpointId::from_bytes(bytes).map(iroh_base::EndpointAddr::from)
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut peer = Peer::new(
            store,
            signer.clone(),
            PeerConfig {
                peers,
                qos: ReconcileQos {
                    direction: ReconcileDirection::Bidirectional,
                },
                provider_publication_budget: None,
                bind: None,
            },
        )
        .context("cannot start native node endpoint")?;
        let mut driver = None;
        let mut selection = AllAccessible::default();
        let mut previous_snapshot = None;
        let mut previous_sent = 0u64;
        let mut received = 0u128;
        update(status, |s| {
            s.node.sync_running = true;
            s.node.maintenance_running = settings.maintenance_enabled;
            s.node.bound_sockets = peer
                .bound_sockets()
                .iter()
                .map(ToString::to_string)
                .collect();
            s.node.sessions_started += 1;
        });
        let driven: Result<()> = runtime.block_on(async {
            let stopped = stopping(stop); tokio::pin!(stopped);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    result = async {
                        peer.try_refresh()?;
                        let snapshot = peer.snapshot()?;
                        if previous_snapshot.as_ref().is_none_or(|previous| snapshot.changes_since(previous) != StoreChanges::NONE) {
                            let roots = with_node(&node, NodeStore::workspace_roots)?;
                            let next = AllAccessible::discover(&snapshot, signer.verifying_key(), roots)?;
                            if previous_snapshot.is_none() || selection.sync != next.sync {
                                peer.set_replication(ReplicationMode::Full, next.sync.iter().copied());
                                peer.activate_collections(next.sync.iter().copied());
                            }
                            if settings.maintenance_enabled && (driver.is_none() || selection.maintenance != next.maintenance) {
                                driver = Some(Driver::new(next.maintenance.iter().copied().map(Target::Handle), true, SuccinctBackend::Cpu)?);
                            }
                            update(status, |s| {
                                s.sync_collections = next.sync.len(); s.maintenance_targets = next.maintenance.len();
                                s.warnings = next.pending.clone();
                            });
                            selection = next;
                        }
                        previous_snapshot = Some(snapshot);
                        let progress = peer.reconcile().await;
                        received += progress.received_bytes as u128;
                        let sent = peer.health().blob_serving.sent_bytes;
                        let delta = progress.received_bytes.saturating_add(sent.saturating_sub(previous_sent));
                        previous_sent = sent;
                        if delta > 0 {
                            update(status, |s| { s.transfer_bytes += delta; s.node.transfer_bytes = s.transfer_bytes; });
                            if let Some(mut value) = sample(signer.verifying_key(), session, "sync", started.elapsed()) {
                                value.received_bytes = Some(received); value.sent_bytes = Some(sent as u128); let _ = metrics.send(value);
                            }
                        }
                        if let Some(driver) = &mut driver { driver.tick(&mut peer, signer, &mut observer).await?; }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        Ok::<(), anyhow::Error>(())
                    } => result?,
                }
            }
            Ok(())
        });
        drop(driver);
        drop(previous_snapshot);
        let stopped = peer
            .shutdown_and_join()
            .context("stop owned native endpoint");
        drop(held_walker);
        let closed = peer.close().context("close owned node handle");
        driven.and(stopped).and(closed)
    } else {
        let mut store = store;
        let mut driver = None;
        let mut selection = AllAccessible::default();
        let mut previous_snapshot = None;
        update(status, |s| {
            s.node.maintenance_running = true;
            s.node.sessions_started += 1;
        });
        let driven: Result<()> = runtime.block_on(async {
            let stopped = stopping(stop); tokio::pin!(stopped);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    result = async {
                        store.refresh()?;
                        let snapshot = store.snapshot()?;
                        if previous_snapshot.as_ref().is_none_or(|previous| snapshot.changes_since(previous) != StoreChanges::NONE) {
                            let next = AllAccessible::discover(&snapshot, signer.verifying_key(),
                                with_node(&node, NodeStore::workspace_roots)?)?;
                            if driver.is_none() || selection.maintenance != next.maintenance {
                                driver = Some(Driver::new(next.maintenance.iter().copied().map(Target::Handle), true, SuccinctBackend::Cpu)?);
                            }
                            update(status, |s| { s.maintenance_targets = next.maintenance.len(); s.warnings = next.pending.clone(); });
                            selection = next;
                        }
                        previous_snapshot = Some(snapshot);
                        if let Some(driver) = &mut driver { driver.tick(&mut store, signer, &mut observer).await?; }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        Ok::<(), anyhow::Error>(())
                    } => result?,
                }
            }
            Ok(())
        });
        drop(driver);
        drop(previous_snapshot);
        driven.and(store.close().context("close local maintenance handle"))
    };
    if let Some(mut value) = sample(
        signer.verifying_key(),
        session,
        "session",
        started.elapsed(),
    ) {
        value.failed = Some(u128::from(result.is_err()));
        value.error = result.as_ref().err().map(|e| format!("{e:#}"));
        let _ = metrics.send(value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_cancellation_before_startup_status_keeps_its_explanation() {
        for sync_enabled in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let bootstrap = crate::config::Bootstrap::new(dir.path());
            let signer = init::prepare(&bootstrap).unwrap();
            let node = Arc::new(Mutex::new(Some(
                NodeStore::open(&bootstrap.pile, signer.clone()).unwrap(),
            )));
            let binding = NodeBinding {
                pile: bootstrap.pile,
                key: bootstrap.key,
                signer: signer.verifying_key().to_bytes(),
            };
            let settings = NodeSettings {
                sync_enabled,
                ..NodeSettings::default()
            };
            let status = Arc::new(Mutex::new(Status::default()));
            let (cancel, stop) = watch::channel(false);
            let (metrics, _reports) = mpsc::channel();

            // The controller deliberately started this task, then its guard
            // fired before synchronous setup published startup status. No
            // timing or actual memory pressure is needed to force this order.
            let reason = crate::resource_guard::check(48 * 1024 * 1024 * 1024, 100)
                .unwrap_err()
                .to_string();
            cancel.send_replace(true);
            update(&status, |s| s.node.problem = Some(reason.clone()));
            node_session(
                &binding,
                &signer,
                &settings,
                &dir.path().join("endpoint.lock"),
                node.clone(),
                stop,
                &status,
                &metrics,
            )
            .unwrap();

            let state = status.lock().unwrap();
            assert_eq!(state.node.sessions_started, 1);
            assert_eq!(
                state.node.problem.as_deref(),
                Some(reason.as_str()),
                "startup must retain the guard explanation (sync={sync_enabled})"
            );
            assert!(*cancel.borrow(), "startup must not undo cancellation");
            drop(state);
            assert_eq!(
                triblespace::core::collection::held::held_walker_threads(),
                0
            );
            node.lock().unwrap().take().unwrap().close().unwrap();
        }
    }
}
