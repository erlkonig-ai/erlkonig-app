use super::{ModelSource, volume_capacity};
use crate::{
    CollectionPages, CollectionSnapshot, Replica, Status,
    collections::{PAGE_SIZE, Page, Row},
};
use erlkonig_crown::WorkActivity;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Screen {
    Crown,
    Collections,
    Storage,
    Sync,
}
impl Screen {
    pub fn from_index(index: isize) -> Self {
        match index {
            1 => Self::Collections,
            2 => Self::Storage,
            3 => Self::Sync,
            _ => Self::Crown,
        }
    }
    pub fn list(self) -> bool {
        matches!(self, Self::Collections | Self::Storage)
    }
}

#[derive(Clone)]
struct Choice {
    root: String,
    name: String,
    expected: String,
    enabled: bool,
}
struct PageRead {
    basis: Status,
    verified: Option<CollectionSnapshot>,
    result: anyhow::Result<Vec<Page>>,
    readers: Vec<(usize, CollectionPages)>,
}

pub(super) struct Model {
    pub status: Status,
    replica: Option<Replica>,
    local_only: bool,
    seen_revision: Option<u64>,
    pub foreground: bool,
    pub activity_serial: u64,
    pub screen: Screen,
    pub began: Instant,
    pub activity: Option<WorkActivity>,
    pub copied: Option<Instant>,
    pub list_error: Option<String>,
    choice_error: Option<String>,
    pending: VecDeque<Choice>,
    flight: Option<(Choice, u64)>,
    displayed: Option<Status>,
    readers: Vec<(usize, CollectionPages)>,
    visible_pages: Vec<usize>,
    reading: Option<Receiver<PageRead>>,
    requests: VecDeque<usize>,
    pages: VecDeque<Arc<Page>>,
    latest_list: Arc<std::sync::Mutex<Option<CollectionSnapshot>>>,
    validating: Option<Receiver<(CollectionSnapshot, CollectionSnapshot, bool)>>,
    checked: Option<(CollectionSnapshot, CollectionSnapshot)>,
    pub total: usize,
    pub access_pending: usize,
    pub list_loaded: bool,
    visible_roots: Vec<String>,
    storage_active: bool,
    path: Option<PathBuf>,
    volume_read: Option<Receiver<Result<(u64, u64), String>>>,
    volume_next: Instant,
    pub capacity: Option<(u64, u64)>,
    pub volume_error: Option<String>,
    pub table_epoch: u64,
    #[cfg(feature = "synthetic-proof")]
    proof_reader: Option<Receiver<Result<PathBuf, String>>>,
    #[cfg(feature = "synthetic-proof")]
    pub fixture: Option<super::fixture::Fixture>,
}

impl Model {
    pub fn new(source: ModelSource) -> Self {
        let now = Instant::now();
        let mut model = Self {
            status: Status::default(),
            replica: None,
            local_only: false,
            seen_revision: None,
            foreground: true,
            activity_serial: 0,
            screen: Screen::Crown,
            began: now,
            activity: None,
            copied: None,
            list_error: None,
            choice_error: None,
            pending: VecDeque::new(),
            flight: None,
            displayed: None,
            readers: Vec::new(),
            visible_pages: vec![0],
            reading: None,
            requests: VecDeque::new(),
            pages: VecDeque::new(),
            latest_list: Default::default(),
            validating: None,
            checked: None,
            total: 0,
            access_pending: 0,
            list_loaded: false,
            visible_roots: Vec::new(),
            storage_active: false,
            path: None,
            volume_read: None,
            volume_next: now,
            capacity: None,
            volume_error: None,
            table_epoch: 1,
            #[cfg(feature = "synthetic-proof")]
            proof_reader: None,
            #[cfg(feature = "synthetic-proof")]
            fixture: None,
        };
        match source {
            ModelSource::Live(root) => model.open(root),
            ModelSource::Error(error) => model.status.error = Some(error),
            #[cfg(feature = "synthetic-proof")]
            ModelSource::Proof { reader } => {
                model.local_only = true;
                model.proof_reader = Some(reader);
            }
            #[cfg(feature = "synthetic-proof")]
            ModelSource::Visual(args) => {
                let fixture = super::fixture::Fixture::new(&args, &mut model.status);
                model.fixture = Some(fixture);
            }
        }
        model
    }
    fn open(&mut self, root: PathBuf) {
        self.invalidate_window();
        self.seen_revision = None;
        self.pending.clear();
        self.flight = None;
        match Replica::open(&root) {
            Ok(replica) => {
                self.activity_serial = if self.foreground && self.local_only {
                    replica.set_local_active()
                } else {
                    replica.set_active(self.foreground)
                };
                self.replica = Some(replica);
                self.path = Some(root);
            }
            Err(error) => self.status.error = Some(error.to_string()),
        }
    }
    pub fn list_status(&self) -> &Status {
        self.displayed.as_ref().unwrap_or(&self.status)
    }

    fn invalidate_window(&mut self) {
        // Dropping the receiver prevents results from a previous owner or authorization
        // scope being adopted, even if two replicas have the same local serial.
        self.reading = None;
        self.readers.clear();
        self.displayed = None;
        self.validating = None;
        self.checked = None;
        self.pages.clear();
        self.total = 0;
        self.access_pending = 0;
        self.list_loaded = false;
        self.requests.clear();
        self.list_error = None;
        self.table_epoch += 1;
    }
    pub fn visible_pages(&mut self, indices: impl Iterator<Item = usize>) {
        let mut offsets: Vec<_> = indices.map(|n| n / PAGE_SIZE * PAGE_SIZE).collect();
        offsets.sort_unstable();
        offsets.dedup();
        offsets.truncate(3);
        if !offsets.is_empty() {
            self.visible_pages = offsets;
        }
    }
    pub fn visual(&self) -> bool {
        #[cfg(feature = "synthetic-proof")]
        {
            self.fixture.is_some()
        }
        #[cfg(not(feature = "synthetic-proof"))]
        {
            false
        }
    }
    pub fn reduced(&self) -> bool {
        #[cfg(feature = "synthetic-proof")]
        {
            self.fixture.as_ref().is_some_and(|f| f.reduced)
        }
        #[cfg(not(feature = "synthetic-proof"))]
        {
            false
        }
    }
    pub fn acknowledged(&self) -> bool {
        self.visual()
            || (self.activity_serial != 0 && self.status.activity_serial == self.activity_serial)
    }
    pub fn accepts_intent(&self) -> bool {
        self.foreground
            && self.status.replay_complete
            && self.list_status().replay_complete
            && self.status.error.is_none()
            && (self.replica.is_some() || self.visual())
            && !self.local_only
    }
    pub fn can_choose(&self) -> bool {
        self.accepts_intent() && self.acknowledged()
    }
    pub fn set_active(&mut self, active: bool) {
        if self.foreground == active {
            return;
        }
        self.foreground = active;
        if !active {
            self.invalidate_window();
        }
        if let Some(r) = &self.replica {
            self.activity_serial = if active && self.local_only {
                r.set_local_active()
            } else {
                r.set_active(active)
            };
        }
        self.sync_storage();
        self.table_epoch += 1;
    }
    pub fn set_screen(&mut self, screen: Screen) {
        if self.screen.list() && !screen.list() {
            self.invalidate_window();
        }
        self.screen = screen;
        self.visible_pages = vec![0];
        self.requests.clear();
        self.sync_storage();
        if screen.list() && !self.list_loaded {
            self.demand(0);
        }
        self.table_epoch += 1;
    }
    fn sync_storage(&mut self) {
        let active = self.foreground && self.screen == Screen::Storage;
        if self.storage_active == active {
            return;
        }
        self.storage_active = active;
        if let Some(r) = &self.replica {
            if !self.local_only {
                r.set_storage_active(active);
            }
        }
        if !active {
            self.storage_roots(Vec::new());
        } else {
            self.volume_next = Instant::now();
        }
    }
    pub fn storage_roots(&mut self, mut roots: Vec<String>) {
        roots.sort_unstable();
        roots.dedup();
        roots.truncate(PAGE_SIZE);
        if roots == self.visible_roots {
            return;
        }
        if let Some(r) = &self.replica {
            if !self.local_only && !r.set_storage_roots(&roots) {
                self.choice_error = Some("Could not request storage sample".into());
            }
        }
        #[cfg(feature = "synthetic-proof")]
        if let Some(f) = &self.fixture {
            self.status.storage = Arc::new(f.storage(&roots));
            self.table_epoch += 1;
        }
        self.visible_roots = roots;
    }
    pub fn row(&self, index: usize) -> Option<&Row> {
        self.pages
            .iter()
            .find(|p| index >= p.offset && index - p.offset < p.rows.len())
            .map(|p| &p.rows[index - p.offset])
    }
    pub fn row_for_root(&self, root: &str) -> Option<&Row> {
        self.pages
            .iter()
            .flat_map(|p| &p.rows)
            .find(|r| r.root == root)
    }
    pub fn demand(&mut self, index: usize) {
        let offset = index / PAGE_SIZE * PAGE_SIZE;
        if !self.pages.iter().any(|p| p.offset == offset) && !self.requests.contains(&offset) {
            if self.requests.len() == 3 {
                self.requests.pop_front();
            }
            self.requests.push_back(offset);
        }
    }
    fn install_page(&mut self, page: Page) {
        self.total = page.total;
        self.access_pending = page.pending;
        self.list_loaded = true;
        self.pages.retain(|p| p.offset != page.offset);
        self.requests.retain(|v| *v != page.offset);
        self.pages.push_back(Arc::new(page));
        while self.pages.len() > 3 {
            self.pages.pop_front();
        }
        self.table_epoch += 1;
    }
    fn pages_tick(&mut self) {
        use std::sync::mpsc::TryRecvError;
        if !self.foreground || !self.screen.list() {
            return;
        }
        self.verify_display();
        if let Some(rx) = &self.reading {
            match rx.try_recv() {
                Ok(reply) => {
                    self.reading = None;
                    self.readers = reply.readers;
                    if self.compatible(&reply.basis) && reply.verified.is_some() {
                        match reply.result {
                            Ok(pages) => {
                                self.checked =
                                    reply.basis.list_snapshot.clone().zip(reply.verified);
                                self.displayed = Some(reply.basis);
                                self.pages.clear();
                                for page in pages {
                                    self.install_page(page);
                                }
                            }
                            Err(e) => {
                                self.list_error = Some(e.to_string());
                                self.table_epoch += 1;
                            }
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.reading = None;
                    self.list_error = Some("Collection reader stopped".into());
                    self.table_epoch += 1;
                }
                Err(TryRecvError::Empty) => return,
            }
        }
        if self.list_error.is_some() {
            return;
        }
        #[cfg(feature = "synthetic-proof")]
        if let Some(f) = &self.fixture {
            let request = self.requests.pop_front().or_else(|| {
                self.visible_pages
                    .iter()
                    .find(|n| !self.pages.iter().any(|p| p.offset == **n))
                    .copied()
            });
            if let Some(offset) = request {
                self.install_page(f.page(offset));
            }
            return;
        }
        let Some(snapshot) = self.status.list_snapshot.clone() else {
            return;
        };
        let mut offsets = self.visible_pages.clone();
        // During the UIKit layout pass the old visible paths can still be reported.
        // Include missing cells actively requested by the new layout in the same bounded read.
        for offset in &self.requests {
            if !offsets.contains(offset) {
                if offsets.len() == 3 {
                    offsets.remove(0);
                }
                offsets.push(*offset);
            }
        }
        if self
            .displayed
            .as_ref()
            .and_then(|s| s.list_snapshot.as_ref())
            == Some(&snapshot)
            && offsets
                .iter()
                .all(|&n| self.pages.iter().any(|p| p.offset == n || n >= p.total))
        {
            self.displayed = Some(self.status.clone());
            self.requests.clear();
            return;
        }
        let basis = self.status.clone();
        let latest_list = self.latest_list.clone();
        let mut readers = std::mem::take(&mut self.readers);
        readers.retain(|(o, _)| offsets.contains(o));
        let (tx, rx) = mpsc::channel();
        self.reading = Some(rx);
        // Pile graph queries have a deeper debug stack requirement than UIKit callbacks.
        let started = std::thread::Builder::new()
            .name("ios-visible-collections".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let result: anyhow::Result<Vec<Page>> = offsets
                    .into_iter()
                    .map(|offset| {
                        let index = readers
                            .iter()
                            .position(|(o, _)| *o == offset)
                            .unwrap_or_else(|| {
                                readers.push((offset, CollectionPages::default()));
                                readers.len() - 1
                            });
                        readers[index].1.read(&snapshot, offset)
                    })
                    .collect();
                let latest = latest_list.lock().ok().and_then(|guard| guard.clone());
                let verified = latest.filter(|latest| match &result {
                    Ok(pages) => pages.iter().all(|p| p.metadata_current(latest)),
                    Err(_) => snapshot == *latest,
                });
                let _ = tx.send(PageRead {
                    basis,
                    verified,
                    result,
                    readers,
                });
            });
        if let Err(e) = started {
            self.reading = None;
            self.list_error = Some(e.to_string());
            self.table_epoch += 1;
        }
    }

    fn compatible(&self, older: &Status) -> bool {
        match (&older.list_snapshot, &self.status.list_snapshot) {
            (Some(old), Some(current)) => {
                current.same_replica(old) && current.complete == old.complete
            }
            _ => false,
        }
    }
    fn verify_display(&mut self) {
        use std::sync::mpsc::TryRecvError;
        if let Some(rx) = &self.validating {
            match rx.try_recv() {
                Ok((basis, checked, valid)) => {
                    self.validating = None;
                    if self
                        .displayed
                        .as_ref()
                        .and_then(|s| s.list_snapshot.as_ref())
                        == Some(&basis)
                    {
                        if !valid {
                            self.invalidate_window();
                            return;
                        }
                        self.checked = Some((basis, checked));
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    self.invalidate_window();
                    return;
                }
                Err(TryRecvError::Empty) => return,
            }
        }
        let Some(basis) = self
            .displayed
            .as_ref()
            .and_then(|s| s.list_snapshot.clone())
        else {
            return;
        };
        let Some(current) = self.status.list_snapshot.clone() else {
            return;
        };
        if basis == current
            || self
                .checked
                .as_ref()
                .is_some_and(|(b, c)| *b == basis && *c == current)
        {
            return;
        }
        // Validate visible facts independently of an expensive new page query, so a
        // revocation need not wait for that query. One check coalesces later prefixes.
        let pages = self.pages.clone();
        let (tx, rx) = mpsc::channel();
        self.validating = Some(rx);
        let started = std::thread::Builder::new()
            .name("ios-visible-access".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                let valid = pages.iter().all(|p| p.metadata_current(&current));
                let _ = tx.send((basis, current, valid));
            });
        if started.is_err() {
            self.invalidate_window();
        }
    }
    pub fn desired(&self, row: &Row) -> bool {
        self.pending
            .iter()
            .find(|c| c.root == row.root)
            .map_or(row.selected, |c| c.enabled)
    }
    pub fn choose(&mut self, row: &Row, enabled: bool) {
        if !self.accepts_intent() || row.state == "unknown" || (enabled && !row.readable) {
            return;
        }
        self.choice_error = None;
        if let Some(latest) = self.pending.iter_mut().find(|c| c.root == row.root) {
            latest.enabled = enabled;
        } else {
            self.pending.push_back(Choice {
                root: row.root.clone(),
                name: row.name.clone(),
                expected: row.state.clone(),
                enabled,
            });
        }
        self.table_epoch += 1;
        self.dispatch_choice();
    }
    fn dispatch_choice(&mut self) {
        if !self.can_choose() || self.flight.is_some() {
            return;
        }
        let Some(choice) = self.pending.front().cloned() else {
            return;
        };
        let serial = self.status.action_serial;
        #[cfg(feature = "synthetic-proof")]
        if let Some(fixture) = &mut self.fixture {
            self.status.action_error =
                fixture.choose(&choice.root, &choice.expected, choice.enabled);
            self.status.action_serial += 1;
            self.status.selected = fixture.rows.iter().filter(|r| r.selected).count();
            self.flight = Some((choice, serial));
            self.refresh_pages();
            return;
        }
        if self
            .replica
            .as_ref()
            .is_some_and(|r| r.choose(&choice.root, &choice.expected, choice.enabled))
        {
            self.flight = Some((choice, serial));
        } else {
            self.choice_error = Some(format!("{}: Could not queue this choice", choice.name));
            self.pending.clear();
            self.table_epoch += 1;
        }
    }
    #[cfg(feature = "synthetic-proof")]
    fn refresh_pages(&mut self) {
        self.requests.clear();
        for page in self.pages.drain(..) {
            self.requests.push_back(page.offset);
        }
        self.list_loaded = false;
        self.list_error = None;
        if self.requests.is_empty() && self.screen.list() {
            self.requests.push_back(0);
        }
        self.table_epoch += 1;
    }
    fn acknowledge(&mut self) {
        if self.status.error.is_some() {
            self.pending.clear();
            self.flight = None;
            return;
        }
        if self
            .flight
            .as_ref()
            .is_some_and(|(_, s)| *s != self.status.action_serial)
        {
            let (done, _) = self.flight.take().unwrap();
            if let Some(error) = &self.status.action_error {
                self.choice_error = Some(format!("{}: {error}", done.name));
            }
            self.pending.retain_mut(|c| {
                if c.root != done.root {
                    return true;
                }
                if c.enabled == done.enabled || self.status.action_error.is_some() {
                    return false;
                }
                c.expected = if done.enabled {
                    "selected"
                } else {
                    "unselected"
                }
                .into();
                true
            });
            self.table_epoch += 1;
        }
        self.dispatch_choice();
    }
    pub fn discover(&mut self) {
        if self.can_choose() {
            #[cfg(feature = "synthetic-proof")]
            if self.fixture.is_some() {
                self.status.listening = true;
                self.table_epoch += 1;
                return;
            }
            if let Some(r) = &self.replica {
                r.discover();
            }
        }
    }
    pub fn error(&self) -> Option<&str> {
        self.choice_error
            .as_deref()
            .or(self.list_error.as_deref())
            .or(self.status.action_error.as_deref())
            .or(self.status.error.as_deref())
            .or(self.status.last_cleanup_error.as_deref())
    }
    pub fn tick(&mut self) {
        #[cfg(feature = "synthetic-proof")]
        if let Some(result) = self.proof_reader.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.proof_reader = None;
            match result {
                Ok(path) => self.open(path),
                Err(e) => self.status.error = Some(e),
            }
        }
        if let Some(next) = self
            .replica
            .as_ref()
            .and_then(|r| r.read_after(self.seen_revision))
        {
            let old = &self.status;
            let list_changed = next.list_snapshot != old.list_snapshot;
            let dirty = match self.screen {
                Screen::Storage => {
                    !Arc::ptr_eq(&next.storage, &old.storage)
                        || next.composition.as_ref().map(Arc::as_ptr)
                            != old.composition.as_ref().map(Arc::as_ptr)
                }
                Screen::Sync => {
                    next.network.as_ref().map(Arc::as_ptr) != old.network.as_ref().map(Arc::as_ptr)
                }
                _ => false,
            };
            if dirty
                || next.action_serial != old.action_serial
                || next.error != old.error
                || next.replay_complete != old.replay_complete
                || next.listening != old.listening
                || next.last_cleanup_error != old.last_cleanup_error
            {
                self.table_epoch += 1;
            }
            self.seen_revision = Some(next.revision);
            self.status = next;
            if list_changed {
                if let Ok(mut latest) = self.latest_list.lock() {
                    *latest = self.status.list_snapshot.clone();
                }
                self.list_error = None;
                if self
                    .displayed
                    .as_ref()
                    .is_some_and(|previous| !self.compatible(previous))
                {
                    self.invalidate_window();
                }
            }
        }
        self.acknowledge();
        self.pages_tick();
        self.volume_tick();
        let working = self.foreground
            && self.acknowledged()
            && self.status.running
            && self.status.selected > 0;
        erlkonig_crown::observe(&mut self.activity, working, self.began.elapsed());
    }
    fn volume_tick(&mut self) {
        let now = Instant::now();
        if let Some(result) = self.volume_read.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.volume_read = None;
            if self.storage_active {
                self.volume_next = now + Duration::from_secs(5);
                self.set_volume(result);
            }
        }
        if !self.storage_active || now < self.volume_next || self.volume_read.is_some() {
            return;
        }
        #[cfg(feature = "synthetic-proof")]
        if self.fixture.is_some() {
            self.volume_next = now + Duration::from_secs(5);
            self.set_volume(Ok((128_000_000_000, 32_000_000_000)));
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.volume_read = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(volume_capacity(&path));
        });
    }
    fn set_volume(&mut self, result: Result<(u64, u64), String>) {
        let (cap, error) = match result {
            Ok((total, free)) if total > 0 && free <= total => (Some((total, free)), None),
            Ok(_) => (None, Some("Invalid capacity".into())),
            Err(e) => (None, Some(e)),
        };
        if self.capacity != cap || self.volume_error != error {
            self.capacity = cap;
            self.volume_error = error;
            self.table_epoch += 1;
        }
    }
    pub fn replaying(&self) -> bool {
        !self.status.replay_complete
            || self
                .status
                .replay_progress
                .is_some_and(|r| r.prefix_bytes < r.observed_bytes)
    }
    pub fn progress(&self) -> Option<(u64, u64)> {
        let s = &self.status;
        if !s.ready || s.error.is_some() {
            return None;
        }
        if self.replaying() {
            return s
                .replay_progress
                .map(|p| (p.prefix_bytes, p.observed_bytes));
        }
        s.sync_progress
            .filter(|p| p.unknown_roots == 0 && p.direct_present < p.records)
            .map(|p| (p.direct_present, p.records))
    }
    pub fn fresh_network(&self) -> bool {
        self.foreground
            && self.acknowledged()
            && self.status.error.is_none()
            && self
                .status
                .network
                .as_ref()
                .is_some_and(|n| n.age_ms() <= 5000)
    }
    pub fn phase(&self) -> String {
        let s = &self.status;
        if s.error.is_some() {
            return "Needs attention".into();
        }
        if !self.foreground {
            return "Paused".into();
        }
        let replay = s
            .replay_progress
            .filter(|r| self.visual() || r.activity_serial == self.activity_serial);
        if !self.acknowledged() && replay.is_none() {
            return if s.replay_complete {
                "Resuming"
            } else {
                "Reading local data"
            }
            .into();
        }
        if !s.ready || !s.replay_complete {
            if let Some(r) = replay.and_then(|r| r.records_per_second) {
                return if r.measured_at.elapsed() > Duration::from_secs(5) {
                    format!(
                        "Reading local data · last {} local pile records/s · waiting for an update",
                        r.records_per_second
                    )
                } else {
                    format!(
                        "Reading local data · {} local pile records/s",
                        r.records_per_second
                    )
                };
            }
            return "Reading local data".into();
        }
        if let Some(n) = &s.network {
            if !self.fresh_network() {
                return "Waiting for update".into();
            }
            return match n.connections.len() {
                0 if s.listening => "Listening".into(),
                0 => "No connected nodes".into(),
                1 => "1 connected node".into(),
                count => format!("{count} connected nodes"),
            };
        }
        if s.running && (s.listening || s.selected > 0) {
            "Starting network"
        } else if s.running {
            "Network idle"
        } else {
            "Local only"
        }
        .into()
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
