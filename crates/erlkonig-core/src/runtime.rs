//! One node identity, one logical store and one app-owned network endpoint.

use crate::{
    active_work::ActiveWork,
    config::NodeBinding,
    config_mcp::ConfigMcp,
    credentials, init,
    node_metrics::{Sample, ServiceCode, ServiceEvent, ServicePhase},
    node_store::{ConfigView, NodeSettings, NodeStore},
    paths::{Layout, ensure_private_dir},
    status::{CleanupFailure, NodeStatus, ServiceState, Status},
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
use triblespace_net::peer::{Peer, PeerConfig};
use triblespace_net::reconcile::ReplicationMode;

pub(crate) type SharedNode = Arc<Mutex<Option<NodeStore>>>;
type ResourceCheck = Arc<dyn Fn() -> Result<()> + Send + Sync>;

pub struct Runtime {
    binding: NodeBinding,
    status: Arc<Mutex<Status>>,
    activity: ActiveWork,
    node: SharedNode,
    stop: watch::Sender<bool>,
    services: ServiceControl,
    secret_entry: crate::secret_entry::SecretEntry,
    workers: Vec<JoinHandle<Result<()>>>,
    lock: Option<File>,
}

fn update(status: &Arc<Mutex<Status>>, f: impl FnOnce(&mut Status)) {
    f(&mut status.lock().unwrap_or_else(|e| e.into_inner()));
}

#[derive(Clone)]
pub(crate) struct ServiceControl {
    pub(crate) status: Arc<Mutex<Status>>,
    stop: watch::Sender<bool>,
    retry: mpsc::SyncSender<()>,
}

impl ServiceControl {
    pub(crate) fn new(
        status: Arc<Mutex<Status>>,
        stop: watch::Sender<bool>,
    ) -> (Self, mpsc::Receiver<()>) {
        let (retry, requests) = mpsc::sync_channel(1);
        (Self { status, stop, retry }, requests)
    }

    pub(crate) fn retry(&self) -> Result<bool> {
        if *self.stop.borrow() {
            bail!("application is shutting down");
        }
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if status.config_problem.is_some() {
            bail!("resolve the node configuration before retrying services");
        }
        if matches!(status.node.services, ServiceState::Running | ServiceState::Starting) {
            return Ok(false);
        }
        if status.node.retry_pending { return Ok(true); }
        if status.node.services != ServiceState::Stopped {
            bail!("services are not stopped; no retry was requested");
        }
        match self.retry.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(())) => {
                status.node.retry_pending = true;
                Ok(true)
            }
            Err(mpsc::TrySendError::Disconnected(())) => bail!("service controller is unavailable"),
        }
    }
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
        Self::start_with_guard(layout, Arc::new(crate::resource_guard::sample))
    }

    fn start_with_guard(layout: &Layout, guard: ResourceCheck) -> Result<Self> {
        ensure_private_dir(&layout.dir)?;
        let lock = lease(&layout.lock()).context(
            "another Erlkonig owner is using this data directory; stop it before starting this app",
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
        let port = node.config().and_then(|view| view.mcp_port())
            .unwrap_or_else(|_| NodeSettings::default().mcp_port);
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
        let (services, retries) = ServiceControl::new(status.clone(), stop.clone());
        let mut app = Self {
            binding,
            status,
            activity: ActiveWork::default(),
            node,
            stop,
            services,
            secret_entry: crate::secret_entry::SecretEntry::default(),
            workers: Vec::new(),
            lock: Some(lock),
        };
        let http_stop = app.stop.subscribe();
        let state = app.status.clone();
        let activity = app.activity.clone();
        let node = app.node.clone();
        let binding = app.binding.clone();
        let services = app.services.clone();
        let secret_entry = app.secret_entry.clone();
        let mcp_url = app.status().mcp_url.context("local MCP address")?;
        app.workers.push(
            thread::Builder::new()
                .name("erlkonig-faculties".into())
                .spawn(move || {
                    let result = (|| {
                        let config = ConfigMcp::new(binding.clone(), node.clone(), mcp_url, port, services);
                        let dispatch = WorkspaceDispatch::new(binding, move || {
                            with_node(&node, NodeStore::config)
                        })?.with_secret_entry(secret_entry);
                        dispatch.finish(endpoint.serve_until(
                            &[&activity.faculty(&dispatch), &activity.faculty(&config)],
                            stopping(http_stop),
                        ))
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
        let activity = app.activity.clone();
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
                        activity,
                        port,
                        retries,
                        guard,
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
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        status.active_work = self.activity.count();
        status
    }
    /// Request one guarded retry without editing persistent settings.
    pub fn retry_services(&self) -> Result<bool> {
        self.services.retry()
    }
    pub fn begin_shutdown(&self) {
        // Release a queued/visible local-entry waiter before the HTTP drain.
        self.secret_entry.close();
        self.stop.send_replace(true);
    }

    pub fn take_secret_prompt(&self) -> Option<crate::secret_entry::SecretPrompt> {
        self.secret_entry.take_prompt()
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
            eprintln!("Erlkonig shutdown: {error:#}");
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
    stop: watch::Sender<Option<StopCause>>,
    worker: JoinHandle<Result<()>>,
    attempt: Attempt,
}

/// One actual start attempt, including preflight refusals and early failures.
#[derive(Clone, Copy)]
struct Attempt {
    id: Id,
    started: Instant,
    endpoint: ed25519_dalek::VerifyingKey,
}

impl Attempt {
    fn new(endpoint: ed25519_dalek::VerifyingKey) -> Self {
        Self { id: genid().id, started: Instant::now(), endpoint }
    }

    fn event(&self, node: &SharedNode, status: &Arc<Mutex<Status>>,
        phase: ServicePhase, code: ServiceCode) {
        let result = ServiceEvent::now(self.endpoint, self.id, self.started.elapsed(), phase, code)
            .and_then(|event| with_node(node, |node| node.append_service_events(&[event])));
        if let Err(error) = result {
            // No recursive event, retry queue, or success-shaped placeholder.
            // Keep this independent of successful periodic counter writes.
            update(status, |s| s.service_events_problem = Some(format!("{error:#}")));
        }
    }
}

#[derive(Clone, Debug)]
struct StopCause { reason: String, code: ServiceCode }

impl StopCause {
    fn new(reason: &str, code: ServiceCode) -> Self { Self { reason: reason.into(), code } }
    fn guard(error: &anyhow::Error) -> Self {
        let code = match error.downcast_ref::<crate::resource_guard::Failure>() {
            // No platform samples RSS, so the RSS-specific code is never
            // truthful here; the generic code names a footprint stop.
            Some(crate::resource_guard::Failure::FootprintLimit) => ServiceCode::ResourceLimit,
            Some(crate::resource_guard::Failure::HeadroomLimit) => ServiceCode::ResourceHeadroomLimit,
            None => ServiceCode::ResourceObservationFailed,
        };
        Self { reason: format!("{error:#}"), code }
    }
    fn expected(&self) -> bool {
        matches!(self.code, ServiceCode::SettingsChanged | ServiceCode::ApplicationShutdown)
    }
}

fn record_stop(status: &mut NodeStatus, reason: String) {
    if status.stop_reason.as_ref().is_some_and(|old| old != &reason) {
        status.retry_stop_reason = Some(reason);
    } else {
        status.stop_reason = Some(reason);
    }
}

impl Task {
    fn cancel(&self, cause: StopCause, status: &Arc<Mutex<Status>>, node: &SharedNode) {
        // First cancellation owns the explanation, even if shutdown or a later
        // guard sample arrives while the endpoint is draining.
        let first = self.stop.send_if_modified(|value| {
            if value.is_some() { return false; }
            *value = Some(cause.clone());
            true
        });
        if first && !cause.expected() {
            self.attempt.event(node, status, ServicePhase::Stop, cause.code);
        }
        update(status, |s| {
            s.node.services = ServiceState::Stopping;
            if let Some(cause) = self.stop.borrow().clone() {
                record_stop(&mut s.node, cause.reason);
            }
        });
    }
}

fn join_task(task: Task, status: &Arc<Mutex<Status>>, node: &SharedNode) {
    let error = match task.worker.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("{error:#}")),
        Err(_) => {
            task.attempt.event(node, status, ServicePhase::Session, ServiceCode::WorkerPanicked);
            Some("node worker panicked".into())
        }
    };
    update(status, |s| {
        s.node.sync_running = false;
        s.node.maintenance_running = false;
        s.node.bound_sockets.clear();
        s.node.services = ServiceState::Stopped;
        if let Some(cause) = task.stop.borrow().clone() {
            record_stop(&mut s.node, cause.reason);
        }
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
    activity: ActiveWork,
    initial_port: u16,
    retries: mpsc::Receiver<()>,
    guard: ResourceCheck,
) -> Result<()> {
    let (reports, receiver) = mpsc::channel();
    let mut task: Option<Task> = None;
    // The last interpreted service settings are independent of task liveness.
    // Reaping a finished handle must not turn unrelated Config edits into retries.
    let mut settings: Option<NodeSettings> = None;
    let mut pending: HashMap<(Id, String), Sample> = HashMap::new();
    let mut failures = Vec::new();
    let mut last_metrics = Instant::now();
    let mut last_guard = Instant::now() - Duration::from_secs(1);
    while !*stop.borrow() {
        if task.as_ref().is_some_and(|task| task.worker.is_finished()) {
            join_task(task.take().unwrap(), &status, &node);
        }
        if last_guard.elapsed() >= Duration::from_secs(1) {
            if let Err(error) = guard() {
                if let Some(task) = &task {
                    task.cancel(StopCause::guard(&error), &status, &node);
                }
            }
            last_guard = Instant::now();
        }
        let mut start = false;
        match with_node(&node, |node| {
            node.poll_config()?.map(|view| {
                let settings = view.settings()?;
                node.initialize_workspace_sync()?;
                Ok(settings)
            }).transpose()
        }) {
            Ok(Some(next)) => {
                // Workspaces select views: changing their facts must not reopen
                // the node endpoint or alter what it is allowed to synchronize.
                let unchanged = settings.as_ref().is_some_and(|old| {
                    old.sync_enabled == next.sync_enabled
                        && old.maintenance_enabled == next.maintenance_enabled
                });
                if !unchanged {
                    if let Some(old) = task.take() {
                        old.cancel(StopCause::new("service settings changed", ServiceCode::SettingsChanged), &status, &node);
                        join_task(old, &status, &node);
                    }
                    start = true;
                }
                update(&status, |s| {
                    s.config_generation += 1;
                    s.config_problem = None;
                    s.node.sync_enabled = next.sync_enabled;
                    s.node.maintenance_enabled = next.maintenance_enabled;
                    s.warnings = if next.mcp_port != initial_port {
                        vec!["Restart Erlkonig to use the changed MCP port.".into()]
                    } else {
                        Vec::new()
                    };
                });
                settings = Some(next);
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(old) = task.take() {
                    old.cancel(StopCause::new("node configuration is unavailable", ServiceCode::ConfigurationUnavailable), &status, &node);
                    join_task(old, &status, &node);
                } else if status.lock().unwrap_or_else(|e| e.into_inner()).config_problem.is_none() {
                    Attempt::new(signer.verifying_key()).event(&node, &status,
                        ServicePhase::Startup, ServiceCode::ConfigurationUnavailable);
                }
                settings = None;
                update(&status, |s| {
                    s.config_problem = Some(format!("{error:#}; node services stopped"))
                });
            }
        }
        let retry = retries.try_iter().next().is_some();
        // A retry never replaces a live or still-draining task. Requests are
        // coalesced, and every attempted start samples the guard again.
        start |= retry && task.is_none();
        if start && !*stop.borrow() {
            if let Some(settings) = settings.as_ref() {
                let attempt = Attempt::new(signer.verifying_key());
                if !settings.sync_enabled && !settings.maintenance_enabled {
                    update(&status, |s| s.node.services = ServiceState::Disabled);
                } else if let Err(error) = guard() {
                    let cause = StopCause::guard(&error);
                    attempt.event(&node, &status, ServicePhase::Stop, cause.code);
                    update(&status, |s| {
                        s.node.services = ServiceState::Stopped;
                        record_stop(&mut s.node, cause.reason);
                    });
                } else if task.is_none() {
                    // Retain diagnostics until the replacement really starts.
                    update(&status, |s| {
                        s.node.services = ServiceState::Starting;
                    });
                    let (cancel, cancel_rx) = watch::channel(None);
                    let reason = cancel_rx.clone();
                    let worker_cancel = cancel.clone();
                    let check = guard.clone();
                    let worker_settings = settings.clone();
                    let worker_binding = binding.clone();
                    let worker_signer = signer.clone();
                    let lock = identity_lock.clone();
                    let state = status.clone();
                    let work = activity.clone();
                    let metrics = reports.clone();
                    let shared = node.clone();
                    let worker =
                        thread::Builder::new()
                            .name("erlkonig-node".into())
                            .spawn(move || {
                                let mut entered = false;
                                let mut guard_failed = false;
                                let result = (|| {
                                    if let Err(error) = check() {
                                        let cause = StopCause::guard(&error);
                                        worker_cancel.send_if_modified(|reason| {
                                            if reason.is_some() { return false; }
                                            *reason = Some(cause.clone());
                                            true
                                        });
                                        attempt.event(&shared, &state, ServicePhase::Startup, cause.code);
                                        guard_failed = true;
                                        return Err(error);
                                    }
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
                                            shared.clone(),
                                            cancel_rx,
                                            &state,
                                            &work,
                                            &metrics,
                                            attempt,
                                            &mut entered,
                                        )
                                    })
                                })();
                                if result.is_err() && !entered && !guard_failed {
                                    attempt.event(&shared, &state, ServicePhase::Startup, ServiceCode::StartupFailed);
                                }
                                update(&state, |s| {
                                    s.node.sync_running = false;
                                    s.node.maintenance_running = false;
                                    s.node.bound_sockets.clear();
                                    // Only the controller can mark Stopped,
                                    // after joining this thread and its owners.
                                    s.node.services = ServiceState::Stopping;
                                    if let Some(cause) = reason.borrow().clone() {
                                        record_stop(&mut s.node, cause.reason);
                                    }
                                    if let Err(error) = &result {
                                        s.node.problem = Some(format!("{error:#}"));
                                    }
                                });
                                result
                            }).map_err(|error| {
                                attempt.event(&node, &status, ServicePhase::Startup, ServiceCode::StartupFailed);
                                error
                            })?;
                    task = Some(Task {
                        stop: cancel,
                        worker,
                        attempt,
                    });
                }
            }
        }
        if retry {
            update(&status, |s| s.node.retry_pending = false);
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
        old.cancel(StopCause::new("application is shutting down", ServiceCode::ApplicationShutdown), &status, &node);
        join_task(old, &status, &node);
    }
    update(&status, |s| s.node.retry_pending = false);
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
    activity: &'a ActiveWork,
    metrics: &'a mpsc::Sender<Sample>,
) -> impl FnMut(Event) + Send + 'a {
    let started = Instant::now();
    let mut published = 0u128;
    let mut failed = 0u128;
    // Driver hops are sequential, and its RAII observer emits HopCancelled
    // when an in-flight tick is dropped. No-op ticks emit no HopStarted.
    let mut work = None;
    move |event| {
        match &event {
            Event::HopStarted { .. } => work = Some(activity.enter()),
            Event::HopFinished { .. } | Event::HopCancelled { .. } => {
                work.take();
            }
            _ => {}
        }
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
    stop: watch::Receiver<Option<StopCause>>,
    status: &Arc<Mutex<Status>>,
    activity: &ActiveWork,
    metrics: &mpsc::Sender<Sample>,
    attempt: Attempt,
    entered: &mut bool,
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
    let session = attempt.id;
    let started = attempt.started;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let store = faculties::storage::open_pile_strict_as(&binding.pile, signer.verifying_key())?;
    let mut observer = maintenance_observer(signer.verifying_key(), session, status, activity, metrics);
    let result = if settings.sync_enabled {
        let mut peer = Peer::new(
            store,
            signer.clone(),
            PeerConfig {
                walk_algorithm: triblespace_net::walk_stream::Algorithm::default(),
                // This app owns the daemon, rather than dialing another one.
                daemon: None,
                provider_publication_budget: None,
                bind: None,
            },
        )
        .context("cannot start native node endpoint")?;
        *entered = true;
        let mut driver = None;
        let mut selection = AllAccessible::default();
        let mut previous_snapshot = None;
        let mut previous_sent = 0u64;
        let mut received = 0u128;
        let mut recorded_addresses = None;
        update(status, |s| {
            s.node.services = if stop.borrow().is_some() {
                ServiceState::Stopping
            } else {
                s.node.problem = None;
                s.node.stop_reason = None;
                s.node.retry_stop_reason = None;
                s.node.cleanup_failures.clear();
                ServiceState::Running
            };
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
            let stopped = session_stopping(stop.clone()); tokio::pin!(stopped);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut stopped => break,
                    result = async {
                        let bound = peer.bound_sockets();
                        if recorded_addresses.as_ref() != Some(&bound) {
                            with_node(&node, |node| node.record_sync_addresses(&bound))?;
                            update(status, |s| s.node.bound_sockets = bound.iter().map(ToString::to_string).collect());
                            recorded_addresses = Some(bound);
                        }
                        peer.try_refresh()?;
                        let snapshot = peer.snapshot()?;
                        if previous_snapshot.as_ref().is_none_or(|previous| snapshot.changes_since(previous) != StoreChanges::NONE) {
                            let roots = with_node(&node, NodeStore::workspace_roots)?;
                            let mut next = AllAccessible::discover(&snapshot, signer.verifying_key(), roots)?;
                            // Maintenance remains independently all-accessible.
                            // Only the pile's native Selected relation peers and
                            // hydrates in Full mode; grants alone select nothing.
                            next.sync = crate::workspace::selected_sync_roots(&snapshot, signer.verifying_key())?;
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
                        let progress = activity.during(peer.reconcile()).await;
                        received += progress.received_bytes as u128;
                        let health = peer.health();
                        let sent = health.blob_serving.sent_bytes;
                        let waiting = health.available.iter().map(|root| format!("blake3:{}", hex::encode(root.raw))).collect::<Vec<_>>();
                        update(status, |s| if s.node.grants_waiting != waiting { s.node.grants_waiting = waiting; });
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
        let closed = peer.close().context("close owned node handle");
        session_result(driven, [
            (CleanupFailure::EndpointShutdownFailed, stopped),
            (CleanupFailure::StoreCloseFailed, closed),
        ], status, &node, attempt, stop.borrow().as_ref())
    } else {
        let mut store = store;
        let mut driver = None;
        let mut selection = AllAccessible::default();
        let mut previous_snapshot = None;
        *entered = true;
        update(status, |s| {
            s.node.services = if stop.borrow().is_some() {
                ServiceState::Stopping
            } else {
                s.node.problem = None;
                s.node.stop_reason = None;
                s.node.retry_stop_reason = None;
                s.node.cleanup_failures.clear();
                ServiceState::Running
            };
            s.node.maintenance_running = true;
            s.node.sessions_started += 1;
        });
        let driven: Result<()> = runtime.block_on(async {
            let stopped = session_stopping(stop.clone()); tokio::pin!(stopped);
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
        session_result(driven, [(CleanupFailure::StoreCloseFailed,
            store.close().context("close local maintenance handle"))], status, &node, attempt, stop.borrow().as_ref())
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

async fn session_stopping(mut stop: watch::Receiver<Option<StopCause>>) {
    if stop.borrow().is_none() {
        let _ = stop.changed().await;
    }
}

fn session_result(
    driven: Result<()>,
    cleanup: impl IntoIterator<Item = (CleanupFailure, Result<()>)>,
    status: &Arc<Mutex<Status>>,
    node: &SharedNode,
    attempt: Attempt,
    cause: Option<&StopCause>,
) -> Result<()> {
    if driven.is_err() {
        attempt.event(node, status, ServicePhase::Session, ServiceCode::SessionFailed);
    }
    let mut failures: Vec<_> = driven.err().map(|error| format!("{error:#}")).into_iter().collect();
    let mut failed_cleanup = false;
    for (mut kind, result) in cleanup {
        if let Err(error) = result {
            // This dependency currently exposes an untyped timeout. Recognize
            // only its exact public literal; every other error stays generic.
            if kind == CleanupFailure::EndpointShutdownFailed && error.chain().any(|cause|
                cause.to_string() == "network transport shutdown exceeded 5 seconds") {
                kind = CleanupFailure::EndpointShutdownTimeout;
            }
            update(status, |s| s.node.cleanup_failures.push(kind));
            let code = match kind {
                CleanupFailure::EndpointShutdownFailed => ServiceCode::EndpointShutdownFailed,
                CleanupFailure::EndpointShutdownTimeout => ServiceCode::EndpointShutdownTimeout,
                CleanupFailure::StoreCloseFailed => ServiceCode::StoreCloseFailed,
            };
            attempt.event(node, status, ServicePhase::Cleanup, code);
            failed_cleanup = true;
            failures.push(format!("{error:#}"));
        }
    }
    if failed_cleanup && let Some(cause) = cause.filter(|cause| cause.expected()) {
        // Normal Settings/Quit cancellation is not an error by itself. Retain
        // its cause only when a cleanup failure makes this stop diagnostic.
        attempt.event(node, status, ServicePhase::Stop, cause.code);
    }
    if failures.is_empty() { Ok(()) } else { bail!(failures.join("; ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_stop_uses_the_platform_memory_vocabulary() {
        let cause = StopCause::guard(&crate::resource_guard::check(48 * 1024 * 1024 * 1024, 100)
            .unwrap_err());
        // Neither platform samples RSS: Darwin reports the physical footprint,
        // Linux anonymous, shared and swapped pages. The vocabulary must not
        // claim otherwise on either.
        assert_eq!(cause.code, ServiceCode::ResourceLimit);
        assert!(cause.reason.contains("footprint reached 48 GiB"));
        assert!(!cause.reason.contains("RSS"));
        assert!(!cause.code.description().contains("RSS"));
        let headroom = StopCause::guard(&crate::resource_guard::check(1024, 19).unwrap_err());
        assert_eq!(headroom.code, ServiceCode::ResourceHeadroomLimit);
        let invalid = StopCause::guard(&crate::resource_guard::check(1024, 101).unwrap_err());
        assert_eq!(invalid.code, ServiceCode::ResourceObservationFailed);
    }

    fn wait_for(app: &Runtime, predicate: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let status = app.status();
            if predicate(&status) { return status; }
            assert!(Instant::now() < deadline, "timed out: {status:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn cancellation_keeps_first_reason_and_every_cleanup_failure_after_join() {
        use triblespace::core::{collection::CollectionSnapshotExt, metadata, repo::BlobStoreGet};
        use triblespace::prelude::*;
        use triblespace_net::{health_record, telemetry};
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = crate::config::Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let node = Arc::new(Mutex::new(Some(NodeStore::open(&bootstrap.pile, signer.clone()).unwrap())));
        let shared = node.clone();
        let attempt = Attempt::new(signer.verifying_key());
        let path = dir.path().join("endpoint.lock");
        let owner = lease(&path).unwrap();
        let status = Arc::new(Mutex::new(Status::default()));
        let state = status.clone();
        let (stop, mut cancelled) = watch::channel(None);
        let worker = thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async { cancelled.changed().await.unwrap(); });
            let result = session_result(Err(anyhow::anyhow!("drive failed")), [
                (CleanupFailure::EndpointShutdownFailed,
                    Err(anyhow::anyhow!("network transport shutdown exceeded 5 seconds"))
                        .context("stop owned native endpoint")),
                (CleanupFailure::StoreCloseFailed, Err(anyhow::anyhow!("close failed"))),
            ], &state, &shared, attempt, cancelled.borrow().as_ref());
            drop(owner);
            result
        });
        let task = Task { stop, worker, attempt };
        let cause = StopCause::guard(&crate::resource_guard::check(48 * 1024 * 1024 * 1024, 100)
            .unwrap_err());
        let reason = cause.reason.clone();
        task.cancel(cause, &status, &node);
        task.cancel(StopCause::new("application is shutting down", ServiceCode::ApplicationShutdown), &status, &node);
        join_task(task, &status, &node);
        let state = status.lock().unwrap();
        assert_eq!(state.node.services, ServiceState::Stopped);
        assert_eq!(state.node.stop_reason.as_deref(), Some(reason.as_str()));
        let failure = state.node.problem.as_ref().unwrap();
        for expected in ["drive failed", "stop owned native endpoint", "close failed"] {
            assert!(failure.contains(expected), "{failure}");
        }
        assert_eq!(state.node.cleanup_failures, [
            CleanupFailure::EndpointShutdownTimeout, CleanupFailure::StoreCloseFailed,
        ]);
        assert!(lease(&path).is_ok(), "joined owner released the endpoint lease");
        drop(state);
        let node = node.lock().unwrap().take().unwrap();
        node.close().unwrap();
        let mut node = NodeStore::open(&bootstrap.pile, signer).unwrap();
        let snapshot = node.pile.snapshot().unwrap();
        let observed = snapshot.collection_acquiring(node.metrics).unwrap();
        let facts: TribleSet = observed.view().unwrap();
        let mut events = find!((phase: Inline<inlineencodings::Handle<blobencodings::UTF8String>>, code: Inline<inlineencodings::Handle<blobencodings::UTF8String>>),
            pattern!(&facts, [
                { metadata::tag: crate::node_metrics::KIND_SERVICE_EVENT,
                    health_record::attrs::session: attempt.id,
                    metadata::name: ?code, telemetry::attrs::subject: _?subject },
                { _?subject @ telemetry::attrs::stage: ?phase },
            ]))
            .map(|(phase, code)| {
                let phase: View<str> = snapshot.get(phase).unwrap();
                let text: View<str> = snapshot.get(code).unwrap();
                (phase.to_string(), text.to_string())
            })
            .collect::<Vec<_>>();
        events.sort_unstable();
        assert_eq!(events, vec![
            ("cleanup".into(), "endpoint_shutdown_timeout".into()),
            ("cleanup".into(), "store_close_failed".into()),
            ("session".into(), "session_failed".into()),
            ("stop".into(), "resource_limit".into()),
        ]);
        drop(observed);
        drop(snapshot);
        node.close().unwrap();
    }

    #[test]
    fn expected_stops_are_only_persisted_when_cleanup_fails() {
        use triblespace::core::{collection::CollectionSnapshotExt, metadata, repo::BlobStoreGet};
        use triblespace::prelude::*;
        use triblespace_net::health_record;
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = crate::config::Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let node = Arc::new(Mutex::new(Some(NodeStore::open(&bootstrap.pile, signer.clone()).unwrap())));
        let status = Arc::new(Mutex::new(Status::default()));
        for code in [ServiceCode::SettingsChanged, ServiceCode::ApplicationShutdown] {
            for failed in [false, true] {
                let attempt = Attempt::new(signer.verifying_key());
                let cause = StopCause::new(code.description(), code);
                let cleanup = if failed { Err(anyhow::anyhow!("private path must remain local")) } else { Ok(()) };
                assert_eq!(session_result(Ok(()), [(CleanupFailure::StoreCloseFailed, cleanup)],
                    &status, &node, attempt, Some(&cause)).is_err(), failed);
                with_node(&node, |node| {
                    let snapshot = node.pile.snapshot()?;
                    let observed = snapshot.collection_acquiring(node.metrics)?;
                    let facts: TribleSet = observed.view()?;
                    let mut codes = find!(value: Inline<inlineencodings::Handle<blobencodings::UTF8String>>,
                        pattern!(&facts, [{ metadata::tag: crate::node_metrics::KIND_SERVICE_EVENT,
                            health_record::attrs::session: attempt.id, metadata::name: ?value }]))
                        .map(|value| {
                            let text: View<str> = snapshot.get(value).unwrap();
                            text.to_string()
                        }).collect::<Vec<_>>();
                    codes.sort_unstable();
                    let mut expected = if failed { vec![code.as_str().to_string(), "store_close_failed".into()] } else { vec![] };
                    expected.sort_unstable();
                    assert_eq!(codes, expected);
                    Ok(())
                }).unwrap();
            }
        }
        node.lock().unwrap().take().unwrap().close().unwrap();
    }

    #[test]
    fn telemetry_write_failure_is_independent_visible_and_not_recursive() {
        let status = Arc::new(Mutex::new(Status::default()));
        let closed = Arc::new(Mutex::new(None));
        let signer = ed25519_dalek::SigningKey::from_bytes(&[89; 32]);
        Attempt::new(signer.verifying_key()).event(&closed, &status,
            ServicePhase::Startup, ServiceCode::StartupFailed);
        // This is the exact successful-counter update; it must not claim that
        // the failed service event was saved or silently retry it.
        update(&status, |s| s.metrics_problem = None);
        let status = status.lock().unwrap();
        assert_eq!(status.attention_label(), Some("Attention needed"));
        assert!(status.service_events_problem.as_ref().unwrap().contains("node store is closed"));
        assert!(status.summary().contains("service diagnostics could not be saved"));
        assert!(status.node.problem.is_none());
    }

    #[test]
    fn early_startup_failure_is_persisted_before_any_session_is_ready() {
        use triblespace::core::{collection::CollectionSnapshotExt, metadata, repo::BlobStoreGet};
        use triblespace::prelude::*;
        use triblespace_net::{health_record, telemetry};
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().join("data"));
        std::fs::create_dir(&layout.dir).unwrap();
        let signer = init::prepare(&layout.bootstrap()).unwrap();
        let mut node = NodeStore::open(&layout.default_pile(), signer.clone()).unwrap();
        let view = node.config().unwrap();
        node.put_settings(&view, &NodeSettings { mcp_port: 0,
            sync_enabled: true, maintenance_enabled: false }).unwrap();
        node.close().unwrap();
        let identity = std::env::temp_dir().join(format!("erlkonig-endpoint-{}.lock", hex::encode(signer.verifying_key().as_bytes())));
        let competing = lease(&identity).unwrap();
        let mut app = Runtime::start_with_guard(&layout, Arc::new(|| Ok(()))).unwrap();
        let stopped = wait_for(&app, |s| s.node.services == ServiceState::Stopped);
        assert_eq!(stopped.node.sessions_started, 0);
        assert!(stopped.node.problem.is_some());
        let attempt = with_node(&app.node, |node| {
            let snapshot = node.pile.snapshot()?;
            let observed = snapshot.collection_acquiring(node.metrics)?;
            let facts: TribleSet = observed.view()?;
            let mut events = find!((attempt: Id, code: Inline<inlineencodings::Handle<blobencodings::UTF8String>>),
                pattern!(&facts, [
                    { metadata::tag: crate::node_metrics::KIND_SERVICE_EVENT,
                        health_record::attrs::session: ?attempt,
                        metadata::name: ?code, telemetry::attrs::subject: _?subject },
                    { _?subject @ telemetry::attrs::stage: "startup" },
                ]))
                .map(|(attempt, code)| {
                    let text: View<str> = snapshot.get(code).unwrap();
                    (attempt, text.to_string())
                }).collect::<Vec<_>>();
            assert_eq!(events.len(), 1);
            let (attempt, code) = events.pop().unwrap();
            assert_eq!(code, "startup_failed");
            Ok(attempt)
        }).unwrap();
        drop(competing);
        app.retry_services().unwrap();
        wait_for(&app, |s| s.node.services == ServiceState::Running);
        app.finish().unwrap();
        // A fresh successful attempt has its own session identity; it cannot
        // overwrite or be mistaken for the preceding startup failure.
        let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
        let reports = node.metrics(crate::node_metrics::now_tai_ns().unwrap(), Duration::from_secs(60)).unwrap();
        assert!(reports.iter().any(|report| report.report.session != attempt));
        node.close().unwrap();
    }

    #[test]
    fn failed_retry_startup_preserves_prior_cause_and_reports_its_own_stop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().join("data"));
        std::fs::create_dir(&layout.dir).unwrap();
        let signer = init::prepare(&layout.bootstrap()).unwrap();
        let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
        let view = node.config().unwrap();
        node.put_settings(&view, &NodeSettings { mcp_port: 0,
            sync_enabled: true, maintenance_enabled: false }).unwrap();
        node.close().unwrap();
        let mode = Arc::new(AtomicUsize::new(0));
        let guard = {
            let mode = mode.clone();
            Arc::new(move || match mode.load(Ordering::SeqCst) {
                0 => crate::resource_guard::check(48 * 1024 * 1024 * 1024, 100),
                1 if thread::current().name() == Some("erlkonig-node") => {
                    crate::resource_guard::check(0, 19)
                }
                3 => bail!("service resource guard cannot sample process memory"),
                _ => Ok(()),
            })
        };
        let mut app = Runtime::start_with_guard(&layout, guard).unwrap();
        let stopped = wait_for(&app, |s| s.node.services == ServiceState::Stopped);
        let original = stopped.node.stop_reason.unwrap();
        assert!(original.contains("48 GiB"));
        mode.store(1, Ordering::SeqCst);
        assert!(app.retry_services().unwrap());
        let failed = wait_for(&app, |s| s.node.services == ServiceState::Stopped
            && !s.node.retry_pending && s.node.problem.is_some());
        assert_eq!(failed.node.stop_reason.as_deref(), Some(original.as_str()));
        assert!(failed.node.retry_stop_reason.unwrap().contains("headroom below 20%"));
        assert_eq!(failed.node.sessions_started, 0);
        mode.store(2, Ordering::SeqCst);
        app.retry_services().unwrap();
        let running = wait_for(&app, |s| s.node.services == ServiceState::Running);
        assert!(running.node.stop_reason.is_none());
        assert!(running.node.retry_stop_reason.is_none());
        mode.store(3, Ordering::SeqCst);
        let stopped = wait_for(&app, |s| s.node.services == ServiceState::Stopped);
        assert!(stopped.node.stop_reason.unwrap().contains("cannot sample process memory"));
        // Quit wins over a request queued while the controller awaits config.
        let node = app.node.lock().unwrap();
        mode.store(2, Ordering::SeqCst);
        app.retry_services().unwrap();
        app.begin_shutdown();
        drop(node);
        app.finish().unwrap();
        assert_eq!(app.status().node.sessions_started, 1);
        assert!(!app.status().node.retry_pending);
    }

    #[test]
    fn guarded_retry_reaps_owners_and_never_changes_settings_or_retries_itself() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().join("data"));
        std::fs::create_dir(&layout.dir).unwrap();
        let signer = init::prepare(&layout.bootstrap()).unwrap();
        let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
        let view = node.config().unwrap();
        node.put_settings(&view, &NodeSettings { mcp_port: 0,
            sync_enabled: true, maintenance_enabled: false }).unwrap();
        node.close().unwrap();
        let pressure = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(AtomicUsize::new(0));
        let guard = {
            let pressure = pressure.clone();
            let samples = samples.clone();
            Arc::new(move || {
                samples.fetch_add(1, Ordering::SeqCst);
                crate::resource_guard::check(0, if pressure.load(Ordering::SeqCst) { 19 } else { 100 })
            })
        };
        let mut app = Runtime::start_with_guard(&layout, guard).unwrap();
        wait_for(&app, |s| s.node.services == ServiceState::Running);
        assert!(!app.retry_services().unwrap(), "running retry is an idempotent no-op");
        let observation = app.config().unwrap().observation().unwrap();
        pressure.store(true, Ordering::SeqCst);
        let stopped = wait_for(&app, |s| s.node.services == ServiceState::Stopped);
        assert!(!stopped.node.sync_running);
        assert!(stopped.node.bound_sockets.is_empty());
        assert_eq!(stopped.node.sessions_started, 1);
        assert!(stopped.node.stop_reason.unwrap().contains("headroom below 20%"));
        assert!(app.status().mcp_url.is_some());
        let before = samples.load(Ordering::SeqCst);
        assert!(app.retry_services().unwrap());
        wait_for(&app, |s| !s.node.retry_pending && samples.load(Ordering::SeqCst) > before);
        assert_eq!(app.status().node.sessions_started, 1, "pressure blocks retry");
        pressure.store(false, Ordering::SeqCst);
        let view = app.config().unwrap();
        let generation = app.status().config_generation;
        app.create_workspace(&view, "unrelated workspace edit").unwrap();
        wait_for(&app, |s| s.config_generation > generation);
        assert_eq!(app.status().node.services, ServiceState::Stopped);
        assert_eq!(app.status().node.sessions_started, 1);
        assert_ne!(app.config().unwrap().observation().unwrap(), observation);
        let before_retry = app.config().unwrap();
        let settings_before_retry = before_retry.settings().unwrap();
        let roots_before_retry = crate::workspace::selected_sync_roots(
            before_retry.observed.snapshot(),
            ed25519_dalek::VerifyingKey::from_bytes(&app.binding().signer).unwrap(),
        ).unwrap();
        assert!(app.retry_services().unwrap());
        // Repeated queued requests coalesce; they cannot replace a live owner.
        let _ = app.retry_services().unwrap();
        let restarted = wait_for(&app, |s| s.node.services == ServiceState::Running);
        assert_eq!(restarted.node.sessions_started, 2);
        assert!(restarted.node.stop_reason.is_none());
        let after_retry = app.config().unwrap();
        assert_eq!(after_retry.settings().unwrap(), settings_before_retry);
        assert_eq!(after_retry.workspace_ids().unwrap(), before_retry.workspace_ids().unwrap());
        assert_eq!(crate::workspace::selected_sync_roots(
            after_retry.observed.snapshot(),
            ed25519_dalek::VerifyingKey::from_bytes(&app.binding().signer).unwrap(),
        ).unwrap(), roots_before_retry);
        // Rebinding publishes new socket hints in the same Config collection;
        // the whole observation may change without changing user settings.
        assert!(Runtime::start(&layout).is_err());
        pressure.store(true, Ordering::SeqCst);
        wait_for(&app, |s| s.node.services == ServiceState::Stopped);
        // Admit a retry and a settings stop under the same node lock. The
        // controller must use the new settings, not the old stopped task's.
        let mut node = app.node.lock().unwrap();
        pressure.store(false, Ordering::SeqCst);
        assert!(app.retry_services().unwrap());
        let node_store = node.as_mut().unwrap();
        let view = node_store.config().unwrap();
        let mut settings = view.settings().unwrap();
        settings.sync_enabled = false;
        node_store.put_settings(&view, &settings).unwrap();
        drop(node);
        let disabled = wait_for(&app, |s| s.node.services == ServiceState::Disabled && !s.node.retry_pending);
        assert_eq!(disabled.node.sessions_started, 2);
        app.begin_shutdown();
        assert!(app.retry_services().is_err());
        app.finish().unwrap();
        assert!(app.is_finished());
        let mut next = Runtime::start(&layout).unwrap();
        let view = next.config().unwrap();
        let mut settings = view.settings().unwrap();
        settings.sync_enabled = true;
        next.put_settings(&view, &settings).unwrap();
        wait_for(&next, |s| s.node.services == ServiceState::Running);
        next.finish().unwrap();
    }

    #[test]
    fn maintenance_boundaries_release_only_their_own_work() {
        use triblespace::core::collection::CollectionHandle;
        let signer = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let status = Arc::new(Mutex::new(Status::default()));
        let work = ActiveWork::default();
        let (metrics, _reports) = mpsc::channel();
        let mut observer = maintenance_observer(signer.verifying_key(), genid().id, &status, &work, &metrics);
        let collection = CollectionHandle::new([5; 32]);
        observer(Event::Failure { reference: "fixture".into(), error: "before hop".into() });
        assert_eq!(work.count(), 0, "no actual maintenance was entered");
        let reconciliation = work.enter();
        for error in [None, Some("synthetic maintenance failure".into())] {
            observer(Event::HopStarted { collection });
            assert_eq!(work.count(), 2);
            observer(Event::HopFinished {
                collection, elapsed: Duration::ZERO, census: None,
                publications: Default::default(), error,
            });
            assert_eq!(work.count(), 1);
            observer(Event::HopCancelled { collection, elapsed: Duration::ZERO });
            assert_eq!(work.count(), 1, "a duplicate ending never double-releases");
        }
        observer(Event::HopStarted { collection });
        observer(Event::HopCancelled { collection, elapsed: Duration::ZERO });
        assert_eq!(work.count(), 1);
        observer(Event::HopStarted { collection });
        drop(observer);
        assert_eq!(work.count(), 1, "observer teardown releases unfinished work");
        drop(reconciliation);
        assert_eq!(work.count(), 0);
        assert_eq!(status.lock().unwrap().transfer_bytes, 0, "activity invents no payload bytes");
    }

    #[test]
    fn real_local_maintenance_emits_work_and_idle_ticks_stay_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = crate::config::Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let mut node = NodeStore::open(&bootstrap.pile, signer.clone()).unwrap();
        let mut driver = Driver::new([Target::Handle(node.config.handle())], true, SuccinctBackend::Cpu).unwrap();
        let work = ActiveWork::default();
        let status = Arc::new(Mutex::new(Status::default()));
        let (metrics, _reports) = mpsc::channel();
        let mut observer = maintenance_observer(signer.verifying_key(), genid().id, &status, &work, &metrics);
        let mut starts = 0;
        let mut observed = |event: Event| {
            let started = matches!(event, Event::HopStarted { .. });
            observer(event);
            if started {
                starts += 1;
                assert_eq!(work.count(), 1);
            } else {
                assert_eq!(work.count(), 0);
            }
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            assert!(driver.tick(&mut node.pile, &signer, &mut observed).await.unwrap().is_some());
            for _ in 0..3 {
                driver.tick(&mut node.pile, &signer, &mut observed).await.unwrap();
            }
            assert!(driver.tick(&mut node.pile, &signer, &mut observed).await.unwrap().is_none());
        });
        assert!(starts > 0);
        assert_eq!(work.count(), 0);
        drop(observer);
        node.close().unwrap();
    }

    #[test]
    fn persisted_settings_conflicts_leave_authenticated_recovery_available() {
        use faculties::schemas::config::config;
        use triblespace::core::collection::CollectionStoreExt;
        use triblespace::prelude::*;
        use crate::node_schema as attrs;
        use serde_json::{Value, json};

        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path().join("data"));
        std::fs::create_dir(&layout.dir).unwrap();
        let signer = init::prepare(&layout.bootstrap()).unwrap();
        let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
        for sync in [true,false] {
            node.pile.commit(node.config,&node.signer,entity! {
                config::anchor:attrs::NODE_SETTINGS,attrs::mcp_port:0_u16,
                attrs::sync_enabled:sync,attrs::maintenance_enabled:false,
            }).unwrap();
        }
        node.close().unwrap();
        let mut app = Runtime::start(&layout).unwrap();
        let url = app.status().mcp_url.unwrap();
        let token = credentials::load(&layout.dir.join("mcp.token")).unwrap();
        let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)).build().unwrap();
        let response = client.post(&url).bearer_auth(&token)
            .header("Accept","application/json, text/event-stream")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"recovery-test","version":"1"}}}))
            .send().unwrap();
        assert!(response.status().is_success());
        let session = response.headers()["mcp-session-id"].to_str().unwrap().to_owned();
        assert!(client.post(&url).bearer_auth(&token)
            .header("Accept","application/json, text/event-stream")
            .header("mcp-session-id",&session).header("mcp-protocol-version","2025-06-18")
            .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send().unwrap().status().is_success());
        let request = |name: &str, arguments: Value| -> Value {
            let response: Value = client.post(&url).bearer_auth(&token)
                .header("Accept","application/json, text/event-stream")
                .header("mcp-session-id",&session).header("mcp-protocol-version","2025-06-18")
                .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}}))
                .send().unwrap().json().unwrap();
            assert_ne!(response["result"]["isError"],true,"{response}");
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
        };
        let inspected = request("config_inspect",json!({}));
        assert!(inspected["settings"].is_null());
        assert_eq!(inspected["mcp_url"],url);
        let deadline = Instant::now()+Duration::from_secs(5);
        while app.status().config_problem.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let status = app.status();
        assert!(status.config_problem.is_some());
        assert!(!status.node.sync_running && !status.node.maintenance_running);
        assert_eq!(status.node.sessions_started,0);
        let saved = request("config_set_settings",json!({"observation":inspected["observation"],
            "mcp_port":0,"sync_enabled":false,"maintenance_enabled":false}));
        assert_eq!(saved["port_restart_required"],false);
        assert_eq!(saved["mcp_url"],url);
        assert!(request("config_inspect",json!({}))["settings_problem"].is_null());
        app.finish().unwrap();
        let mut restarted = Runtime::start(&layout).unwrap();
        assert_eq!(restarted.config().unwrap().settings().unwrap().mcp_port,0);
        restarted.finish().unwrap();
    }

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
            let (cancel, stop) = watch::channel(None);
            let (metrics, _reports) = mpsc::channel();
            let activity = ActiveWork::default();

            // The controller deliberately started this task, then its guard
            // fired before synchronous setup published startup status. No
            // timing or actual memory pressure is needed to force this order.
            let cause = StopCause::guard(&crate::resource_guard::check(48 * 1024 * 1024 * 1024, 100)
                .unwrap_err());
            let reason = cause.reason.clone();
            cancel.send_replace(Some(cause));
            update(&status, |s| s.node.stop_reason = Some(reason.clone()));
            node_session(
                &binding,
                &signer,
                &settings,
                &dir.path().join("endpoint.lock"),
                node.clone(),
                stop,
                &status,
                &activity,
                &metrics,
                Attempt::new(signer.verifying_key()),
                &mut false,
            )
            .unwrap();

            let state = status.lock().unwrap();
            assert_eq!(state.node.sessions_started, 1);
            assert_eq!(
                state.node.stop_reason.as_deref(),
                Some(reason.as_str()),
                "startup must retain the guard explanation (sync={sync_enabled})"
            );
            assert!(cancel.borrow().is_some(), "startup must not undo cancellation");
            assert_eq!(activity.count(), 0, "stopped sessions never start a pass");
            drop(state);
            node.lock().unwrap().take().unwrap().close().unwrap();
        }
    }
}
