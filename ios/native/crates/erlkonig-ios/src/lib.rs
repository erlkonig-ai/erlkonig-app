//! iOS-owned foreground replica. No desktop runtime, MCP, or ambient paths.
use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use serde::Serialize;
use std::{
    ffi::{CStr, CString, c_char},
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

mod network;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct ReplayProgress {
    prefix_bytes: u64,
    observed_bytes: u64,
    activity_serial: u64,
}
impl ReplayProgress {
    fn observe(snapshot: &PileSnapshot, observed_bytes: u64, activity_serial: u64) -> Self {
        Self { prefix_bytes: snapshot.prefix_len() as u64, observed_bytes, activity_serial }
    }
}

/// Current-source native count aggregate for admitted selected roots only.
/// Native replay maintains the index; this is only a snapshot projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
struct SyncProgress {
    records: u64,
    direct_present: u64,
    unknown_roots: usize,
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

#[derive(Default, Serialize)]
struct Status {
    node_id: String,
    running: bool,
    ready: bool,
    /// False means values describe a prefix; mutations still wait for EOF.
    replay_complete: bool,
    replay_progress: Option<ReplayProgress>,
    sync_progress: Option<SyncProgress>,
    /// Acknowledges activation after complete local observation, pause after checkpoint.
    activity_serial: u64,
    selected: usize,
    listening: bool,
    collections: collections::Page,
    storage: Vec<storage::ByteObservation>,
    composition: Option<storage::Composition>,
    action_error: Option<String>,
    action_serial: u64,
    last_cleanup_error: Option<String>,
    network_started: bool,
    network: Option<network::Observation>,
    bound_sockets: usize,
    message: String,
    error: Option<String>,
}
enum Command {
    Active(bool, u64),
    LocalActive(u64),
    Discover,
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
    Stop,
}
pub struct Replica {
    command: mpsc::Sender<Command>,
    activity_serial: AtomicU64,
    status: Arc<Mutex<Status>>,
}

fn publish(status: &Arc<Mutex<Status>>, f: impl FnOnce(&mut Status)) {
    if let Ok(mut status) = status.lock() {
        f(&mut status);
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
) {
    worker_with_cleanup(root, commands, status, pause);
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
        let mut opening_page_requested = true;
        let mut opening_published = Instant::now();
        let mut peer = None;
        let mut previous = None;
        let mut roots = Vec::new();
        let mut active = false;
        let mut paused = false; // Initial local replay may precede the first activity request.
        let mut network_allowed = true;
        let mut requested_activity = 0;
        let mut listening = false;
        let mut page_offset = 0;
        let mut storage_roots = Vec::new();
        let mut storage_requested = false;
        let mut storage_changed = false;
        let mut storage_sampled = Instant::now();
        let mut storage_sample = None;
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
                    cleanup(&mut peer, &mut opening)?;
                    collections::start_discovery(opening.as_mut().expect("local owner"), &key)?;
                    opening_page_requested = true;
                    previous = None;
                }
                #[cfg(test)]
                Ok(Command::OpeningCheckpoint(checkpoint)) => {
                    checkpoint(opening.as_ref(), peer.is_some(), &status, &mut replay_budget);
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
                    if storage_roots.is_empty() {
                        storage_changed = false;
                        publish(&status, |s| s.storage.clear());
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
                Ok(Command::Page(offset)) => {
                    page_offset = offset;
                    opening_page_requested = true;
                    previous = None;
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
                        inactive_view = Some((collections::project(&snapshot, key.verifying_key(), page_offset, collections::PAGE_SIZE)?,
                            selected, ReplayProgress::observe(&snapshot, pile.observed_length()?, requested_activity), sync));
                        result
                    };
                    let error = choice.err().map(|error| format!("{error:#}"));
                    if let Some((page, selected, progress, sync)) = inactive_view {
                        publish(&status, |s| {
                            s.collections = page;
                            s.replay_progress = Some(progress);
                            s.sync_progress = Some(sync);
                            s.selected = selected.len();
                            s.action_error = error;
                            s.action_serial = s.action_serial.wrapping_add(1);
                        });
                    } else {
                        // Acknowledge only with the post-choice row/count projection
                        // after replication retargeting and the durable checkpoint.
                        completed_choice = Some(error);
                    }
                    previous = None;
                }
                Ok(Command::Active(next, serial)) => {
                    requested_activity = serial;
                    if next == active && (next || paused) {
                        // Repeated lifecycle events still need their own ack.
                        // An in-progress replay acknowledges only at EOF below.
                        if !next || replay_complete {
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
                            s.network = None;
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
                    opening_page_requested = true;
                    active = true;
                    publish(&status, |s| {
                        s.running = false;
                        s.network = None;
                        s.message = if replay_complete { "Local collections ready" } else { "Loading local collections…" }.into();
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if !paused && opening.is_some() && (!replay_complete || opening_page_requested) {
                let progress = (|| -> Result<bool> {
                    let (snapshot, complete) = opening.as_mut().unwrap().snapshot_step(replay_budget)?;
                    if complete || opening_page_requested || opening_published.elapsed() >= Duration::from_millis(250) {
                        let page = if complete {
                            collections::project(&snapshot, key.verifying_key(), page_offset, collections::PAGE_SIZE)?
                        } else {
                            collections::project_prefix(&snapshot, key.verifying_key(), page_offset, collections::PAGE_SIZE)?
                        };
                        let (selected, sync) = if complete {
                            let selected = selected_in(&snapshot, &key)?;
                            (selected.len(), Some(sync_progress(&snapshot, &key, &selected)?))
                        } else {
                            (0, None)
                        };
                        let listening = complete && collections::listening_in(&snapshot, key.verifying_key())?;
                        // Backing extent is a current observation, not snapshot state. An
                        // append after the replay step can make this denominator grow.
                        let observed_bytes = opening.as_ref().unwrap().observed_length()?;
                        let progress = ReplayProgress::observe(&snapshot, observed_bytes, requested_activity);
                        publish(&status, |s| {
                            if s.replay_progress.map(|old| old.prefix_bytes)
                                != Some(progress.prefix_bytes) { s.storage.clear(); }
                            s.replay_progress = Some(progress);
                            s.sync_progress = sync;
                            s.collections = page;
                            s.replay_complete = complete;
                            if complete && active { s.activity_serial = requested_activity; }
                            s.selected = selected;
                            s.listening = listening;
                            s.message = if complete { "Local collections ready" } else { "Loading local collections…" }.into();
                        });
                        opening_page_requested = false;
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
                    // Complete local rows have already been published. Network
                    // startup and serving indexes cannot delay that publication.
                    #[cfg(test)]
                    if let Some(checkpoint) = before_foreground.take() { checkpoint(&pile, &status); }
                    peer = Some(foreground_pile(pile, key.clone())?);
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
                        let page = collections::project(&snapshot, key.verifying_key(), page_offset, collections::PAGE_SIZE)?;
                        let next = selected_in(&snapshot, &key)?;
                        changed_view = Some((page, sync_progress(&snapshot, &key, &next)?));
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
                    // Publish snapshot-native rows/counts before optional inventory
                    // setup or byte steps. No byte batch owns their readiness.
                    publish(&status, |s| {
                        if let Some((page, sync)) = changed_view {
                            s.collections = page;
                            s.sync_progress = Some(sync);
                        }
                        s.replay_progress = Some(snapshot_progress);
                    });
                    // Only visible roots; arrivals coalesce into a five-second sample.
                    // No periodic scan while Storage is hidden (empty request set).
                    if storage_sample.is_none() && !storage_roots.is_empty() && (storage_requested ||
                        (storage_changed && storage_sampled.elapsed() >= Duration::from_secs(5))) {
                        let sample = storage::IncrementalSample::new(snapshot.clone(), key.verifying_key(), &storage_roots, storage_statistics.as_ref());
                        // Counts are already in the independently published page;
                        // this pinned batch publishes only optional byte quantities.
                        publish(&status, |s| s.storage = sample.observations().to_vec());
                        storage_sample = Some(sample);
                        storage_requested = false;
                        storage_changed = false;
                        storage_sampled = Instant::now();
                    }
                    if let Some(sample) = storage_sample.as_mut() {
                        if let Some((observations, summary)) = sample.step(4096) {
                            if summary.is_some() { storage_statistics = summary; }
                            publish(&status, |s| s.storage = observations);
                            storage_sample = None;
                        }
                    }
                    // Chart roots come from the complete native row projection,
                    // independently of viewport and current replication choices.
                    if chart_active && chart_sample.is_none() && (chart_requested ||
                        (chart_changed && chart_sampled.elapsed() >= Duration::from_secs(5))) {
                        match storage::IncrementalCompositionSample::new(snapshot.clone(), key.verifying_key(), chart_statistics.as_ref()) {
                            Ok(sample) => chart_sample = Some(sample),
                            Err(error) => publish(&status, |s| s.composition = Some(storage::Composition::unavailable(error))),
                        }
                        chart_requested = false;
                        chart_changed = false;
                        chart_sampled = Instant::now();
                    }
                    if let Some(sample) = chart_sample.as_mut() {
                        if let Some((composition, summary)) = sample.step(4096) {
                            if summary.is_some() { chart_statistics = summary; }
                            publish(&status, |s| s.composition = Some(composition));
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
                        s.network = network::Observation::observe(&health, s.network.as_ref());
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
            s.collections = collections::Page::default();
            s.storage.clear();
            s.composition = None;
            s.network_started = false;
            s.bound_sockets = 0;
            s.network = None;
            s.message = "Replica needs attention".into();
            s.error = Some(format!("{error:#}"));
        });
    }
}

// Debug query/reconcile paths exceed the iOS dispatch queue's ~544 KiB stack.
// Own the execution stack rather than depending on the caller's thread budget.
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

/// All pointers belong to the native shell; no Swift object crosses the ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_open(path: *const c_char) -> *mut Replica {
    if path.is_null() { return std::ptr::null_mut(); }
    let Ok(path) = (unsafe { CStr::from_ptr(path) }).to_str() else {
        return std::ptr::null_mut();
    };
    let root = PathBuf::from(path);
    let (command, receiver) = mpsc::channel();
    let status = Arc::new(Mutex::new(Status::default()));
    let worker_status = status.clone();
    if std::thread::Builder::new()
        .name("ios-replica".into())
        .stack_size(WORKER_STACK_BYTES)
        .spawn(move || worker(root, receiver, worker_status))
        .is_err()
    {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(Replica { command, status, activity_serial: AtomicU64::new(0) }))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_active(replica: *mut Replica, active: bool) -> u64 {
    let Some(replica) = (unsafe { replica.as_ref() }) else { return 0; };
    let serial = replica.activity_serial.fetch_add(1, Ordering::Relaxed) + 1;
    if replica.command.send(Command::Active(active, serial)).is_ok() { serial } else { 0 }
}
/// Foreground local replay for the synthetic proof reader, without networking.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_local_active(replica: *mut Replica) -> u64 {
    let Some(replica) = (unsafe { replica.as_ref() }) else { return 0; };
    let serial = replica.activity_serial.fetch_add(1, Ordering::Relaxed) + 1;
    if replica.command.send(Command::LocalActive(serial)).is_ok() { serial } else { 0 }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_discover(replica: *mut Replica) {
    if let Some(replica) = unsafe { replica.as_ref() } {
        let _ = replica.command.send(Command::Discover);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_collections_page(replica: *mut Replica, offset: usize) {
    if let Some(replica) = unsafe { replica.as_ref() } {
        let _ = replica.command.send(Command::Page(offset));
    }
}
/// Enable transient whole-row-set composition sampling while Storage is visible.
/// Never changes selections, tracks inventories, or requests content.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_storage_active(replica: *mut Replica, active: bool) {
    if let Some(replica) = unsafe { replica.as_ref() } {
        let _ = replica.command.send(Command::StorageActive(active));
    }
}

/// Replace the transient visible-root request set. Never persists a choice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_storage_roots(replica: *mut Replica, roots: *const c_char) -> bool {
    let Some(replica) = (unsafe { replica.as_ref() }) else { return false; };
    if roots.is_null() { return false; }
    let Ok(json) = (unsafe { CStr::from_ptr(roots) }).to_str() else { return false; };
    let Ok(values) = serde_json::from_str::<Vec<String>>(json) else { return false; };
    if values.len() > collections::PAGE_SIZE { return false; }
    let mut roots = Vec::new();
    for value in values {
        let Ok(bytes) = hex::decode(value) else { return false; };
        let Ok(bytes) = <[u8; 32]>::try_from(bytes) else { return false; };
        roots.push(CollectionHandle::new(bytes));
    }
    roots.sort_unstable(); roots.dedup();
    replica.command.send(Command::Storage(roots)).is_ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_choose(replica: *mut Replica, root: *const c_char,
    expected: *const c_char, enabled: bool) -> bool {
    let Some(replica) = (unsafe { replica.as_ref() }) else { return false; };
    if root.is_null() || expected.is_null() { return false; }
    let Ok(root) = (unsafe { CStr::from_ptr(root) }).to_str() else { return false; };
    let Ok(expected) = (unsafe { CStr::from_ptr(expected) }).to_str() else { return false; };
    let Ok(bytes) = hex::decode(root) else { return false; };
    let Ok(bytes) = <[u8; 32]>::try_from(bytes) else { return false; };
    replica.command.send(Command::Choose(CollectionHandle::new(bytes), expected.to_owned(), enabled)).is_ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_status(replica: *const Replica) -> *mut c_char {
    let Some(replica) = (unsafe { replica.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let Ok(status) = replica.status.lock() else {
        return std::ptr::null_mut();
    };
    CString::new(serde_json::to_string(&*status).expect("status JSON"))
        .expect("JSON has no NUL")
        .into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_release(replica: *mut Replica) {
    if !replica.is_null() {
        let replica = unsafe { Box::from_raw(replica) };
        let _ = replica.command.send(Command::Stop);
    }
}
#[repr(C)]
pub struct CrownPoint {
    pub x: f64,
    pub y: f64,
}
fn crown_activity(began: f64, ended: f64) -> Option<erlkonig_crown::WorkActivity> {
    Some(erlkonig_crown::WorkActivity {
        began_at: Duration::try_from_secs_f64(began).ok()?,
        ended_at: (ended >= began).then(|| Duration::try_from_secs_f64(ended).ok()).flatten(),
    })
}

/// Stateless bridge to the same activity observer used by the native crown.
/// The caller retains only two timestamps; no replica
/// state or timer is created.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_crown_observe(seconds: f64, working: bool,
    began: *mut f64, ended: *mut f64) -> bool {
    let Ok(now) = Duration::try_from_secs_f64(seconds) else { return false; };
    let (Some(began), Some(ended)) = (unsafe { began.as_mut() }, unsafe { ended.as_mut() }) else { return false; };
    let mut activity = crown_activity(*began, *ended);
    if activity.is_some_and(|a| a.began_at > now || a.ended_at.is_some_and(|end| end > now)) { return false; }
    erlkonig_crown::observe(&mut activity, working, now);
    *began = activity.map_or(-1.0, |value| value.began_at.as_secs_f64());
    *ended = activity.and_then(|value| value.ended_at).map_or(-1.0, |value| value.as_secs_f64());
    true
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn erl_ios_crown(
    seconds: f64,
    began: f64,
    ended: f64,
    reduced: bool,
    attention: bool,
    points: *mut CrownPoint,
    capacity: usize,
) -> usize {
    let Ok(now) = Duration::try_from_secs_f64(seconds) else { return 0; };
    let frame = erlkonig_crown::parametric_frame(now, crown_activity(began, ended), reduced, attention);
    if points.is_null() || capacity < frame.points.len() {
        return frame.points.len();
    }
    let count = frame.points.len();
    for (index, point) in frame.points.into_iter().enumerate() {
        unsafe {
            points.add(index).write(CrownPoint {
                x: point.x,
                y: point.y,
            });
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    unsafe fn wait(
        replica: *mut Replica,
        predicate: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = unsafe { erl_ios_status(replica) };
            assert!(!text.is_null());
            let value: serde_json::Value =
                serde_json::from_slice(unsafe { CStr::from_ptr(text) }.to_bytes()).unwrap();
            unsafe {
                erl_ios_string_free(text);
            }
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
        let path = CString::new(root.path().to_str().unwrap()).unwrap();
        unsafe {
            let first = erl_ios_open(path.as_ptr());
            assert!(!first.is_null());
            let before = wait(first, |s| s["ready"] == true && s["replay_complete"] == true);
            erl_ios_active(first, true);
            let active = wait(first, |s| s["running"] == true);
            assert_eq!(active["selected"], 0);
            assert_eq!(active["network_started"], false);
            assert_eq!(active["bound_sockets"], 0);
            std::thread::sleep(Duration::from_millis(500));
            let idle = wait(first, |s| s["running"] == true);
            assert_eq!(idle["network_started"], false);
            assert_eq!(idle["bound_sockets"], 0);
            erl_ios_active(first, false);
            wait(first, |s| {
                s["running"] == false && s["message"] == "Replica paused"
            });
            erl_ios_release(first);
            let second = erl_ios_open(path.as_ptr());
            assert!(!second.is_null());
            let after = wait(second, |s| s["ready"] == true && s["replay_complete"] == true);
            assert_eq!(before["node_id"], after["node_id"]);
            erl_ios_active(second, true);
            wait(second, |s| s["running"] == true);
            erl_ios_active(second, false);
            wait(second, |s| {
                s["running"] == false && s["message"] == "Replica paused"
            });
            erl_ios_release(second);
        }
    }
    #[test]
    fn explicit_discovery_starts_empty_listener_and_survives_worker_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = CString::new(root.path().to_str().unwrap()).unwrap();
        unsafe {
            let first = erl_ios_open(path.as_ptr());
            assert!(!first.is_null());
            wait(first, |s| s["ready"] == true && s["replay_complete"] == true);
            erl_ios_active(first, true);
            let before = wait(first, |s| s["running"] == true);
            assert_eq!(before["network_started"], false);
            assert_eq!(before["selected"], 0);
            erl_ios_discover(first);
            let listening = wait(first, |s| {
                s["listening"] == true && s["network_started"] == true
            });
            assert_eq!(listening["selected"], 0);
            assert!(listening["bound_sockets"].as_u64().unwrap() > 0);
            erl_ios_active(first, false);
            wait(first, |s| {
                s["running"] == false && s["message"] == "Replica paused"
            });
            erl_ios_release(first);
            let second = erl_ios_open(path.as_ptr());
            assert!(!second.is_null());
            wait(second, |s| s["ready"] == true && s["replay_complete"] == true);
            erl_ios_active(second, true);
            let reopened = wait(second, |s| {
                s["listening"] == true && s["network_started"] == true
            });
            assert_eq!(reopened["node_id"], before["node_id"]);
            assert_eq!(reopened["selected"], 0);
            erl_ios_active(second, false);
            wait(second, |s| {
                s["running"] == false && s["message"] == "Replica paused"
            });
            erl_ios_release(second);
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
        let path = CString::new(dir.path().to_str().unwrap()).unwrap();
        let root_text = CString::new(hex::encode(root.raw)).unwrap();
        let unset = CString::new("unset").unwrap();
        let selected = CString::new("selected").unwrap();
        unsafe {
            let first = erl_ios_open(path.as_ptr());
            erl_ios_active(first, true);
            let available = wait(first, |s| s["collections"]["total"] == 1);
            assert_eq!(available["selected"], 0);
            assert_eq!(available["network_started"], false);
            assert!(erl_ios_choose(first, root_text.as_ptr(), unset.as_ptr(), true));
            wait(first, |s| s["selected"] == 1 && s["action_serial"] == 1);
            assert!(erl_ios_choose(first, root_text.as_ptr(), unset.as_ptr(), false));
            let stale = wait(first, |s| s["action_serial"] == 2);
            assert!(!stale["action_error"].is_null());
            assert_eq!(stale["selected"], 1);
            assert!(erl_ios_choose(first, root_text.as_ptr(), selected.as_ptr(), false));
            let off = wait(first, |s| s["selected"] == 0 && s["action_serial"] == 3);
            assert!(off["action_error"].is_null());
            erl_ios_active(first, false);
            wait(first, |s| s["running"] == false && s["message"] == "Replica paused");
            erl_ios_release(first);
            let second = erl_ios_open(path.as_ptr()); erl_ios_active(second, true);
            let reopened = wait(second, |s| s["collections"]["total"] == 1);
            assert_eq!(reopened["node_id"], available["node_id"]);
            assert_eq!(reopened["collections"]["rows"][0]["state"], "unselected");
            assert_eq!(reopened["selected"], 0);
            assert_eq!(reopened["network_started"], false);
            erl_ios_active(second, false);
            wait(second, |s| s["running"] == false && s["message"] == "Replica paused");
            erl_ios_release(second);
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
        let path = CString::new(dir.path().to_str().unwrap()).unwrap();
        let root_text = CString::new(hex::encode(root.raw)).unwrap();
        let unset = CString::new("unset").unwrap();
        // Inactive worker exercises the real ABI/choice/checkpoint path without
        // starting an endpoint or the known Mac CoreWLAN enrichment path.
        let (acknowledged, stopped, stale) = unsafe {
            let replica = erl_ios_open(path.as_ptr());
            wait(replica, |s| s["ready"] == true && s["replay_complete"] == true);
            assert!(erl_ios_choose(replica, root_text.as_ptr(), unset.as_ptr(), true));
            let first = wait(replica, |s| s["action_serial"] == 1);
            // Queue rapid valid choices without waiting for intermediate polls.
            for (expected, enabled) in [("selected", false), ("unselected", true), ("selected", false)] {
                let expected = CString::new(expected).unwrap();
                assert!(erl_ios_choose(replica, root_text.as_ptr(), expected.as_ptr(), enabled));
            }
            let stopped = wait(replica, |s| s["action_serial"] == 4);
            let outdated = CString::new("selected").unwrap();
            assert!(erl_ios_choose(replica, root_text.as_ptr(), outdated.as_ptr(), true));
            let stale = wait(replica, |s| s["action_serial"] == 5);
            erl_ios_release(replica);
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
        let mut replica = Replica { command: sender.clone(), status: status.clone(), activity_serial: AtomicU64::new(0) };
        let path = dir.path().to_owned();
        let thread = std::thread::Builder::new().stack_size(WORKER_STACK_BYTES).spawn(move || {
            worker_with_cleanup(path, receiver, status, |peer, local| {
                let was_open = peer.is_some();
                pause(peer, local)?;
                if was_open {
                    // Real lazy-peer shutdown/checkpoint happened. Inject only the
                    // native joined-timeout report, not an actual endpoint.
                    Err(CleanupFailure { shutdown: Some(JOINED_TRANSPORT_TIMEOUT.into()), flush: None, close: None }.into())
                } else { Ok(()) }
            });
        }).unwrap();
        unsafe {
            wait(&mut replica, |s| s["ready"] == true && s["replay_complete"] == true);
            sender.send(Command::Active(true, 0)).unwrap();
            let first = wait(&mut replica, |s| s["running"] == true && s["collections"]["total"] == 1);
            sender.send(Command::Active(false, 0)).unwrap();
            let paused = wait(&mut replica, |s| s["running"] == false && s["last_cleanup_error"].is_string());
            assert!(paused["last_cleanup_error"].as_str().unwrap().contains(JOINED_TRANSPORT_TIMEOUT));
            assert_eq!(paused["network_started"], false);
            sender.send(Command::Active(true, 0)).unwrap();
            wait(&mut replica, |s| s["running"] == true);
            // A foreground command already queued behind failing background
            // cleanup must still run; a subsequent page ack proves channel life.
            sender.send(Command::Active(false, 0)).unwrap();
            sender.send(Command::Active(true, 0)).unwrap();
            sender.send(Command::Page(32)).unwrap();
            let resumed = wait(&mut replica, |s| s["running"] == true && s["collections"]["offset"] == 32);
            assert_eq!(resumed["node_id"], first["node_id"]);
            assert_eq!(resumed["selected"], 0);
            assert_eq!(resumed["network_started"], false);
            sender.send(Command::Active(false, 0)).unwrap();
            wait(&mut replica, |s| s["running"] == false);
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
    fn crown_observer_ffi_preserves_rapid_resume_and_settles_at_canonical_rest() {
        fn points(now: f64, began: f64, ended: f64) -> Vec<(f64, f64)> {
            unsafe {
                let count = erl_ios_crown(now, began, ended, false, false, std::ptr::null_mut(), 0);
                let mut values: Vec<_> = (0..count).map(|_| CrownPoint { x: 0.0, y: 0.0 }).collect();
                assert_eq!(erl_ios_crown(now, began, ended, false, false, values.as_mut_ptr(), count), count);
                values.into_iter().map(|point| (point.x, point.y)).collect()
            }
        }
        let idle = points(0.0, -1.0, -1.0);
        assert!((idle[72].1 - 4.055).abs() < 1e-12);
        let (mut began, mut ended) = (-1.0, -1.0);
        unsafe {
            assert!(erl_ios_crown_observe(1.0, true, &mut began, &mut ended));
            assert_eq!(points(1.0, began, ended), idle);
            assert!(erl_ios_crown_observe(2.0, false, &mut began, &mut ended));
            let before_resume = points(2.4, began, ended);
            assert!(erl_ios_crown_observe(2.4, true, &mut began, &mut ended));
            assert_eq!(began, 1.0); // Swift's previous rising-edge reset changed this.
            assert_eq!(before_resume, points(2.4, began, ended));
            for (now, working) in [(2.5,false),(2.6,false),(2.7,true),(2.8,false),(2.9,true),(3.0,false)] {
                assert!(erl_ios_crown_observe(now, working, &mut began, &mut ended));
            }
            assert_eq!(ended, 3.0);
            assert!(erl_ios_crown_observe(4.0, false, &mut began, &mut ended));
            assert_eq!(ended, 3.0); // Repeated inactive polls cannot postpone landing.
            assert_eq!(points(4.6, began, ended), idle);
            assert_eq!(points(20.0, began, ended), idle);
            assert!(!erl_ios_crown_observe(f64::NAN, true, &mut began, &mut ended));
            assert_eq!((began, ended), (1.0, 3.0));
            assert!(!erl_ios_crown_observe(2.9, true, &mut began, &mut ended));
            assert_eq!((began, ended), (1.0, 3.0));
            assert!(erl_ios_crown_observe(4.6, true, &mut began, &mut ended));
            assert_eq!((began, ended), (4.6, -1.0));
            assert_eq!(points(4.6, began, ended), idle);
        }
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
                    status.network_started, status.error.clone(), status.collections.rows.clone())).unwrap();
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
            worker_with_cleanup(path, receiver, status, pause);
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
        for (rows, _, network_started, error, _) in &samples {
            assert!(!network_started);
            assert!(error.is_none(), "{error:?}");
            assert_eq!(rows.len(), 1);
        }
        // Actual shared page counts are already ready while the byte batch is unfinished.
        assert_eq!(samples[0].4.len(), 2);
        assert!(samples[0].4.iter().all(|row| row.counts == Some(storage::Counts { records: 1, direct_present: 1 })));
        assert!(samples[1].4.iter().all(|row| row.counts == Some(storage::Counts { records: 1, direct_present: 1 })));
        assert_eq!(samples[0].0[0].root, hex::encode(roots[0].raw));
        assert!(samples[0].1);
        assert!(samples[0].0[0].resident_bytes.is_none());
        // Changing scope completed A instead of throwing it away for B.
        assert_eq!(samples[1].0[0].root, hex::encode(roots[0].raw));
        assert!(!samples[1].1);
        assert!(samples[1].0[0].resident_bytes.is_some());
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
        let path = CString::new(dir.path().to_str().unwrap()).unwrap();
        let request = CString::new(serde_json::to_string(&vec![hex::encode(root.raw)]).unwrap()).unwrap();
        let too_many = CString::new(serde_json::to_string(&vec![hex::encode(root.raw); 33]).unwrap()).unwrap();
        let hidden = CString::new("[]").unwrap();
        unsafe {
            let replica = erl_ios_open(path.as_ptr()); erl_ios_active(replica, true);
            let before = wait(replica, |s| s["collections"]["total"] == 1);
            assert_eq!(before["storage"], serde_json::json!([]));
            assert_eq!(before["collections"]["rows"][0]["counts"]["records"], 1, "row counts require no Storage request");
            assert!(!erl_ios_storage_roots(replica, too_many.as_ptr()));
            assert!(erl_ios_storage_roots(replica, request.as_ptr()));
            let sampled = wait(replica, |s| s["storage"].as_array().is_some_and(|rows| rows.len() == 1));
            assert_eq!(sampled["collections"]["rows"][0]["counts"]["records"], 1);
            assert_eq!(sampled["collections"]["rows"][0]["counts"]["direct_present"], 1);
            assert!(sampled["storage"][0].get("counts").is_none());
            assert!(sampled["storage"][0]["resident_error"].is_null());
            assert_eq!(sampled["network_started"], false);
            assert!(erl_ios_storage_roots(replica, hidden.as_ptr()));
            wait(replica, |s| s["storage"] == serde_json::json!([]));
            erl_ios_active(replica, false);
            wait(replica, |s| s["running"] == false && s["message"] == "Replica paused");
            erl_ios_release(replica);
        }
    }

}

#[cfg(test)]
mod startup_tests;
