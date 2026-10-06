//! One app-owned lifetime, with independently cancellable managed-pile sessions.

use crate::{
    config::Bootstrap,
    credentials, init,
    node_metrics::Sample,
    node_store::{ConfigView, ManagedPile, NodeStore},
    paths::{Layout, ensure_private_dir},
    status::{PileStatus, Status},
};
use anyhow::{Context, Result, bail};
use faculties::mcp::{catalog, http};
use faculties::storage::Storage;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use trible::maintenance::{Driver, Event, SuccinctBackend, Target};
use triblespace::core::{
    id::{Id, genid},
    repo::StorageClose,
    signing_key_file,
};
use triblespace_net::peer::{Peer, PeerConfig, ReconcileDirection, ReconcileQos};
use triblespace_net::reconcile::ReplicationMode;

type SharedNode = Arc<Mutex<Option<NodeStore>>>;

pub struct Runtime {
    status: Arc<Mutex<Status>>,
    node: SharedNode,
    stop: watch::Sender<bool>,
    workers: Vec<JoinHandle<Result<()>>>,
    lock: Option<File>,
}

fn update(status: &Arc<Mutex<Status>>, f: impl FnOnce(&mut Status)) {
    f(&mut status.lock().unwrap_or_else(|e| e.into_inner()));
}

fn update_pile(status: &Arc<Mutex<Status>>, id: Id, f: impl FnOnce(&mut PileStatus)) {
    update(status, |s| {
        if let Some(p) = s.managed.iter_mut().find(|p| p.id == id.to_string()) {
            f(p);
        }
    });
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
        let bootstrap = Bootstrap::new(&layout.dir);
        let signer = init::prepare(&bootstrap).map_err(anyhow::Error::msg)?;
        let mut node = NodeStore::open(&bootstrap.pile, signer.clone())?;
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
            ..Status::default()
        }));
        let node = Arc::new(Mutex::new(Some(node)));
        let (stop, _) = watch::channel(false);
        let mut app = Self {
            status,
            node,
            stop,
            workers: Vec::new(),
            lock: Some(lock),
        };
        let http_stop = app.stop.subscribe();
        let state = app.status.clone();
        app.workers.push(
            thread::Builder::new()
                .name("erlkonig-faculties".into())
                .spawn(move || {
                    let result = (|| {
                        // MCP is deliberately one data world: the fixed node pile only.
                        let storage = Storage::shared_with_peers(
                            bootstrap.pile.clone(),
                            Some(bootstrap.key.clone()),
                            Vec::new(),
                        );
                        let mut config = catalog::Config::new(&bootstrap.pile);
                        config.key = Some(bootstrap.key);
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
        let state = app.status.clone();
        let node = app.node.clone();
        let control_stop = app.stop.subscribe();
        let layout = layout.clone();
        app.workers.push(
            thread::Builder::new()
                .name("erlkonig-controller".into())
                .spawn(move || {
                    let result = controller(layout, node, control_stop, state.clone(), port);
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

    pub fn put_managed(&self, expected: &ConfigView, pile: &ManagedPile) -> Result<()> {
        if *self.stop.borrow() {
            bail!("application is shutting down");
        }
        with_node(&self.node, |node| {
            node.put_managed(expected, pile).map(|_| ())
        })
    }

    pub fn remove_managed(&self, expected: &ConfigView, id: Id) -> Result<()> {
        if *self.stop.borrow() {
            bail!("application is shutting down");
        }
        with_node(&self.node, |node| {
            node.remove_managed(expected, id).map(|_| ())
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

fn file_identity(path: &Path) -> Result<(u64, u64)> {
    let metadata = fs::metadata(path).with_context(|| format!("cannot open {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok((metadata.dev(), metadata.ino()))
}

struct Task {
    config: ManagedPile,
    stop: watch::Sender<bool>,
    worker: JoinHandle<Result<()>>,
}

fn join_task(task: Task, status: &Arc<Mutex<Status>>) {
    task.stop.send_replace(true);
    let id = task.config.id;
    let error = match task.worker.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("{error:#}")),
        Err(_) => Some("pile worker panicked".into()),
    };
    update_pile(status, id, |p| {
        p.sync_running = false;
        p.maintenance_running = false;
        p.bound_sockets.clear();
        if error.is_some() {
            p.problem = error;
        }
    });
}

fn controller(
    layout: Layout,
    node: SharedNode,
    stop: watch::Receiver<bool>,
    status: Arc<Mutex<Status>>,
    initial_port: u16,
) -> Result<()> {
    let ownership_dir = layout.dir.join("owners");
    ensure_private_dir(&ownership_dir)?;
    let root_identity = file_identity(&layout.default_pile())?;
    let (reports, receiver) = mpsc::channel();
    let mut tasks: Vec<Task> = Vec::new();
    let mut pending: HashMap<(Option<Id>, Id, String), Sample> = HashMap::new();
    let mut failures = Vec::new();
    let mut last_metrics = Instant::now() - Duration::from_secs(10);
    while !*stop.borrow() {
        match with_node(&node, |node| {
            node.poll_config()?
                .map(|view| {
                    let ids = view.managed_ids()?;
                    let settings = view.settings()?;
                    Ok((view, ids, settings))
                })
                .transpose()
        }) {
            Ok(Some((view, ids, settings))) => {
                let mut desired = Vec::new();
                let mut entries = Vec::new();
                for id in ids {
                    match view.managed(id) {
                        Ok(Some(config)) => {
                            entries.push(PileStatus {
                                id: id.to_string(),
                                label: config.label.clone(),
                                pile: config.pile.display().to_string(),
                                key: config.key.display().to_string(),
                                sync_enabled: config.sync_enabled,
                                maintenance_enabled: config.maintenance_enabled,
                                ..PileStatus::default()
                            });
                            desired.push(config);
                        }
                        Ok(None) => {}
                        Err(error) => entries.push(PileStatus {
                            id: id.to_string(),
                            label: id.to_string(),
                            problem: Some(format!("Configuration: {error:#}")),
                            ..PileStatus::default()
                        }),
                    }
                }
                // This is a disposable activation plan, not a second configuration store.
                let mut identities = HashMap::new();
                let mut signers = HashMap::new();
                let mut invalid = HashMap::new();
                for config in &desired {
                    if !config.sync_enabled && !config.maintenance_enabled {
                        continue;
                    }
                    let result = (|| {
                        let identity = file_identity(&config.pile)?;
                        if identity == root_identity {
                            bail!(
                                "the node pile is already owned by the app; it cannot be added as a managed pile"
                            );
                        }
                        if let Some(other) = identities.insert(identity, config.id) {
                            invalid.insert(other, "the same pile file is configured more than once (including aliases)".to_owned());
                            bail!(
                                "the same pile file is configured more than once (including aliases)"
                            );
                        }
                        let signer = signing_key_file::load_existing(&config.key)
                            .context("cannot load the selected signing identity")?;
                        if config.sync_enabled {
                            if config.sync_collections.is_empty() {
                                bail!("Sync is enabled but no exact collections are selected");
                            }
                            if let Some(other) =
                                signers.insert(signer.verifying_key().to_bytes(), config.id)
                            {
                                invalid.insert(other, "another syncing pile uses this signing identity; choose a distinct key".to_owned());
                                bail!(
                                    "another syncing pile uses this signing identity; choose a distinct key"
                                );
                            }
                        }
                        if config.maintenance_enabled && config.maintain_targets.is_empty() {
                            bail!("Maintenance is enabled but no exact targets are selected");
                        }
                        Ok::<_, anyhow::Error>(())
                    })();
                    if let Err(error) = result {
                        invalid.insert(config.id, format!("{error:#}"));
                    }
                }
                let keep: HashSet<Id> = tasks
                    .iter()
                    .filter(|task| {
                        !task.worker.is_finished()
                            && desired.iter().any(|config| config == &task.config)
                            && !invalid.contains_key(&task.config.id)
                    })
                    .map(|task| task.config.id)
                    .collect();
                let mut retained = Vec::new();
                for task in tasks.drain(..) {
                    if keep.contains(&task.config.id) {
                        retained.push(task);
                    } else {
                        task.stop.send_replace(true);
                        join_task(task, &status);
                    }
                }
                tasks = retained;
                update(&status, |s| {
                    for entry in &mut entries {
                        if let Some(old) = s.managed.iter().find(|old| old.id == entry.id) {
                            entry.transfer_bytes = old.transfer_bytes;
                            entry.maintenance_publications = old.maintenance_publications;
                            entry.sessions_started = old.sessions_started;
                            entry.node_id = old.node_id.clone();
                            if keep.iter().any(|id| id.to_string() == entry.id) {
                                entry.sync_running = old.sync_running;
                                entry.maintenance_running = old.maintenance_running;
                                entry.bound_sockets = old.bound_sockets.clone();
                                entry.problem = old.problem.clone();
                            }
                        }
                        if let Some((_, problem)) =
                            invalid.iter().find(|(id, _)| id.to_string() == entry.id)
                        {
                            entry.problem = Some(problem.clone());
                        }
                    }
                    s.managed = entries;
                    s.config_problem = None;
                    s.warnings = if settings.mcp_port != initial_port {
                        vec!["The MCP port changed in the node pile; restart Erlkönig to bind the new port.".into()]
                    } else {
                        Vec::new()
                    };
                    s.config_generation += 1;
                    s.sync_collections = desired
                        .iter()
                        .filter(|c| c.sync_enabled && !invalid.contains_key(&c.id))
                        .map(|c| c.sync_collections.len())
                        .sum();
                    s.maintenance_targets = desired
                        .iter()
                        .filter(|c| c.maintenance_enabled && !invalid.contains_key(&c.id))
                        .map(|c| c.maintain_targets.len())
                        .sum();
                });
                for config in desired {
                    if invalid.contains_key(&config.id)
                        || keep.contains(&config.id)
                        || (!config.sync_enabled && !config.maintenance_enabled)
                    {
                        continue;
                    }
                    let (cancel, cancel_rx) = watch::channel(false);
                    let worker_config = config.clone();
                    let state = status.clone();
                    let metrics = reports.clone();
                    let owner_dir = ownership_dir.clone();
                    let result = thread::Builder::new()
                        .name(format!("erlkonig-pile-{}", config.id))
                        .spawn(move || {
                            let result = managed_session(
                                &worker_config,
                                &owner_dir,
                                cancel_rx,
                                &state,
                                &metrics,
                            );
                            update_pile(&state, worker_config.id, |p| {
                                p.sync_running = false;
                                p.maintenance_running = false;
                                p.bound_sockets.clear();
                                if let Err(error) = &result {
                                    p.problem = Some(format!("{error:#}"));
                                }
                            });
                            result
                        });
                    match result {
                        Ok(worker) => tasks.push(Task {
                            config,
                            stop: cancel,
                            worker,
                        }),
                        Err(error) => update_pile(&status, config.id, |p| {
                            p.problem = Some(format!("cannot start session: {error}"))
                        }),
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                // A failed observation cannot authorize retaining the previous worker plan.
                for task in &tasks {
                    task.stop.send_replace(true);
                }
                for task in tasks.drain(..) {
                    join_task(task, &status);
                }
                update(&status, |s| {
                    s.config_problem = Some(format!("{error:#}; managed services stopped"))
                });
            }
        }
        let mut failure = false;
        for report in receiver.try_iter() {
            failure |= report.error.is_some();
            if report.error.is_some() {
                failures.push(report.clone());
            }
            pending.insert(
                (report.managed, report.session, report.role.clone()),
                report,
            );
        }
        if !pending.is_empty() && (failure || last_metrics.elapsed() >= Duration::from_secs(10)) {
            let mut batch: Vec<_> = pending.drain().map(|(_, sample)| sample).collect();
            batch.append(&mut failures);
            let result = with_node(&node, |node| node.append_metrics(&batch));
            update(&status, |s| {
                s.metrics_problem = result.err().map(|e| format!("metrics persistence: {e:#}"))
            });
            last_metrics = Instant::now();
        }
        // Controller scheduling only. No detached retry/watch process exists.
        thread::sleep(Duration::from_millis(200));
    }
    for task in &tasks {
        task.stop.send_replace(true);
    }
    for task in tasks {
        join_task(task, &status);
    }
    for report in receiver.try_iter() {
        if report.error.is_some() {
            failures.push(report.clone());
        }
        pending.insert(
            (report.managed, report.session, report.role.clone()),
            report,
        );
    }
    let mut batch: Vec<_> = pending.into_values().collect();
    batch.append(&mut failures);
    if !batch.is_empty() {
        with_node(&node, |node| node.append_metrics(&batch))?;
    }
    Ok(())
}

fn sample(
    config: &ManagedPile,
    endpoint: ed25519_dalek::VerifyingKey,
    session: Id,
    role: &str,
    elapsed: Duration,
) -> Option<Sample> {
    Sample::now(Some(config.id), "native", role, session, elapsed)
        .ok()
        .map(|mut sample| {
            sample.endpoint = Some(endpoint);
            sample
        })
}

fn maintenance_observer<'a>(
    config: &'a ManagedPile,
    endpoint: ed25519_dalek::VerifyingKey,
    session: Id,
    status: &'a Arc<Mutex<Status>>,
    metrics: &'a mpsc::Sender<Sample>,
) -> impl FnMut(Event) + Send + 'a {
    let started = Instant::now();
    let mut published = 0u128;
    let mut failed = 0u128;
    move |event| match event {
        Event::HopFinished {
            publications,
            error,
            ..
        } => {
            let count = publications
                .merges
                .saturating_add(publications.derives)
                .saturating_add(publications.maps);
            published = published.saturating_add(count);
            failed = failed.saturating_add(u128::from(error.is_some()));
            update(status, |s| {
                s.maintenance_publications = s
                    .maintenance_publications
                    .saturating_add(count.min(u64::MAX as u128) as u64);
                if let Some(p) = s.managed.iter_mut().find(|p| p.id == config.id.to_string()) {
                    p.maintenance_publications = p
                        .maintenance_publications
                        .saturating_add(count.min(u64::MAX as u128) as u64);
                    p.problem = error.clone().map(|e| format!("Maintenance: {e}"));
                }
            });
            if let Some(mut observation) =
                sample(config, endpoint, session, "maintenance", started.elapsed())
            {
                observation.failed = Some(failed);
                observation.publications = Some(published);
                observation.error = error;
                let _ = metrics.send(observation);
            }
        }
        Event::Failure { error, .. } => {
            failed = failed.saturating_add(1);
            update_pile(status, config.id, |p| {
                p.problem = Some(format!("Maintenance: {error}"))
            });
            if let Some(mut observation) =
                sample(config, endpoint, session, "maintenance", started.elapsed())
            {
                observation.publications = Some(published);
                observation.failed = Some(failed);
                observation.error = Some(error);
                let _ = metrics.send(observation);
            }
        }
        _ => {}
    }
}

fn managed_session(
    config: &ManagedPile,
    ownership_dir: &Path,
    stop: watch::Receiver<bool>,
    status: &Arc<Mutex<Status>>,
    metrics: &mpsc::Sender<Sample>,
) -> Result<()> {
    let identity = file_identity(&config.pile)?;
    let signer = signing_key_file::load_existing(&config.key)?;
    // Advisory app ownership is separate from Pile's short-lived append locks.
    // Non-participating standalone daemons require an explicit ownership handoff.
    let _pile_lease =
        lease(&ownership_dir.join(format!("pile-{}-{}.lock", identity.0, identity.1)))?;
    let _key_lease = if config.sync_enabled {
        Some(lease(&ownership_dir.join(format!(
            "peer-{}.lock",
            hex::encode(signer.verifying_key().as_bytes())
        )))?)
    } else {
        None
    };
    let session = genid().id;
    update_pile(status, config.id, |p| {
        p.node_id = Some(hex::encode(signer.verifying_key().as_bytes()))
    });
    let started = Instant::now();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let targets: Vec<_> = config
        .maintain_targets
        .iter()
        .copied()
        .map(Target::Handle)
        .collect();
    let store = faculties::storage::open_pile_strict_as(&config.pile, signer.verifying_key())?;
    // Reject a path exchanged while we were opening. Normal appenders are allowed.
    if file_identity(&config.pile)? != identity {
        bail!("pile file identity changed during activation");
    }
    let mut observer =
        maintenance_observer(config, signer.verifying_key(), session, status, metrics);
    let result = if config.sync_enabled {
        let peers = config
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
        .context("cannot start native sync host")?;
        peer.set_replication(
            ReplicationMode::Full,
            config.sync_collections.iter().copied(),
        );
        peer.activate_collections(config.sync_collections.iter().copied());
        let mut driver = if config.maintenance_enabled {
            Some(Driver::new(targets, true, SuccinctBackend::Cpu)?)
        } else {
            None
        };
        update_pile(status, config.id, |p| {
            p.sync_running = true;
            p.maintenance_running = config.maintenance_enabled;
            p.bound_sockets = peer
                .bound_sockets()
                .iter()
                .map(ToString::to_string)
                .collect();
            p.sessions_started += 1;
        });
        let mut previous_sent = 0u64;
        let mut received = 0u128;
        let driven: Result<()> = runtime.block_on(async {
            let stopped = stopping(stop); tokio::pin!(stopped);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    result = async {
                        peer.try_refresh()?;
                        let progress = peer.reconcile().await;
                        received = received.saturating_add(progress.received_bytes as u128);
                        let sent = peer.health().blob_serving.sent_bytes;
                        let sent_delta = sent.saturating_sub(previous_sent);
                        previous_sent = sent;
                        let delta = progress.received_bytes.saturating_add(sent_delta);
                        if delta > 0 {
                            update(status, |s| {
                                s.transfer_bytes = s.transfer_bytes.saturating_add(delta);
                                if let Some(p) = s.managed.iter_mut().find(|p| p.id == config.id.to_string()) { p.transfer_bytes = p.transfer_bytes.saturating_add(delta); }
                            });
                            if let Some(mut observation) = sample(config, signer.verifying_key(), session, "sync", started.elapsed()) {
                                observation.received_bytes = Some(received); observation.sent_bytes = Some(sent as u128);
                                let _ = metrics.send(observation);
                            }
                        }
                        if let Some(driver) = &mut driver { driver.tick(&mut peer, &signer, &mut observer).await?; }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        Ok::<(), anyhow::Error>(())
                    } => result?,
                }
            }
            Ok(())
        });
        drop(driver);
        let stopped = peer.shutdown_and_join().context("stop managed native host");
        let closed = peer.close().context("close managed pile");
        driven.and(stopped).and(closed)
    } else {
        // Sync off means no serving, repair, discovery or network acquisition.
        let mut store = store;
        let mut driver = Driver::new(targets, true, SuccinctBackend::Cpu)?;
        update_pile(status, config.id, |p| {
            p.maintenance_running = true;
            p.sessions_started += 1;
        });
        let driven: Result<()> = runtime.block_on(async {
            let stopped = stopping(stop);
            tokio::pin!(stopped);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    result = async {
                        store.refresh()?;
                        driver.tick(&mut store, &signer, &mut observer).await?;
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        Ok::<(), anyhow::Error>(())
                    } => result?,
                }
            }
            Ok(())
        });
        drop(driver);
        driven.and(store.close().context("close local maintenance pile"))
    };
    if let Some(mut observation) = sample(
        config,
        signer.verifying_key(),
        session,
        "session",
        started.elapsed(),
    ) {
        observation.failed = Some(if result.is_err() { 1 } else { 0 });
        observation.error = result.as_ref().err().map(|e| format!("{e:#}"));
        let _ = metrics.send(observation);
    }
    result
}
