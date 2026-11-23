//! iOS-owned foreground replica. No desktop runtime, MCP, or ambient paths.
use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc, atomic::{AtomicU64, Ordering}},
    time::{Duration, Instant},
};
use triblespace_core::{
    collection::{
        CollectionHandle, CollectionRead, HeldRead,
        record_counts::RecordCountStatus,
        selection::{self, Selection},
    },
    prelude::{find, pattern},
    repo::pile::{Pile, PileSnapshot},
    capability::QuorumOutcome,
    repo::{CapabilityProofRead, SnapshotSource, StorageClose, StorageFlush, StoreChanges, StoreSnapshot},
    signing_key_file,
};
use triblespace_net::{
    peer::{Peer, PeerConfig},
    reconcile::ReplicationMode,
};

pub mod network;
#[cfg(target_os = "ios")]
mod application;
#[cfg(target_os = "ios")]
mod ui;
#[cfg(all(test, not(target_os = "ios")))]
#[path = "ui/host_tests.rs"]
mod ui_model_tests;
pub mod collections;
pub mod storage;
#[cfg(feature = "synthetic-proof")]
pub mod collections_proof;
#[cfg(feature = "synthetic-proof")]
pub mod proof;

/// Strictly local bootstrap; callers supply the application container explicitly.
/// Never repairs/truncates a malformed pile or replaces a missing identity.
fn prepare_local(root: &Path) -> Result<(SigningKey, PathBuf)> {
    if !root.is_absolute() {
        bail!("replica directory must be absolute");
    }
    fs::create_dir_all(root)?;
    let pile_path = root.join("replica.pile");
    let key_path = root.join("node.key");
    for path in [&pile_path, &key_path] {
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("replica files must not be symlinks");
        }
    }
    if fs::metadata(&pile_path).is_ok_and(|m| m.len() > 0) && !key_path.exists() {
        bail!("existing replica has no identity; refusing replacement");
    }
    let key = if key_path.exists() {
        signing_key_file::load_existing(&key_path)?
    } else {
        signing_key_file::init(&key_path)?
    };
    if !pile_path.exists() {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pile_path)?
            .sync_all()?;
        fs::File::open(root)?.sync_all()?;
    }
    Ok((key, pile_path))
}

pub fn open_local(root: &Path) -> Result<(SigningKey, Pile)> {
    let (key, pile_path) = prepare_local(root)?;
    let mut pile = Pile::open_as(&pile_path, key.verifying_key())?;
    if let Err(error) = pile.refresh() {
        let close = pile.close();
        bail!("replica replay failed: {error}; close: {close:?}");
    }
    Ok((key, pile))
}

/// Read the native selection register, including its conflict semantics.
/// Only explicit collection choices write this register; grants alone do not.
pub fn selected(pile: &mut Pile, key: &SigningKey) -> Result<Vec<CollectionHandle>> {
    selected_in(&pile.snapshot()?, key)
}
fn selected_in(snapshot: &PileSnapshot, key: &SigningKey) -> Result<Vec<CollectionHandle>> {
    let facts = selection::config_facts(snapshot, key.verifying_key())?;
    let mut roots: Vec<_> = find!(root: CollectionHandle,
        pattern!(&facts, [{ _?state @ selection::sync_collection: ?root }]))
    .filter(|root| selection::sync_selection(&facts, *root) == Selection::Selected)
    .collect();
    roots.sort();
    roots.dedup();
    Ok(roots)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayProgress {
    pub prefix_bytes: u64,
    pub observed_bytes: u64,
    pub activity_serial: u64,
    pub records_per_second: Option<ReplayRate>,
}
#[cfg(test)]
impl Serialize for ReplayProgress {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut fields = serializer.serialize_struct("ReplayProgress", 6)?;
        fields.serialize_field("prefix_bytes", &self.prefix_bytes)?;
        fields.serialize_field("observed_bytes", &self.observed_bytes)?;
        fields.serialize_field("activity_serial", &self.activity_serial)?;
        // Keep the original fresh-only field for existing clients.
        fields.serialize_field("records_per_second", &self.records_per_second)?;
        fields.serialize_field("last_records_per_second", &self.records_per_second.map(|rate| rate.records_per_second))?;
        fields.serialize_field("rate_age_ms", &self.records_per_second.map(|rate| rate.measured_at.elapsed().as_millis() as u64))?;
        fields.end()
    }
}
impl ReplayProgress {
    fn observe(snapshot: &PileSnapshot, observed_bytes: u64, activity_serial: u64) -> Self {
        Self { prefix_bytes: snapshot.prefix_len() as u64, observed_bytes, activity_serial, records_per_second: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayRate {
    pub records_per_second: u64,
    pub measured_at: Instant,
}

#[cfg(test)]
impl Serialize for ReplayRate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.measured_at.elapsed() > Duration::from_secs(5) {
            serializer.serialize_none()
        } else {
            serializer.serialize_u64(self.records_per_second)
        }
    }
}

#[derive(Default)]
struct ReplayMeter {
    baseline: Option<(Instant, u64)>,
    rate: Option<ReplayRate>,
}

impl ReplayMeter {
    fn observe(&mut self, now: Instant, records: u64) -> Option<ReplayRate> {
        if let Some((start, previous)) = self.baseline {
            if let Some(delta) = records.checked_sub(previous) {
                let elapsed = now.duration_since(start);
                if elapsed < Duration::from_secs(1) { return self.rate; }
                self.rate = Some(ReplayRate {
                    records_per_second: (delta as f64 / elapsed.as_secs_f64()).round() as u64,
                    measured_at: now,
                });
            } else {
                self.rate = None;
            }
        }
        self.baseline = Some((now, records));
        self.rate
    }
}

/// Current-source native count aggregate for admitted selected roots only.
/// Native replay maintains the index; this is only a snapshot projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SyncProgress {
    pub records: u64,
    pub direct_present: u64,
    pub unknown_roots: usize,
}

impl SyncProgress {
    fn observe(&mut self, status: RecordCountStatus) {
        if let RecordCountStatus::Ready { counts, .. } = status {
            if let (Some(records), Some(present)) = (self.records.checked_add(counts.records),
                self.direct_present.checked_add(counts.direct_present)) {
                self.records = records; self.direct_present = present;
                return;
            }
        }
        self.unknown_roots += 1;
    }
}
fn sync_progress(snapshot: &PileSnapshot, key: &SigningKey, selected: &[CollectionHandle]) -> Result<SyncProgress> {
    let proofs = snapshot.proofs()?.collect::<Result<Vec<_>, _>>()?;
    let mut progress = SyncProgress::default();
    for &root in selected {
        if root == selection::config_handle(key.verifying_key()) { continue; }
        if matches!(triblespace_core::collection::collection_reader_is_admitted_by(
            snapshot, root, key.verifying_key(), &proofs), Ok(QuorumOutcome::Met)) {
            progress.observe(snapshot.record_count_status(root));
        }
    }
    Ok(progress)
}

#[derive(Default, Clone, PartialEq)]
pub struct Status {
    pub revision: u64,
    origin: Arc<()>,
    pub node_id: String,
    pub running: bool,
    pub ready: bool,
    /// False means values describe a prefix; mutations still wait for EOF.
    pub replay_complete: bool,
    pub replay_progress: Option<ReplayProgress>,
    pub sync_progress: Option<SyncProgress>,
    /// Acknowledges activation after complete local observation, pause after checkpoint.
    pub activity_serial: u64,
    pub selected: usize,
    pub listening: bool,
    pub list_revision: u64,
    pub list_snapshot: Option<CollectionSnapshot>,
    #[cfg(test)]
    test_page_offset: usize,
    pub storage: Arc<Vec<storage::ByteObservation>>,
    pub composition: Option<Arc<storage::Composition>>,
    pub action_error: Option<String>,
    pub action_serial: u64,
    pub last_cleanup_error: Option<String>,
    pub network_started: bool,
    pub bound_sockets: usize,
    pub message: String,
    pub error: Option<String>,
}
enum Command {
    Active(bool, u64),
    LocalActive(u64),
    Discover,
    #[cfg(test)]
    Page(usize),
    Storage(Vec<CollectionHandle>),
    StorageActive(bool),
    Choose(CollectionHandle, String, bool),
    #[cfg(test)]
    StorageCheckpoint(Box<dyn FnOnce(&mut Peer<Pile>, bool, &Arc<Mutex<Status>>) + Send>),
    #[cfg(test)]
    OpeningCheckpoint(Box<dyn FnOnce(Option<&Pile>, bool, &Arc<Mutex<Status>>, &mut u64) + Send>),
    #[cfg(test)]
    BeforeForeground(Box<dyn FnOnce(&Pile, &Arc<Mutex<Status>>) + Send>),
    #[cfg(test)]
    BeforeFirstPeerTurn(Box<dyn FnOnce(&Arc<Mutex<Status>>) + Send>),
    #[cfg(test)]
    BeforePreparation(Box<dyn FnOnce(&Arc<Mutex<Status>>) + Send>),
    Stop,
}
pub struct Replica {
    command: mpsc::Sender<Command>,
    activity_serial: AtomicU64,
    peer: Arc<arc_swap::ArcSwapOption<network::Session>>,
    status: Arc<Mutex<Status>>,
}

#[derive(Clone)]
pub struct CollectionSnapshot {
    origin: Arc<()>,
    snapshot: Arc<PileSnapshot>,
    reader: ed25519_dalek::VerifyingKey,
    pub complete: bool,
    pub revision: u64,
}

impl CollectionSnapshot {
    pub fn same_replica(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.origin, &other.origin) && self.reader == other.reader
    }
}

impl PartialEq for CollectionSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.same_replica(other) && self.revision == other.revision
    }
}

impl Status {
    fn offer_snapshot(&mut self, snapshot: &PileSnapshot, reader: ed25519_dalek::VerifyingKey, complete: bool) {
        if self.list_snapshot.as_ref().is_some_and(|old| old.complete == complete && old.reader == reader
            && old.snapshot.prefix_len() == snapshot.prefix_len() && snapshot.changes_since(&old.snapshot).is_empty()) {
            return;
        }
        self.list_revision = self.list_revision.wrapping_add(1);
        self.list_snapshot = Some(CollectionSnapshot { origin: self.origin.clone(), snapshot: Arc::new(snapshot.clone()), reader, complete, revision: self.list_revision });
    }
}

/// One visible page and its native dependency read set. Each screen owns its reader.
#[derive(Default)]
pub struct CollectionPages {
    origin: Option<Arc<()>>,
    cached: Option<collections::CachedPage>,
}

impl CollectionPages {
    /// May traverse metadata on a cold/invalidated page. Call on a reader thread.
    /// The owned immutable snapshot stays valid across worker progress or release.
    pub fn read(&mut self, view: &CollectionSnapshot, offset: usize) -> Result<collections::Page> {
        if !self.origin.as_ref().is_some_and(|origin| Arc::ptr_eq(origin, &view.origin)) {
            self.cached = None;
            self.origin = Some(view.origin.clone());
        }
        collections::CachedPage::read_view(&mut self.cached, view, offset)
    }
}

fn publish(status: &Arc<Mutex<Status>>, f: impl FnOnce(&mut Status)) {
    if let Ok(mut status) = status.lock() {
        let previous = status.clone();
        f(&mut status);
        if *status != previous { status.revision = status.revision.wrapping_add(1); }
    }
}
// The retained native host reports this only after its runtime thread joined.
// No other shutdown failure is treated as evidence of a completed join.
const JOINED_TRANSPORT_TIMEOUT: &str = "network transport shutdown exceeded 5 seconds";
#[derive(Debug)]
struct CleanupFailure {
    shutdown: Option<String>,
    flush: Option<String>,
    close: Option<String>,
}
impl CleanupFailure {
    fn can_resume(&self) -> bool {
        self.shutdown.as_deref() == Some(JOINED_TRANSPORT_TIMEOUT)
            && self.flush.is_none() && self.close.is_none()
    }
}
impl std::fmt::Display for CleanupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut errors = Vec::new();
        if let Some(error) = &self.shutdown { errors.push(format!("network shutdown: {error}")); }
        if let Some(error) = &self.flush { errors.push(format!("durable checkpoint: {error}")); }
        if let Some(error) = &self.close { errors.push(format!("replica close: {error}")); }
        write!(f, "{}", errors.join("; "))
    }
}
impl std::error::Error for CleanupFailure {}

fn finish(peer: &mut Option<Peer<Pile>>) -> Result<()> {
    if let Some(mut peer) = peer.take() {
        // Attempt all three and retain all failures, not just the first one.
        let shutdown = peer.shutdown_and_join().err().map(|e| format!("{e:#}"));
        let flush = peer.flush().err().map(|e| format!("{e:#}"));
        let close = peer.close().err().map(|e| format!("{e:#}"));
        if shutdown.is_some() || flush.is_some() || close.is_some() {
            return Err(CleanupFailure { shutdown, flush, close }.into());
        }
    }
    Ok(())
}
// Pause releases only the network owner. The native pile (including a partial
// replay and its coverage/held indexes) belongs to this worker until final close.
fn pause(peer: &mut Option<Peer<Pile>>, local: &mut Option<Pile>) -> Result<()> {
    let shutdown = if let Some(mut current) = peer.take() {
        let shutdown = current.shutdown_and_join().err().map(|e| format!("{e:#}"));
        debug_assert!(local.is_none());
        *local = Some(current.into_store());
        shutdown
    } else { None };
    let flush = local.as_mut().and_then(|pile| pile.flush().err()).map(|e| format!("{e:#}"));
    if shutdown.is_some() || flush.is_some() {
        return Err(CleanupFailure { shutdown, flush, close: None }.into());
    }
    Ok(())
}

fn foreground_pile(mut pile: Pile, current_key: SigningKey) -> Result<Peer<Pile>> {
    let listening = collections::listening(&mut pile, &current_key)?;
    let roots = selected(&mut pile, &current_key)?;
    let config = PeerConfig {
        daemon: None,
        provider_publication_budget: None,
        bind: None,
    };
    let mut peer = if listening || !roots.is_empty() {
        Peer::new(pile, current_key, config).context("start replica endpoint")?
    } else {
        Peer::lazy(pile, current_key, config)
    };
    peer.set_replication(ReplicationMode::Full, roots.iter().copied());
    if !roots.is_empty() {
        peer.activate_collections(roots);
    }
    Ok(peer)
}

fn worker(
    root: PathBuf,
    commands: mpsc::Receiver<Command>,
    status: Arc<Mutex<Status>>,
    peer: Arc<arc_swap::ArcSwapOption<network::Session>>,
) {
    worker_with_cleanup(root, commands, status, peer, pause);
}

/// Coalesce visibility into the next sample without discarding a bounded,
/// pinned batch already in progress. Hiding Storage cancels its work.
fn update_storage_request(
    roots: &mut Vec<CollectionHandle>,
    requested: Vec<CollectionHandle>,
    sample: &mut Option<storage::IncrementalSample>,
) -> bool {
    if *roots == requested { return false; }
    *roots = requested;
    if roots.is_empty() { *sample = None; }
    true
}

// The injected pause boundary exercises recovery without opening a network
// endpoint or pretending the fixture reproduces a real transport timeout.
// Final close is independent, so a failed pause cannot skip owner cleanup.
fn worker_with_cleanup(
    root: PathBuf,
    commands: mpsc::Receiver<Command>,
    status: Arc<Mutex<Status>>,
    sessions: Arc<arc_swap::ArcSwapOption<network::Session>>,
    mut cleanup: impl FnMut(&mut Option<Peer<Pile>>, &mut Option<Pile>) -> Result<()>,
) {
    let result = (|| -> Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _runtime_context = runtime.enter();
        let (key, pile_path) = prepare_local(&root)?;
        publish(&status, |s| {
            s.node_id = hex::encode(key.verifying_key().as_bytes());
        });
        publish(&status, |s| {
            s.ready = true;
            s.message = "Loading local collections…".into();
        });
        // Identity is available before mapping or replay. This same store is
        // moved into Peer after EOF, never discarded just to reopen it.
        let mut opening = Some(Pile::open_as(&pile_path, key.verifying_key())?);
        let mut replay_complete = false;
        let replay_budget = 4 * 1024 * 1024u64;
        #[cfg(test)]
        let mut replay_budget = replay_budget;
        #[cfg(test)]
        let mut before_foreground: Option<Box<dyn FnOnce(&Pile, &Arc<Mutex<Status>>) + Send>> = None;
        #[cfg(test)]
        let mut before_peer_turn: Option<Box<dyn FnOnce(&Arc<Mutex<Status>>) + Send>> = None;
        #[cfg(test)]
        let mut before_preparation: Option<Box<dyn FnOnce(&Arc<Mutex<Status>>) + Send>> = None;
        let mut snapshot_requested = true;
        let mut opening_published = Instant::now();
        let mut replay_meter = ReplayMeter::default();
        let mut peer = None;
        let mut previous = None;
        let mut roots = Vec::new();
        let mut active = false;
        let mut paused = false; // Initial local replay may precede the first activity request.
        let mut network_allowed = true;
        let mut requested_activity = 0;
        let mut listening = false;
        let mut storage_roots = Vec::new();
        let mut storage_requested = false;
        let mut storage_changed = false;
        let mut storage_sampled = Instant::now();
        let mut storage_sample = None;
        let mut storage_authority = None;
        let mut storage_statistics = None;
        let mut storage_generation = None;
        let mut chart_active = false;
        let mut chart_requested = false;
        let mut chart_changed = false;
        let mut chart_sampled = Instant::now();
        let mut chart_sample = None;
        let mut chart_statistics = None;
        let operation = (|| -> Result<()> {
        loop {
            let command = if !paused && opening.is_some() && !replay_complete {
                // A command boundary between bounded replay steps. Do not add
                // the ordinary idle delay to every piece of a cold pile.
                commands.recv_timeout(Duration::ZERO)
            } else if active {
                commands.recv_timeout(Duration::from_millis(200))
            } else {
                commands
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            };
            let command = match command {
                Ok(Command::LocalActive(serial)) => {
                    network_allowed = false;
                    Ok(Command::Active(true, serial))
                }
                Ok(Command::Active(true, serial)) => {
                    network_allowed = true;
                    Ok(Command::Active(true, serial))
                }
                other => other,
            };
            let mut completed_choice = None;
            match command {
                Ok(Command::LocalActive(_)) => unreachable!("normalized activity command"),
                Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Ok(());
                }
                Ok(Command::Discover) => {
                    if !replay_complete {
                        publish(&status, |s| s.action_error = Some("Local collections are still loading".into()));
                        continue;
                    }
                    // Discovery changes saved native intent through the same store.
                    // A dormant peer needs an eager host afterward, but not a reopen.
                    sessions.store(None);
                    cleanup(&mut peer, &mut opening)?;
                    collections::start_discovery(opening.as_mut().expect("local owner"), &key)?;
                    snapshot_requested = true;
                    previous = None;
                }
                #[cfg(test)]
                Ok(Command::OpeningCheckpoint(checkpoint)) => {
                    checkpoint(opening.as_ref(), peer.is_some(), &status, &mut replay_budget);
                    continue;
                }
                #[cfg(test)]
                Ok(Command::BeforeFirstPeerTurn(checkpoint)) => {
                    before_peer_turn = Some(checkpoint);
                    continue;
                }
                #[cfg(test)]
                Ok(Command::BeforePreparation(checkpoint)) => {
                    before_preparation = Some(checkpoint);
                    continue;
                }
                #[cfg(test)]
                Ok(Command::BeforeForeground(checkpoint)) => {
                    before_foreground = Some(checkpoint);
                    continue;
                }
                #[cfg(test)]
                Ok(Command::StorageCheckpoint(checkpoint)) => {
                    // A queued fixture/observation boundary, never a sampling
                    // turn. Production has no checkpoint command or state.
                    checkpoint(peer.as_mut().expect("active test peer"), storage_sample.is_some(), &status);
                    continue;
                }
                Ok(Command::Storage(requested)) => {
                    if !update_storage_request(&mut storage_roots, requested, &mut storage_sample) { continue; }
                    storage_requested = true;
                    let visible: std::collections::HashSet<_> = storage_roots.iter().map(|root| hex::encode(root.raw)).collect();
                    publish(&status, |s| Arc::make_mut(&mut s.storage).retain(|row| visible.contains(&row.root)));
                    if storage_roots.is_empty() {
                        storage_changed = false;
                        storage_authority = None;
                    }
                }
                Ok(Command::StorageActive(next)) => {
                    if next == chart_active { continue; }
                    chart_active = next;
                    chart_requested = next;
                    if !next {
                        chart_sample = None;
                        chart_changed = false;
                        publish(&status, |s| s.composition = None);
                    }
                }
                #[cfg(test)]
                Ok(Command::Page(offset)) => {
                    publish(&status, |s| s.test_page_offset = offset);
                    snapshot_requested = true;
                }
                Ok(Command::Choose(root_handle, expected, enabled)) => {
                    if !replay_complete {
                        publish(&status, |s| {
                            s.action_error = Some("Local collections are still loading".into());
                            s.action_serial = s.action_serial.wrapping_add(1);
                        });
                        continue;
                    }
                    // Do not hold the status mutex across storage/network work.
                    let mut inactive_view = None;
                    let choice = if let Some(current) = peer.as_mut() {
                        (|| -> Result<()> {
                            current.try_refresh()?;
                            collections::choose(&mut current.store(), &key, root_handle, &expected, enabled)
                        })()
                    } else {
                        let pile = opening.as_mut().expect("local owner");
                        let result = collections::choose(pile, &key, root_handle, &expected, enabled);
                        let snapshot = pile.snapshot()?;
                        let selected = selected_in(&snapshot, &key)?;
                        let sync = sync_progress(&snapshot, &key, &selected)?;
                        let progress = ReplayProgress::observe(&snapshot, pile.observed_length()?, requested_activity);
                        inactive_view = Some((snapshot, selected, progress, sync));
                        result
                    };
                    let error = choice.err().map(|error| format!("{error:#}"));
                    if let Some((snapshot, selected, progress, sync)) = inactive_view {
                        publish(&status, |s| {
                            s.offer_snapshot(&snapshot, key.verifying_key(), true);
                            s.replay_progress = Some(progress);
                            s.sync_progress = Some(sync);
                            s.selected = selected.len();
                            s.action_error = error;
                            s.action_serial = s.action_serial.wrapping_add(1);
                        });
                    } else {
                        // Acknowledge only with the post-choice snapshot
                        // after replication retargeting and the durable checkpoint.
                        completed_choice = Some(error);
                    }
                    previous = None;
                }
                Ok(Command::Active(next, serial)) => {
                    if next != active { replay_meter = ReplayMeter::default(); }
                    requested_activity = serial;
                    if next == active && (next || paused) {
                        // Repeated lifecycle events still need their own ack.
                        // An in-progress replay acknowledges only at EOF below.
                        if !next || replay_complete {
                            if let Some(current) = &peer {
                                sessions.store(Some(Arc::new(network::Session { serial, reader: current.health_reader() })));
                            }
                            publish(&status, |s| s.activity_serial = serial);
                        }
                        continue;
                    }
                    paused = !next;
                    if !next {
                        storage_sample = None;
                        chart_sample = None;
                        chart_requested = chart_active;
                        storage_requested = !storage_roots.is_empty();
                        sessions.store(None);
                        let stopped = cleanup(&mut peer, &mut opening);
                        active = false;
                        let warning = match stopped {
                            Ok(()) => None,
                            Err(error) if error.downcast_ref::<CleanupFailure>().is_some_and(CleanupFailure::can_resume) => {
                                Some(format!("Previous pause: {error:#}. Host joined and local store checkpointed; transport close did not finish."))
                            }
                            Err(error) => return Err(error),
                        };
                        publish(&status, |s| {
                            if warning.is_some() { s.last_cleanup_error = warning; }
                            s.running = false;
                            s.network_started = false;
                            s.bound_sockets = 0;
                            s.message = "Replica paused".into();
                            s.activity_serial = serial;
                        });
                        continue;
                    }
                    // A retained prefix remains valid. Catch up only its tail;
                    // acknowledge this activation after its complete observation.
                    if let Some(pile) = opening.as_ref() {
                        replay_complete = pile.replayed_length() == pile.observed_length()?;
                    }
                    snapshot_requested = true;
                    active = true;
                    publish(&status, |s| {
                        s.running = false;
                        s.message = if replay_complete { "Local collections ready" } else { "Loading local collections…" }.into();
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if !paused && opening.is_some() && (!replay_complete || snapshot_requested) {
                let progress = (|| -> Result<bool> {
                    let (snapshot, complete) = opening.as_mut().unwrap().snapshot_step(replay_budget)?;
                    if complete || snapshot_requested || opening_published.elapsed() >= Duration::from_millis(250) {
                        let pile = opening.as_ref().unwrap();
                        let mut progress = ReplayProgress::observe(&snapshot, pile.observed_length()?, requested_activity);
                        if !complete {
                            progress.records_per_second = replay_meter.observe(Instant::now(), pile.replayed_records());
                        }
                        if complete {
                            replay_meter = ReplayMeter::default();
                        }
                        let offer_prefix = |s: &mut Status| {
                            if s.replay_progress.map(|old| old.prefix_bytes) != Some(progress.prefix_bytes) {
                                s.storage = Arc::default();
                            }
                            s.replay_progress = Some(progress);
                            s.offer_snapshot(&snapshot, key.verifying_key(), complete);
                        };
                        let reached_eof = complete && !replay_complete;
                        if reached_eof {
                            // The pile has reached EOF. Selection and admission checks
                            // below must finish before actions or foreground are ready.
                            publish(&status, |s| {
                                offer_prefix(s);
                                s.replay_complete = false;
                            });
                            #[cfg(test)]
                            if let Some(checkpoint) = before_preparation.take() { checkpoint(&status); }
                        }
                        let (selected, sync) = if complete {
                            let selected = selected_in(&snapshot, &key)?;
                            (selected.len(), Some(sync_progress(&snapshot, &key, &selected)?))
                        } else {
                            (0, None)
                        };
                        let listening = complete && collections::listening_in(&snapshot, key.verifying_key())?;
                        publish(&status, |s| {
                            if !reached_eof { offer_prefix(s); }
                            s.sync_progress = sync;
                            if !complete { s.composition = None; }
                            s.replay_complete = complete;
                            if complete && active { s.activity_serial = requested_activity; }
                            s.selected = selected;
                            s.listening = listening;
                            s.message = if complete { "Local collections ready" } else { "Loading local collections…" }.into();
                        });
                        snapshot_requested = false;
                        opening_published = Instant::now();
                    }
                    Ok(complete)
                })();
                match progress {
                    Ok(complete) => replay_complete = complete,
                    Err(error) => bail!("replica replay failed: {error:#}"),
                }
            }
            if active && network_allowed && replay_complete {
                if let Some(pile) = opening.take() {
                    // The complete local snapshot is already readable. Network
                    // startup and serving indexes cannot delay that publication.
                    #[cfg(test)]
                    if let Some(checkpoint) = before_foreground.take() { checkpoint(&pile, &status); }
                    let current = foreground_pile(pile, key.clone())?;
                    sessions.store(Some(Arc::new(network::Session { serial: requested_activity, reader: current.health_reader() })));
                    peer = Some(current);
                    #[cfg(test)]
                    if let Some(checkpoint) = before_peer_turn.take() { checkpoint(&status); }
                    publish(&status, |s| s.running = true);
                }
            }
            if let Some(current) = peer.as_mut() {
                let operation = (|| -> Result<()> {
                    current.try_refresh()?;
                    let snapshot = current.store().snapshot()?;
                    let observed_bytes = current.store().observed_length()?;
                    let snapshot_progress = ReplayProgress::observe(&snapshot, observed_bytes, requested_activity);
                    let mut changed_view = None;
                    if storage_generation != Some(snapshot.held_generation()) {
                        storage_changed = true;
                        chart_changed = true;
                        storage_generation = Some(snapshot.held_generation());
                    }
                    if previous
                        .as_ref()
                        .is_none_or(|old| snapshot.changes_since(old) != StoreChanges::NONE)
                    {
                        storage_changed = true;
                        chart_changed = true;
                        listening = collections::listening(&mut current.store(), &key)?;
                        let next = selected_in(&snapshot, &key)?;
                        changed_view = Some(sync_progress(&snapshot, &key, &next)?);
                        if previous.is_none() || roots != next {
                            current.set_replication(ReplicationMode::Full, next.iter().copied());
                            if !next.is_empty() {
                                current.activate_collections(next.iter().copied());
                            }
                            roots = next;
                        }
                        current.try_refresh()?;
                        // Keep the boundary that triggered this pass. Native arrivals can
                        // race our selection writes; a newer snapshot was not all examined.
                        previous = Some(snapshot.clone());
                    }
                    // Expose the immutable snapshot before optional inventory
                    // setup or byte steps. No byte batch owns their readiness.
                    publish(&status, |s| {
                        if let Some(sync) = changed_view {
                            s.offer_snapshot(&snapshot, key.verifying_key(), true);
                            s.sync_progress = Some(sync);
                        }
                        s.replay_progress = Some(snapshot_progress);
                    });
                    // Only visible roots; arrivals coalesce into a five-second sample.
                    // No periodic scan while Storage is hidden (empty request set).
                    if storage_authority.as_ref().is_some_and(|(old, reads)|
                        !snapshot.changes_for(old, reads).is_empty()) {
                        storage_sample = None;
                        storage_requested = true;
                    }
                    let mut started_bytes = false;
                    if storage_sample.is_none() && !storage_roots.is_empty() && (storage_requested ||
                        (storage_changed && storage_sampled.elapsed() >= Duration::from_secs(5))) {
                        let sample = storage::IncrementalSample::new(snapshot.clone(), key.verifying_key(), &storage_roots, storage_statistics.as_ref());
                        storage_authority = Some(sample.authority.clone());
                        started_bytes = true;
                        storage_sample = Some(sample);
                        storage_requested = false;
                        storage_changed = false;
                        storage_sampled = Instant::now();
                    }
                    if let Some(sample) = storage_sample.as_mut() {
                        if let Some((observations, summary)) = sample.step(4096) {
                            if summary.is_some() { storage_statistics = summary; }
                            if !storage_requested {
                                publish(&status, |s| s.storage = Arc::new(observations));
                            }
                            storage_sample = None;
                        } else if started_bytes {
                            // An already complete sample never needs a pending publication.
                            publish(&status, |s| s.storage = Arc::new(sample.pending_rows(&s.storage)));
                        }
                    }
                    // Chart roots come from the complete native row projection,
                    // independently of viewport and current replication choices.
                    if chart_active && chart_sample.is_none() && (chart_requested ||
                        (chart_changed && chart_sampled.elapsed() >= Duration::from_secs(5))) {
                        match storage::IncrementalCompositionSample::new(snapshot.clone(), key.verifying_key(), chart_statistics.as_ref()) {
                            Ok(sample) => chart_sample = Some(sample),
                            Err(error) => publish(&status, |s| s.composition = Some(Arc::new(storage::Composition::unavailable(error)))),
                        }
                        chart_requested = false;
                        chart_changed = false;
                        chart_sampled = Instant::now();
                    }
                    if let Some(sample) = chart_sample.as_mut() {
                        if let Some((composition, summary)) = sample.step(4096) {
                            if summary.is_some() { chart_statistics = summary; }
                            publish(&status, |s| s.composition = Some(Arc::new(composition)));
                            chart_sample = None;
                        }
                    }
                    let progress = if listening || !roots.is_empty() {
                        Some(runtime.block_on(async {
                            let result = tokio::time::timeout(
                                Duration::from_millis(150),
                                current.reconcile(),
                            )
                            .await;
                            // Even an empty listening replica must drive incoming grant exchange.
                            tokio::time::sleep(Duration::from_millis(10)).await;
                            result
                        }))
                    } else {
                        None
                    };
                    current.flush().context("durable checkpoint")?;
                    let health = current.health();
                    publish(&status, |s| {
                        s.listening = listening;
                        s.selected = roots.len();
                        if let Some(error) = completed_choice.take() {
                            s.action_error = error;
                            s.action_serial = s.action_serial.wrapping_add(1);
                        }
                        s.network_started = health.started_at.is_some();
                        s.bound_sockets = current.bound_sockets().len();
                        s.message = if roots.is_empty() {
                            if listening {
                                "Listening for collection access"
                            } else {
                                "No collections selected"
                            }
                        } else {
                            match progress {
                                Some(Ok(stats)) if stats.received_bytes > 0 => {
                                    "Received data saved on this device"
                                }
                                Some(Err(_)) => "Sync in progress",
                                _ => "Waiting for selected collections",
                            }
                        }
                        .into();
                    });
                    Ok(())
                })();
                operation?;
            }
        }
        })();
        // Stop and worker errors close whichever owner remains, even after an
        // earlier operation failed. Peer::new's consuming startup error still
        // cannot return its backend here. Pause never reaches this boundary.
        drop(previous.take());
        drop(storage_sample.take());
        drop(chart_sample.take());
        sessions.store(None);
        let peer_close = finish(&mut peer);
        let local_close = opening.take().map(Pile::close).transpose();
        match (operation, peer_close, local_close) {
            (Ok(()), Ok(()), Ok(_)) => Ok(()),
            (operation, peer, local) => bail!("replica: {operation:?}; peer close: {peer:?}; local close: {local:?}"),
        }
    })();
    if let Err(error) = result {
        publish(&status, |s| {
            s.running = false;
            s.ready = true;
            s.replay_complete = false;
            s.replay_progress = None;
            s.sync_progress = None;
            s.list_snapshot = None;
            s.list_revision = s.list_revision.wrapping_add(1);
            s.storage = Arc::default();
            s.composition = None;
            s.network_started = false;
            s.bound_sockets = 0;
            s.message = "Replica needs attention".into();
            s.error = Some(format!("{error:#}"));
        });
    }
}

// Debug query/reconcile paths exceed the iOS dispatch queue's ~544 KiB stack.
// Own the execution stack rather than depending on the caller's thread budget.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

impl Replica {
    /// Start the single native store owner. Opening/replay proceeds on its own stack.
    pub fn open(root: &Path) -> Result<Self> {
        let root = root.to_owned();
        let (command, receiver) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let worker_status = status.clone();
        let peer = Arc::new(arc_swap::ArcSwapOption::empty());
        let worker_peer = peer.clone();
        std::thread::Builder::new().name("ios-replica".into()).stack_size(WORKER_STACK_BYTES)
            .spawn(move || worker(root, receiver, worker_status, worker_peer))?;
        Ok(Self { command, status, peer, activity_serial: AtomicU64::new(0) })
    }

    /// Bind only to the peer owned by this foreground request. The slot changes
    /// at lifecycle boundaries, independently of the pile/status publisher.
    pub fn peer_health(&self, serial: u64) -> Option<triblespace_net::health::HealthReader> {
        self.peer.load().as_ref().filter(|s| serial != 0 && s.serial == serial).map(|s| s.reader.clone())
    }

    /// A frame with no change or a busy writer can keep its current immutable view.
    /// Capture does not query the pile or copy retained storage observations.
    pub fn read_after(&self, previous: Option<u64>) -> Option<Status> {
        let status = self.status.try_lock().ok()?;
        (previous != Some(status.revision)).then(|| status.clone())
    }

    fn activity(&self, command: impl FnOnce(u64) -> Command) -> u64 {
        let serial = self.activity_serial.fetch_add(1, Ordering::Relaxed) + 1;
        if self.command.send(command(serial)).is_ok() { serial } else { 0 }
    }

    pub fn set_active(&self, active: bool) -> u64 { self.activity(|serial| Command::Active(active, serial)) }
    pub fn set_local_active(&self) -> u64 { self.activity(Command::LocalActive) }
    pub fn discover(&self) -> bool { self.command.send(Command::Discover).is_ok() }

    /// Enable whole-device composition sampling only while the chart is visible.
    pub fn set_storage_active(&self, active: bool) -> bool {
        self.command.send(Command::StorageActive(active)).is_ok()
    }

    /// Replace the bounded visible-root byte request without changing selection.
    pub fn set_storage_roots(&self, ids: &[String]) -> bool {
        if ids.len() > collections::PAGE_SIZE { return false; }
        let mut roots = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(root) = parse_handle(id) else { return false; };
            roots.push(root);
        }
        roots.sort_unstable();
        roots.dedup();
        self.command.send(Command::Storage(roots)).is_ok()
    }

    pub fn choose(&self, root: &str, expected: &str, enabled: bool) -> bool {
        let Some(root) = parse_handle(root) else { return false; };
        self.command.send(Command::Choose(root, expected.to_owned(), enabled)).is_ok()
    }
}

fn parse_handle(id: &str) -> Option<CollectionHandle> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(id, &mut bytes).ok()?;
    Some(CollectionHandle::new(bytes))
}

impl Drop for Replica {
    fn drop(&mut self) { let _ = self.command.send(Command::Stop); }
}

#[cfg(test)]
impl Status {
    // Reuse historical fixture assertions while explicitly querying the reader
    // side. Production never constructs this combined object.
    fn test_page(&self) -> collections::Page {
        self.list_snapshot.as_ref().map(|view|
            CollectionPages::default().read(view, self.test_page_offset).expect("fixture page")
        ).unwrap_or_default()
    }

    fn test_value(&self) -> serde_json::Value {
        serde_json::json!({
            "node_id": self.node_id, "running": self.running, "ready": self.ready,
            "replay_complete": self.replay_complete, "replay_progress": self.replay_progress,
            "sync_progress": self.sync_progress, "activity_serial": self.activity_serial,
            "selected": self.selected, "listening": self.listening, "collections": self.test_page(),
            "storage": &*self.storage, "composition": self.composition.as_deref(),
            "action_error": self.action_error, "action_serial": self.action_serial,
            "last_cleanup_error": self.last_cleanup_error, "network_started": self.network_started,
            "bound_sockets": self.bound_sockets,
            "message": self.message, "error": self.error
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait(
        replica: &Replica,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let Some(status) = replica.read_after(None) else {
                assert!(Instant::now() < deadline, "worker stayed busy");
                std::thread::yield_now();
                continue;
            };
            let value = status.test_value();
            assert!(value["error"].is_null(), "{value}");
            if predicate(&value) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "worker transition timed out: {value}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn empty_shell_worker_pauses_and_restarts_with_the_same_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        {
            let first = Replica::open(path).unwrap();
            let before = wait(&first, |s| s["ready"] == true && s["replay_complete"] == true);
            first.set_active(true);
            let active = wait(&first, |s| s["running"] == true);
            assert_eq!(active["selected"], 0);
            assert_eq!(active["network_started"], false);
            assert_eq!(active["bound_sockets"], 0);
            std::thread::sleep(Duration::from_millis(500));
            let idle = wait(&first, |s| s["running"] == true);
            assert_eq!(idle["network_started"], false);
            assert_eq!(idle["bound_sockets"], 0);
            first.set_active(false);
            wait(&first, |s| {
                s["running"] == false && s["message"] == "Replica paused"
            });
            drop(first);
            let second = Replica::open(path).unwrap();
            let after = wait(&second, |s| s["ready"] == true && s["replay_complete"] == true);
            assert_eq!(before["node_id"], after["node_id"]);
            second.set_active(true);
            wait(&second, |s| s["running"] == true);
            second.set_active(false);
            wait(&second, |s| {
                s["running"] == false && s["message"] == "Replica paused"
            });
            drop(second);
        }
    }
    #[test]
    fn explicit_discovery_starts_empty_listener_and_survives_worker_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        {
            let first = Replica::open(path).unwrap();
            wait(&first, |s| s["ready"] == true && s["replay_complete"] == true);
            first.set_active(true);
            let before = wait(&first, |s| s["running"] == true);
            assert_eq!(before["network_started"], false);
            assert_eq!(before["selected"], 0);
            first.discover();
            let listening = wait(&first, |s| {
                s["listening"] == true && s["network_started"] == true
            });
            assert_eq!(listening["selected"], 0);
            assert!(listening["bound_sockets"].as_u64().unwrap() > 0);
            first.set_active(false);
            wait(&first, |s| {
                s["running"] == false && s["message"] == "Replica paused"
            });
            drop(first);
            let second = Replica::open(path).unwrap();
            wait(&second, |s| s["ready"] == true && s["replay_complete"] == true);
            second.set_active(true);
            let reopened = wait(&second, |s| {
                s["listening"] == true && s["network_started"] == true
            });
            assert_eq!(reopened["node_id"], before["node_id"]);
            assert_eq!(reopened["selected"], 0);
            second.set_active(false);
            wait(&second, |s| {
                s["running"] == false && s["message"] == "Replica paused"
            });
            drop(second);
        }
    }
    #[test]
    fn shell_choice_commands_recheck_state_flush_and_survive_restart() {
        use triblespace_core::collection::{CollectionStoreExt, private_policy, grant_collection_read};
        let dir = tempfile::tempdir().unwrap();
        let (phone, mut pile) = open_local(dir.path()).unwrap();
        let owner = SigningKey::from_bytes(&[49; 32]);
        let root = pile.collection("available", private_policy(owner.verifying_key())).unwrap().handle();
        grant_collection_read(&mut pile, root, &owner, phone.verifying_key()).unwrap();
        pile.flush().unwrap(); pile.close().unwrap();
        let path = dir.path();
        let root_text = hex::encode(root.raw);
        let unset = "unset";
        let selected = "selected";
        {
            let first = Replica::open(path).unwrap();
            first.set_active(true);
            let available = wait(&first, |s| s["collections"]["total"] == 1);
            assert_eq!(available["selected"], 0);
            assert_eq!(available["network_started"], false);
            assert!(first.choose(&root_text, unset, true));
            wait(&first, |s| s["selected"] == 1 && s["action_serial"] == 1);
            assert!(first.choose(&root_text, unset, false));
            let stale = wait(&first, |s| s["action_serial"] == 2);
            assert!(!stale["action_error"].is_null());
            assert_eq!(stale["selected"], 1);
            assert!(first.choose(&root_text, selected, false));
            let off = wait(&first, |s| s["selected"] == 0 && s["action_serial"] == 3);
            assert!(off["action_error"].is_null());
            first.set_active(false);
            wait(&first, |s| s["running"] == false && s["message"] == "Replica paused");
            drop(first);
            let second = Replica::open(path).unwrap(); second.set_active(true);
            let reopened = wait(&second, |s| s["collections"]["total"] == 1);
            assert_eq!(reopened["node_id"], available["node_id"]);
            assert_eq!(reopened["collections"]["rows"][0]["state"], "unselected");
            assert_eq!(reopened["selected"], 0);
            assert_eq!(reopened["network_started"], false);
            second.set_active(false);
            wait(&second, |s| s["running"] == false && s["message"] == "Replica paused");
            drop(second);
        }
    }

    #[test]
    fn choice_acknowledgement_contains_matching_rows_and_count_without_network() {
        use triblespace_core::collection::{CollectionStoreExt, private_policy};
        use triblespace_core::{metadata, prelude::entity};
        let dir = tempfile::tempdir().unwrap();
        let (key, mut pile) = open_local(dir.path()).unwrap();
        let collection = pile.collection("choice", private_policy(key.verifying_key())).unwrap();
        let root = collection.handle();
        pile.commit(collection, &key, entity! { metadata::name: "synthetic" }).unwrap();
        pile.flush().unwrap(); pile.close().unwrap();
        let path = dir.path();
        let root_text = hex::encode(root.raw);
        let unset = "unset";
        // Inactive worker exercises the real owner/choice/checkpoint path without
        // starting an endpoint or the known Mac CoreWLAN enrichment path.
        let (acknowledged, stopped, stale) = {
            let replica = Replica::open(path).unwrap();
            wait(&replica, |s| s["ready"] == true && s["replay_complete"] == true);
            assert!(replica.choose(&root_text, unset, true));
            let first = wait(&replica, |s| s["action_serial"] == 1);
            // Queue rapid valid choices without waiting for intermediate polls.
            for (expected, enabled) in [("selected", false), ("unselected", true), ("selected", false)] {
                assert!(replica.choose(&root_text, expected, enabled));
            }
            let stopped = wait(&replica, |s| s["action_serial"] == 4);
            let outdated = "selected";
            assert!(replica.choose(&root_text, outdated, true));
            let stale = wait(&replica, |s| s["action_serial"] == 5);
            drop(replica);
            (first, stopped, stale)
        };
        let (key, mut reopened) = open_local(dir.path()).unwrap();
        assert!(selected(&mut reopened, &key).unwrap().is_empty());
        assert_eq!(collections::page(&mut reopened, &key, 0).unwrap().rows[0].state, "unselected");
        reopened.close().unwrap();
        assert_eq!(acknowledged["running"], false);
        assert_eq!(acknowledged["network_started"], false);
        assert_eq!(acknowledged["selected"], 1, "{acknowledged}");
        assert_eq!(acknowledged["collections"]["rows"][0]["state"], "selected", "{acknowledged}");
        for value in [&stopped, &stale] {
            assert_eq!(value["selected"], 0, "{value}");
            assert_eq!(value["collections"]["rows"][0]["state"], "unselected", "{value}");
            assert_eq!(value["network_started"], false);
        }
        assert!(stopped["action_error"].is_null());
        assert!(!stale["action_error"].is_null());
    }

    #[test]
    fn cleanup_failure_keeps_all_errors_and_only_joined_timeout_is_recoverable() {
        let recoverable = CleanupFailure { shutdown: Some(JOINED_TRANSPORT_TIMEOUT.into()), flush: None, close: None };
        assert!(recoverable.can_resume());
        let failed = CleanupFailure { shutdown: Some(JOINED_TRANSPORT_TIMEOUT.into()),
            flush: Some("flush failed".into()), close: Some("close failed".into()) };
        assert!(!failed.can_resume());
        let text = failed.to_string();
        assert!(text.contains("network shutdown") && text.contains("flush failed") && text.contains("close failed"));
        assert!(!CleanupFailure { shutdown: Some("network host thread panicked".into()), flush: None, close: None }.can_resume());
    }

    #[test]
    fn cleanup_timeout_keeps_worker_alive_for_queued_foreground_and_durable_reopen() {
        use triblespace_core::collection::{CollectionStoreExt, private_policy};
        use triblespace_core::{metadata, prelude::entity};
        let dir = tempfile::tempdir().unwrap();
        let (key, mut pile) = open_local(dir.path()).unwrap();
        let collection = pile.collection("recovery", private_policy(key.verifying_key())).unwrap();
        let root = collection.handle();
        pile.commit(collection, &key, entity! { metadata::name: "durable recovery fixture" }).unwrap();
        collections::choose(&mut pile, &key, root, "unset", false).unwrap();
        let before = storage::observe(&pile.snapshot().unwrap(), key.verifying_key(), root).counts.unwrap();
        assert!(before.records > 0 && before.records == before.direct_present);
        pile.close().unwrap();
        let (sender, receiver) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let replica = Replica { command: sender.clone(), status: status.clone(), peer: Default::default(), activity_serial: AtomicU64::new(0) };
        let path = dir.path().to_owned();
        let thread = std::thread::Builder::new().stack_size(WORKER_STACK_BYTES).spawn(move || {
            worker_with_cleanup(path, receiver, status, Default::default(), |peer, local| {
                let was_open = peer.is_some();
                pause(peer, local)?;
                if was_open {
                    // Real lazy-peer shutdown/checkpoint happened. Inject only the
                    // native joined-timeout report, not an actual endpoint.
                    Err(CleanupFailure { shutdown: Some(JOINED_TRANSPORT_TIMEOUT.into()), flush: None, close: None }.into())
                } else { Ok(()) }
            });
        }).unwrap();
        {
            wait(&replica, |s| s["ready"] == true && s["replay_complete"] == true);
            sender.send(Command::Active(true, 0)).unwrap();
            let first = wait(&replica, |s| s["running"] == true && s["collections"]["total"] == 1);
            sender.send(Command::Active(false, 0)).unwrap();
            let paused = wait(&replica, |s| s["running"] == false && s["last_cleanup_error"].is_string());
            assert!(paused["last_cleanup_error"].as_str().unwrap().contains(JOINED_TRANSPORT_TIMEOUT));
            assert_eq!(paused["network_started"], false);
            sender.send(Command::Active(true, 0)).unwrap();
            wait(&replica, |s| s["running"] == true);
            // A foreground command already queued behind failing background
            // cleanup must still run; a subsequent page ack proves channel life.
            sender.send(Command::Active(false, 0)).unwrap();
            sender.send(Command::Active(true, 0)).unwrap();
            sender.send(Command::Page(32)).unwrap();
            let resumed = wait(&replica, |s| s["running"] == true && s["collections"]["offset"] == 32);
            assert_eq!(resumed["node_id"], first["node_id"]);
            assert_eq!(resumed["selected"], 0);
            assert_eq!(resumed["network_started"], false);
            sender.send(Command::Active(false, 0)).unwrap();
            wait(&replica, |s| s["running"] == false);
            sender.send(Command::Stop).unwrap();
        }
        thread.join().unwrap();
        let (reopened_key, mut reopened) = open_local(dir.path()).unwrap();
        assert_eq!(reopened_key.verifying_key(), key.verifying_key());
        assert_eq!(collections::page(&mut reopened, &key, 0).unwrap().rows[0].state, "unselected");
        assert_eq!(storage::observe(&reopened.snapshot().unwrap(), key.verifying_key(), root).counts, Some(before));
        reopened.close().unwrap();
    }

    #[test]
    fn storage_worker_publishes_before_bytes_finish_and_schedules_latest_scope() {
        use triblespace_core::collection::{CollectionCommit, CollectionRecord,
            CollectionStore, CollectionStoreExt, HeldStore, private_policy};
        use triblespace_core::prelude::*;
        let dir = tempfile::tempdir().unwrap();
        let (key, mut pile) = open_local(dir.path()).unwrap();
        let data = pile.put::<blobencodings::UTF8String, _>("direct data").unwrap();
        let meta = pile.put::<blobencodings::SimpleArchive, _>(TribleSet::new()).unwrap();
        let roots = ["worker a", "worker b"].map(|name| {
            let root = pile.collection(name, private_policy(key.verifying_key())).unwrap().handle();
            pile.insert(CollectionRecord::Commit(CollectionCommit::sign(
                &key, root, *data.as_transmute(), meta,
            ))).unwrap();
            root
        });
        pile.close().unwrap();
        let (sender, receiver) = mpsc::channel();
        let (observations, observed) = mpsc::channel();
        let checkpoint = || {
            let observations = observations.clone();
            Command::StorageCheckpoint(Box::new(move |_, unfinished, status| {
                let status = status.lock().unwrap();
                observations.send((status.storage.clone(), unfinished,
                    status.network_started, status.error.clone(), status.test_page().rows)).unwrap();
            }))
        };
        // Queue the entire script first: no wall-clock polling can consume an
        // extra sampling turn between a request and its checkpoint.
        sender.send(Command::Active(true, 0)).unwrap();
        sender.send(Command::StorageCheckpoint(Box::new(move |peer, _, _| {
            let mut pile = peer.store();
            pile.track_held(roots);
            for index in 0..4100 {
                let blob = pile.put::<blobencodings::UTF8String, _>(format!("worker held {index}")).unwrap();
                pile.note_held(roots[0], *blob.as_transmute());
            }
            pile.snapshot().unwrap();
            let mut work = pile.drive_held().unwrap();
            assert!(work.run_until_idle());
            assert!(pile.snapshot().unwrap().held(roots[0]).unwrap().len() > 4096);
        }))).unwrap();
        sender.send(Command::Storage(vec![roots[0]])).unwrap();
        sender.send(checkpoint()).unwrap();
        sender.send(Command::Storage(vec![roots[1]])).unwrap();
        sender.send(checkpoint()).unwrap();
        sender.send(Command::Page(0)).unwrap();
        sender.send(checkpoint()).unwrap();
        sender.send(Command::Page(32)).unwrap();
        sender.send(checkpoint()).unwrap();
        sender.send(Command::Stop).unwrap();
        drop(observations);
        let status = Arc::new(Mutex::new(Status::default()));
        let path = dir.path().to_owned();
        let (finished, completion) = mpsc::channel();
        let thread = std::thread::Builder::new().stack_size(WORKER_STACK_BYTES).spawn(move || {
            worker_with_cleanup(path, receiver, status, Default::default(), pause);
            let _ = finished.send(());
        }).unwrap();
        // Stop is already queued even if a later assertion fails. Bound the
        // channel wait and join before inspecting any published observations.
        let completed = completion.recv_timeout(Duration::from_secs(30));
        let _ = sender.send(Command::Stop);
        drop(sender);
        thread.join().unwrap();
        completed.expect("scripted storage worker did not settle");
        let samples: Vec<_> = observed.try_iter().collect();
        assert_eq!(samples.len(), 4);
        for (_, _, network_started, error, _) in &samples {
            assert!(!network_started);
            assert!(error.is_none(), "{error:?}");
        }
        // Actual shared page counts are already ready while the byte batch is unfinished.
        assert_eq!(samples[0].4.len(), 2);
        assert!(samples[0].4.iter().all(|row| row.counts == Some(storage::Counts { records: 1, direct_present: 1 })));
        assert!(samples[1].4.iter().all(|row| row.counts == Some(storage::Counts { records: 1, direct_present: 1 })));
        assert_eq!(samples[0].0[0].root, hex::encode(roots[0].raw));
        assert!(samples[0].1);
        assert!(samples[0].0[0].resident_bytes.is_none());
        // Changing scope finishes A for reuse, but no longer publishes it for B.
        assert!(samples[1].0.is_empty());
        assert!(!samples[1].1);
        // Actual worker scheduling then publishes B while removing A's large
        // inventory, and only publishes B's bytes after that batch completes.
        assert_eq!(samples[2].0[0].root, hex::encode(roots[1].raw));
        assert!(samples[2].1);
        assert!(samples[2].0[0].resident_bytes.is_none());
        assert_eq!(samples[3].0[0].root, hex::encode(roots[1].raw));
        assert!(!samples[3].1);
        assert!(samples[3].0[0].resident_bytes.is_some());
    }

    #[test]
    fn storage_visibility_coalesces_without_restarting_the_active_byte_batch() {
        use triblespace_core::collection::{CollectionCommit, CollectionRecord,
            CollectionStore, CollectionStoreExt, HeldStore, private_policy};
        use triblespace_core::prelude::*;
        let dir = tempfile::tempdir().unwrap();
        let (key, mut pile) = open_local(dir.path()).unwrap();
        let data = pile.put::<blobencodings::UTF8String, _>("shared data").unwrap();
        let meta = pile.put::<blobencodings::SimpleArchive, _>(TribleSet::new()).unwrap();
        let roots = ["a", "b", "c"].map(|name| {
            let root = pile.collection(name, private_policy(key.verifying_key())).unwrap().handle();
            pile.insert(CollectionRecord::Commit(CollectionCommit::sign(
                &key, root, *data.as_transmute(), meta,
            ))).unwrap();
            root
        });
        pile.track_held(roots);
        pile.snapshot().unwrap();
        let mut work = pile.drive_held().unwrap();
        assert!(work.run_until_idle());
        let snapshot = pile.snapshot().unwrap();
        let mut requested = roots[..2].to_vec();
        let mut sample = Some(storage::IncrementalSample::new(
            snapshot.clone(), key.verifying_key(), &requested, None,
        ));
        assert!(sample.as_ref().unwrap().observations().iter()
            .all(|row| row.resident_bytes.is_none()));
        assert!(sample.as_mut().unwrap().step(1).is_none());
        assert!(update_storage_request(&mut requested, roots[1..].to_vec(), &mut sample));
        assert!(!update_storage_request(&mut requested, roots[1..].to_vec(), &mut sample));
        let (rows, complete) = sample.as_mut().expect("active work survives visibility").step(4096).unwrap();
        assert_eq!(rows.iter().map(|row| row.root.clone()).collect::<Vec<_>>(),
            roots[..2].iter().map(|root| hex::encode(root.raw)).collect::<Vec<_>>());
        assert!(rows.iter().all(|row| row.resident_bytes.is_some()));

        // The successor is exactly the latest scope, reusing the overlapping
        // root's complete native statistics rather than restarting its work.
        sample = Some(storage::IncrementalSample::new(
            snapshot, key.verifying_key(), &requested, complete.as_ref(),
        ));
        let (next_rows, next) = sample.as_mut().unwrap().step(4096).unwrap();
        assert_eq!(next_rows.iter().map(|row| row.root.clone()).collect::<Vec<_>>(),
            roots[1..].iter().map(|root| hex::encode(root.raw)).collect::<Vec<_>>());
        assert_eq!(next.unwrap().work().records_checked, 0, "byte sampling must never count records");
        assert!(update_storage_request(&mut requested, Vec::new(), &mut sample));
        assert!(sample.is_none());
        pile.close().unwrap();
    }

    #[test]
    fn storage_requests_are_explicit_bounded_and_do_not_start_network() {
        use triblespace_core::collection::{CollectionStoreExt, private_policy};
        use triblespace_core::{metadata, prelude::entity};
        let dir = tempfile::tempdir().unwrap();
        let (key, mut pile) = open_local(dir.path()).unwrap();
        let collection = pile.collection("storage", private_policy(key.verifying_key())).unwrap();
        let root = collection.handle();
        // A descriptor alone is not a native available-collection candidate.
        pile.commit(collection, &key, entity! { metadata::name: "sample" }).unwrap();
        pile.flush().unwrap(); pile.close().unwrap();
        let path = dir.path();
        let request = vec![hex::encode(root.raw)];
        let too_many = vec![hex::encode(root.raw); 33];
        {
            let replica = Replica::open(path).unwrap(); replica.set_active(true);
            let before = wait(&replica, |s| s["collections"]["total"] == 1);
            assert_eq!(before["storage"], serde_json::json!([]));
            assert_eq!(before["collections"]["rows"][0]["counts"]["records"], 1, "row counts require no Storage request");
            assert!(!replica.set_storage_roots(&too_many));
            assert!(replica.set_storage_roots(&request));
            let sampled = wait(&replica, |s| s["storage"].as_array().is_some_and(|rows| rows.len() == 1));
            assert_eq!(sampled["collections"]["rows"][0]["counts"]["records"], 1);
            assert_eq!(sampled["collections"]["rows"][0]["counts"]["direct_present"], 1);
            assert!(sampled["storage"][0].get("counts").is_none());
            assert!(sampled["storage"][0]["resident_error"].is_null());
            assert_eq!(sampled["network_started"], false);
            assert!(replica.set_storage_roots(&[]));
            wait(&replica, |s| s["storage"] == serde_json::json!([]));
            replica.set_active(false);
            wait(&replica, |s| s["running"] == false && s["message"] == "Replica paused");
            drop(replica);
        }
    }

}

#[cfg(test)]
mod startup_tests;
