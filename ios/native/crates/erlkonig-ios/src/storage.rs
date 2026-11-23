//! Native snapshot statistics; access checks remain in the application.
use anyhow::{Result, ensure};
use ed25519_dalek::VerifyingKey;
use serde::Serialize;
#[cfg(test)]
use std::{cmp::Reverse, collections::{BTreeSet, BinaryHeap}};
#[cfg(test)]
use triblespace_core::{inline::encodings::hash::Handle, collection::{CollectionRead, CollectionRecordSelector, HeldRead}, repo::BlobStoreList};
use triblespace_net::storage_snapshot::{StorageSample, StorageSnapshot};
use triblespace_core::{
    capability::QuorumOutcome,
    collection::{self, CollectionHandle,
        descriptor, selection::{self, Selection}},
    prelude::*,
    repo::{CapabilityProofRead, StoreDependencies, StoreRead, pile::PileSnapshot},
    collection::observed_store::ObservedStore,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Counts {
    pub records: usize,
    pub direct_present: usize,
}
#[cfg(test)]
#[derive(Clone, Serialize)]
pub struct Observation {
    pub root: String,
    pub counts: Option<Counts>,
    pub error: Option<String>,
    /// Logical payload bytes in this snapshot's positive native held inventory.
    /// Shared content counts once here and can count in another root too.
    pub resident_bytes: Option<u64>,
    pub resident_error: Option<String>,
}

/// Optional held-inventory bytes; record counts live in snapshot-derived rows.
#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct ByteObservation {
    pub root: String,
    pub resident_bytes: Option<u64>,
    pub resident_error: Option<String>,
    pub pending: bool,
}

/// One root from one immutable local snapshot. Native record routes are bags;
/// deduplicate canonical assertions, not payload identities or derived ids.
/// Presence consults the index, not payload bytes or recursive references.
#[cfg(test)]
fn measure(snapshot: &PileSnapshot, root: CollectionHandle) -> Result<Counts> {
    let selectors = BTreeSet::from([CollectionRecordSelector::Foundations(root)]);
    let records: BTreeSet<_> = snapshot.select_records(&selectors)?.into_iter().collect();
    let mut direct_present = 0;
    for record in &records {
        let mut present = true;
        for handle in record.blob_references() {
            if !snapshot.contains_blob(handle)? { present = false; }
        }
        direct_present += usize::from(present);
    }
    Ok(Counts { records: records.len(), direct_present })
}

fn eligible(snapshot: &impl StoreRead, reader: VerifyingKey, root: CollectionHandle,
    facts: &TribleSet, proofs: &[triblespace_core::capability::CapabilityProof]) -> Result<()> {
        ensure!(root != selection::config_handle(reader), "Private node configuration");
        let state = selection::sync_selection(facts, root);
        let admitted = matches!(collection::collection_reader_is_admitted_by(snapshot, root, reader, proofs), Ok(QuorumOutcome::Met));
        ensure!(admitted || matches!(state, Selection::Selected | Selection::Conflicted(_)), "Collection access unavailable");
        let descriptor: TribleSet = snapshot.get(root)?;
        ensure!(descriptor::parents(&descriptor)?.is_empty(), "Inspect the parent collection");
    Ok(())
}

#[cfg(test)]
pub fn observe(snapshot: &PileSnapshot, reader: VerifyingKey, root: CollectionHandle) -> Observation {
    let result = (|| {
        eligible(snapshot, reader, root, &selection::config_facts(snapshot, reader)?, &snapshot.proofs()?.collect::<Result<Vec<_>, _>>()?)?;
        measure(snapshot, root)
    })();
    match result {
        Ok(counts) => Observation { root: hex::encode(root.raw), counts: Some(counts), error: None,
            resident_bytes: None, resident_error: None },
        Err(error) => Observation { root: hex::encode(root.raw), counts: None, error: Some(format!("{error:#}")),
            resident_bytes: None, resident_error: None },
    }
}

pub type Statistics = StorageSnapshot<PileSnapshot>;

pub struct IncrementalSample {
    pub authority: (PileSnapshot, StoreDependencies),
    rows: Vec<ByteObservation>,
    roots: Vec<CollectionHandle>,
    sample: Option<StorageSample<PileSnapshot>>,
}
impl IncrementalSample {
    pub fn new(snapshot: PileSnapshot, reader: VerifyingKey, roots: &[CollectionHandle], previous: Option<&Statistics>) -> Self {
        let mut accepted = Vec::new();
        let tracked = ObservedStore::new(snapshot.clone());
        let authority: Result<_> = (|| Ok((selection::config_facts(&tracked, reader)?,
            tracked.proofs()?.collect::<Result<Vec<_>, _>>()?)))();
        let rows = roots.iter().map(|root| {
            let resident_error = match &authority {
                Ok((facts, proofs)) => eligible(&tracked, reader, *root, facts, proofs).err().map(|e| format!("{e:#}")),
                Err(error) => Some(format!("{error:#}")),
            };
            if resident_error.is_none() { accepted.push(*root); }
            ByteObservation { root: hex::encode(root.raw), resident_bytes: None, pending: resident_error.is_none(), resident_error }
        }).collect();
        let mut job = Self { rows, roots: roots.to_vec(), sample: None,
            authority: (snapshot.clone(), tracked.dependencies()) };
        // Native snapshot rows already own counts, including replay prefixes.
        // Never trigger Statistics' generic Untracked record fallback here.
        match Statistics::begin(snapshot, &[], &accepted, previous) {
            Ok(sample) => job.sample = Some(sample),
            Err(error) => job.fail(error),
        }
        job
    }
    #[cfg(test)]
    pub fn observations(&self) -> &[ByteObservation] { &self.rows }
    pub fn pending_rows(&self, previous: &[ByteObservation]) -> Vec<ByteObservation> {
        let measured: std::collections::HashMap<_, _> = previous.iter()
            .filter_map(|r| Some((r.root.as_str(), r.resident_bytes?))).collect();
        let mut rows = self.rows.clone();
        for row in &mut rows {
            if row.pending { row.resident_bytes = measured.get(row.root.as_str()).copied(); }
        }
        rows
    }
    fn fail(&mut self, error: String) {
        for row in &mut self.rows {
            row.pending = false;
            if row.resident_error.is_none() { row.resident_error = Some(error.clone()); }
        }
        self.sample = None;
    }
    pub fn step(&mut self, budget: usize) -> Option<(Vec<ByteObservation>, Option<Statistics>)> {
        let Some(sample) = self.sample.as_mut() else { return Some((self.rows.clone(), None)); };
        match sample.step(budget) {
            Ok(None) => None,
            Err(error) => { self.fail(error); Some((self.rows.clone(), None)) }
            Ok(Some(summary)) => {
                for (row, root) in self.rows.iter_mut().zip(&self.roots) {
                    row.pending = false;
                    if row.resident_error.is_none() { row.resident_bytes = summary.resident_bytes(*root); }
                }
                Some((self.rows.clone(), Some(summary)))
            }
        }
    }
}

pub struct IncrementalCompositionSample {
    roots: Vec<(CollectionHandle, String)>,
    sample: StorageSample<PileSnapshot>,
}
impl IncrementalCompositionSample {
    pub fn new(snapshot: PileSnapshot, reader: VerifyingKey, previous: Option<&Statistics>) -> Result<Self> {
        let roots = crate::collections::chart_roots(&snapshot, reader)?;
        let scope: Vec<_> = roots.iter().map(|(root, _)| *root).collect();
        let sample = Statistics::begin(snapshot, &[], &scope, previous).map_err(anyhow::Error::msg)?;
        Ok(Self { roots, sample })
    }
    pub fn step(&mut self, budget: usize) -> Option<(Composition, Option<Statistics>)> {
        match self.sample.step(budget) {
            Ok(None) => None,
            Err(error) => Some((Composition::unavailable(error), None)),
            Ok(Some(summary)) => {
                let totals = summary.composition();
                let segments = self.roots.iter().filter_map(|(root, name)| summary.exclusive_bytes(*root)
                    .map(|bytes| Segment { root: hex::encode(root.raw), name: name.clone(), bytes })).collect();
                let result = Composition { segments, shared_bytes: totals.shared_bytes, known_bytes: totals.known_bytes,
                    known_inventories: totals.known_inventories, unknown_inventories: totals.unknown_inventories, error: None };
                Some((result, Some(summary)))
            }
        }
    }
}

// Disposable UI sampling cursors, not a catalog. All rows share one immutable
// snapshot; only completed sums escape. Dropping a batch cancels its work.
#[cfg(test)]
struct ResidentJob {
    row: usize,
    handles: Box<dyn Iterator<Item = [u8; 32]>>,
    bytes: u64,
    done: bool,
}
#[cfg(test)]
pub struct Sample {
    snapshot: PileSnapshot,
    rows: Vec<Observation>,
    jobs: Vec<ResidentJob>,
    next: usize,
}
#[cfg(test)]
impl Sample {
    pub fn new(snapshot: PileSnapshot, reader: VerifyingKey, roots: &[CollectionHandle]) -> Self {
        let mut rows = Vec::new();
        let mut jobs = Vec::new();
        for root in roots {
            let row = observe(&snapshot, reader, *root);
            if row.counts.is_some() {
                if let Some(held) = snapshot.held(*root) {
                    jobs.push(ResidentJob { row: rows.len(), handles: Box::new(held.into_iter_ordered()), bytes: 0, done: false });
                }
            }
            rows.push(row);
        }
        Self { snapshot, rows, jobs, next: 0 }
    }

    /// At most `budget` index lookups, round-robin across the visible roots.
    /// None means sampling is unfinished, never a partial byte total.
    pub fn step(&mut self, mut budget: usize) -> Option<Vec<Observation>> {
        while budget > 0 && self.jobs.iter().any(|job| !job.done) {
            let index = self.next % self.jobs.len();
            self.next = (index + 1) % self.jobs.len();
            let job = &mut self.jobs[index];
            if job.done { continue; }
            for _ in 0..budget.min(128) {
                let Some(raw) = job.handles.next() else {
                    self.rows[job.row].resident_bytes = Some(job.bytes);
                    job.done = true;
                    break;
                };
                budget -= 1;
                let result = (|| -> Result<()> {
                    let info = self.snapshot.blob_info(Inline::<Handle<blobencodings::UnknownBlob>>::new(raw))?
                        .ok_or_else(|| anyhow::anyhow!("Held content absent from sampled index"))?;
                    job.bytes = job.bytes.checked_add(info.length)
                        .ok_or_else(|| anyhow::anyhow!("Resident byte count overflow"))?;
                    Ok(())
                })();
                if let Err(error) = result {
                    self.rows[job.row].resident_error = Some(format!("{error:#}"));
                    job.done = true;
                    break;
                }
            }
        }
        self.jobs.iter().all(|job| job.done).then(|| self.rows.clone())
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Segment {
    pub root: String,
    pub name: String,
    pub bytes: u64,
}
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Composition {
    pub segments: Vec<Segment>,
    pub shared_bytes: u64,
    pub known_bytes: u64,
    pub known_inventories: usize,
    pub unknown_inventories: usize,
    pub error: Option<String>,
}
impl Composition {
    pub fn unavailable(error: impl std::fmt::Display) -> Self {
        Self { error: Some(error.to_string()), ..Self::default() }
    }
}

/// One transient ordered merge. Memory is one native cursor/head per eligible
/// known inventory, not one entry per blob. Overlap is among known inventories.
#[cfg(test)]
pub struct CompositionSample {
    snapshot: PileSnapshot,
    cursors: Vec<Box<dyn Iterator<Item = [u8; 32]>>>,
    heads: BinaryHeap<Reverse<([u8; 32], usize)>>,
    pending: Option<([u8; 32], usize, usize)>,
    result: Composition,
}
#[cfg(test)]
impl CompositionSample {
    pub fn new(snapshot: PileSnapshot, reader: VerifyingKey) -> Result<Self> {
        let rows = crate::collections::project(&snapshot, reader, 0, usize::MAX)?.rows;
        let mut sample = Self { snapshot, cursors: Vec::new(), heads: BinaryHeap::new(), pending: None,
            result: Composition::default() };
        for row in rows {
            let mut raw = [0; 32];
            hex::decode_to_slice(&row.root, &mut raw)?;
            if let Some(held) = sample.snapshot.held(CollectionHandle::new(raw)) {
                let index = sample.cursors.len();
                let mut cursor = held.into_iter_ordered();
                if let Some(head) = cursor.next() { sample.heads.push(Reverse((head, index))); }
                sample.cursors.push(Box::new(cursor));
                sample.result.segments.push(Segment { root: row.root, name: row.name, bytes: 0 });
                sample.result.known_inventories += 1;
            } else { sample.result.unknown_inventories += 1; }
        }
        Ok(sample)
    }

    /// Budget counts both cursor advances and index lookups. Never publishes a
    /// partial result; a later observation cannot restart this coherent sample.
    pub fn step(&mut self, mut budget: usize) -> Option<Composition> {
        while budget > 0 && (self.pending.is_some() || !self.heads.is_empty()) {
            if let Some((raw, owner, memberships)) = self.pending {
                if self.heads.peek().is_none_or(|Reverse((next, _))| *next != raw) {
                    budget -= 1;
                    let add = (|| -> Result<()> {
                        let bytes = self.snapshot.blob_info(Inline::<Handle<blobencodings::UnknownBlob>>::new(raw))?
                            .ok_or_else(|| anyhow::anyhow!("Held content absent from sampled index"))?.length;
                        self.result.known_bytes = self.result.known_bytes.checked_add(bytes)
                            .ok_or_else(|| anyhow::anyhow!("Known content byte count overflow"))?;
                        let bucket = if memberships == 1 { &mut self.result.segments[owner].bytes }
                            else { &mut self.result.shared_bytes };
                        *bucket = bucket.checked_add(bytes).ok_or_else(|| anyhow::anyhow!("Segment byte count overflow"))?;
                        Ok(())
                    })();
                    if let Err(error) = add { return Some(Composition::unavailable(error)); }
                    self.pending = None;
                    continue;
                }
            }
            let Reverse((raw, owner)) = self.heads.pop().expect("pending group or nonempty head set");
            budget -= 1;
            match self.pending.as_mut() {
                Some((_, _, memberships)) => *memberships += 1,
                None => self.pending = Some((raw, owner, 1)),
            }
            if let Some(next) = self.cursors[owner].next() { self.heads.push(Reverse((next, owner))); }
        }
        (self.pending.is_none() && self.heads.is_empty()).then(|| self.result.clone())
    }
}

#[cfg(test)]
mod tests;
