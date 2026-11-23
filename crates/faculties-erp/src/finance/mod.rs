//! Attributed financial assertions over the existing ERP collection.
//!
//! Writer inputs are not a loaded store model. Readers ask the relations at
//! the point of use, accept opaque subject IDs, and retain competing claims.
//! A kind never determines a quantity's meaning: credit-limit *increase* and
//! credit-limit *level*, for example, retain their distinct source wording.
//! No cross-claim arithmetic or legal classification is inferred here.

pub mod cli;
pub mod reconcile;
pub mod schema;

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use anybytes::View;
use anyhow::{anyhow, bail, Context, Result};
use triblespace::core::collection::{records::CollectionHandle, CollectionStoreExt};
use triblespace::core::inline::encodings::money::{Amount, Euro};
use triblespace::core::inline::TryToInline;
use triblespace::core::metadata;
use triblespace::core::query::{intersectionconstraint::IntersectionConstraint, Constraint};
use triblespace::core::repo::{BlobStoreGet, SnapshotSource};
use triblespace::prelude::*;

use crate::access::{load_signer, open_collection, open_pile_strict};
use crate::revolver::{IntervalValue, TextHandle};

pub mod civil_day;
pub use civil_day::{CivilDate, CivilDay};

#[derive(Clone, Debug, Default)]
pub struct Period {
    pub wording: Option<String>,
    pub start: Option<CivilDate>,
    pub end_exclusive: Option<CivilDate>,
}

#[derive(Clone, Debug)]
pub enum EvidenceRef {
    Uri(String),
    Entity(Id),
}

#[derive(Clone, Debug)]
pub struct Evidence {
    pub reference: EvidenceRef,
    pub locator: String,
    pub publication_date: Option<CivilDate>,
    pub observed_at: Option<IntervalValue>,
    pub date_wording: Option<String>,
}

/// Supported writer views, not an exhaustive taxonomy imposed on readers.
#[derive(Clone, Debug)]
pub enum ObservationKind {
    /// A caller-supplied source kind, without assigning cash-flow, debt, or
    /// accounting semantics. No universal sixth kind is minted.
    SourceSpecific {
        kind: Id,
        wording: String,
        event_date: Option<CivilDate>,
    },
    Stock {
        as_of: Option<CivilDate>,
    },
    Transfer {
        occurred: Option<CivilDate>,
        sender: Option<Id>,
        receiver: Option<Id>,
    },
    Capacity {
        period: Period,
    },
    Commitment {
        period: Period,
        due: Option<CivilDate>,
    },
    PeriodTotal {
        period: Period,
    },
}

impl ObservationKind {
    pub fn id(&self) -> Id {
        match self {
            Self::SourceSpecific { kind, .. } => *kind,
            Self::Stock { .. } => schema::KIND_STOCK,
            Self::Transfer { .. } => schema::KIND_TRANSFER,
            Self::Capacity { .. } => schema::KIND_CAPACITY,
            Self::Commitment { .. } => schema::KIND_COMMITMENT,
            Self::PeriodTotal { .. } => schema::KIND_PERIOD_TOTAL,
        }
    }
}

/// An immutable source-attributed proposition. IDs refer to existing entities
/// without recomputation, name matching, or identity adjudication. `entity`
/// can name a Relations organization, imported ERP party anchor, account, etc.
/// `evidence` can separately name the precise imported ERP snapshot/document.
#[derive(Clone, Debug)]
pub struct Observation {
    pub entity: Id,
    pub measure: Option<Id>,
    pub measure_label: String,
    pub claimant: Option<Id>,
    pub claimant_label: String,
    pub evidence: Evidence,
    pub amount: Option<Amount<Euro>>,
    pub amount_wording: Option<String>,
    pub scope: Option<String>,
    pub basis: Option<String>,
    pub classification: Option<String>,
    pub kind: ObservationKind,
}

/// The recording actor is an existing Relations persona ID, never inferred
/// from a signing key. This is separate from the source issuer's identity.
#[derive(Clone, Debug)]
pub struct Recording {
    pub actor: Id,
    pub at: IntervalValue,
}

pub fn record_fragment(input: &Observation, recording: &Recording) -> Result<Fragment> {
    for (name, text) in [
        ("measure wording", &input.measure_label),
        ("claimant wording", &input.claimant_label),
        ("source locator", &input.evidence.locator),
    ] {
        if text.trim().is_empty() {
            bail!("{name} must not be empty");
        }
    }
    if let EvidenceRef::Uri(uri) = &input.evidence.reference {
        if uri.trim().is_empty() {
            bail!("source URI must not be empty");
        }
    }
    let (source_kind_wording, source_event_date) = match &input.kind {
        ObservationKind::SourceSpecific {
            wording,
            event_date,
            ..
        } => {
            if wording.trim().is_empty() {
                bail!("source kind wording must not be empty");
            }
            (Some(wording.clone()), event_date.as_ref())
        }
        _ => (None, None),
    };
    let (as_of, occurred, sender, receiver, period, due) = match &input.kind {
        ObservationKind::SourceSpecific { .. } => (None, None, None, None, None, None),
        ObservationKind::Stock { as_of } => (as_of.as_ref(), None, None, None, None, None),
        ObservationKind::Transfer {
            occurred,
            sender,
            receiver,
        } => (None, occurred.as_ref(), *sender, *receiver, None, None),
        ObservationKind::Capacity { period } | ObservationKind::PeriodTotal { period } => {
            (None, None, None, None, Some(period), None)
        }
        ObservationKind::Commitment { period, due } => {
            (None, None, None, None, Some(period), due.as_ref())
        }
    };
    if let Some(period) = period {
        if let (Some(start), Some(end)) = (&period.start, &period.end_exclusive) {
            if start >= end {
                bail!("period end must be after its inclusive start");
            }
        }
    }
    let (source_uri, source_entity) = match &input.evidence.reference {
        EvidenceRef::Uri(uri) => (Some(uri.clone()), None),
        EvidenceRef::Entity(id) => (None, Some(*id)),
    };
    let amount_attribute = schema::amount::<Euro>();
    let amount = input
        .amount
        .as_ref()
        .map(|v| v.try_to_inline())
        .transpose()
        .map_err(|error| anyhow!("amount is outside exact currency encoding: {error:?}"))?;
    let mut fragment = entity! {
        metadata::tag: input.kind.id(),
        schema::source_kind_wording?: source_kind_wording,
        schema::source_event_date?: source_event_date,
        schema::entity: input.entity,
        schema::measure?: input.measure,
        schema::measure_label: input.measure_label.clone(),
        schema::claimant?: input.claimant,
        schema::claimant_label: input.claimant_label.clone(),
        schema::source_uri?: source_uri,
        schema::evidence?: source_entity,
        schema::locator: input.evidence.locator.clone(),
        schema::publication_date?: input.evidence.publication_date.as_ref(),
        schema::source_observed_at?: input.evidence.observed_at,
        schema::source_date_wording?: input.evidence.date_wording.clone(),
        amount_attribute?: amount,
        schema::amount_wording?: input.amount_wording.clone(),
        schema::scope?: input.scope.clone(),
        schema::basis?: input.basis.clone(),
        schema::classification?: input.classification.clone(),
        schema::as_of?: as_of,
        schema::occurred?: occurred,
        schema::sender?: sender,
        schema::receiver?: receiver,
        schema::period_start?: period.and_then(|p| p.start.as_ref()),
        schema::period_end?: period.and_then(|p| p.end_exclusive.as_ref()),
        schema::period_label?: period.and_then(|p| p.wording.clone()),
        schema::due?: due,
    };
    let id = fragment.root().expect("entity macro returns one root");
    // A recording is a relation, so union cannot cross-pair one actor with
    // another recorder's time. Its identity does not alter the proposition.
    fragment += entity! {
        schema::recording_of: id,
        schema::recorded_by: recording.actor,
        metadata::created_at: recording.at,
    };
    let (_, facts, metafacts, blobs) = fragment.into_parts();
    Ok(Fragment::rooted_from_parts(id, facts, metafacts, blobs))
}

/// Append to the same native collection as ERP imports, without modifying any
/// imported anchor, source snapshot, allocation, or private original.
pub fn record(
    pile: &Path,
    key: Option<&Path>,
    handle: CollectionHandle,
    input: &Observation,
    recording: &Recording,
) -> Result<Id> {
    let fragment = record_fragment(input, recording)?;
    let id = fragment.root().expect("record fragment has one root");
    let signer = load_signer(pile, key)?;
    let mut pile = open_pile_strict(pile)?;
    let snapshot = pile.snapshot().context("freeze ERP pile observation")?;
    let collection = open_collection(&snapshot, handle, &signer, true)?;
    let result = pile
        .commit(collection, &signer, fragment)
        .context("publish financial observation");
    let close = pile.close().context("close ERP pile");
    match (result, close) {
        (Ok(_), Ok(())) => Ok(id),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(close)) => {
            Err(error.context(format!("also failed to close pile: {close}")))
        }
    }
}

/// Closed civil-day window for stock measurement dates.
#[derive(Clone, Debug)]
pub struct AsOfWindow {
    from: CivilDate,
    through: CivilDate,
}
impl AsOfWindow {
    pub fn new(from: CivilDate, through: CivilDate) -> Result<Self> {
        if from > through {
            bail!("as-of window ends before its start");
        }
        Ok(Self { from, through })
    }
}

/// Nonempty half-open query period, [start, end_exclusive).
#[derive(Clone, Debug)]
pub struct PeriodOverlap {
    start: CivilDate,
    end_exclusive: CivilDate,
}
impl PeriodOverlap {
    pub fn new(start: CivilDate, end_exclusive: CivilDate) -> Result<Self> {
        if start >= end_exclusive {
            bail!("overlap period must have an end after its start");
        }
        Ok(Self {
            start,
            end_exclusive,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub entity: Option<Id>,
    /// Opaque kind ID; readers also accept kinds this writer does not know.
    pub kind: Option<Id>,
    /// Match exact stated inclusive/exclusive boundaries.
    pub period_start: Option<CivilDate>,
    pub period_end: Option<CivilDate>,
    pub as_of: Option<AsOfWindow>,
    pub overlap: Option<PeriodOverlap>,
}

/// Query observation IDs directly. Unknown dates remain visible without a
/// date filter; they cannot prove a requested date-range or period overlap.
/// Ranged queries project typed days before IDs, so malformed date values are
/// skipped by the query conversion boundary, not decoded and filtered in Rust.
pub fn observations(facts: &TribleSet, filter: &Filter) -> BTreeSet<Id> {
    // Sentinels participate only in query comparison. They are never written
    // as dates or supplied as alleged missing boundaries.
    let minimum = CivilDate::parse("0001-01-01")
        .expect("valid domain minimum")
        .to_inline();
    let maximum = CivilDate::parse("9999-12-31")
        .expect("valid domain maximum")
        .to_inline();
    // pattern! allocates local variables through the surrounding find!
    // context, so each query constructs its own selection constraints here.
    match (&filter.as_of, &filter.overlap) {
        (None, None) => find!(observation: Id, {
            let mut clauses: Vec<Box<dyn Constraint<'_> + Send + Sync + '_>> = vec![Box::new(
                pattern!(facts, [{ ?observation @ schema::entity: _?subject }]),
            )];
            if let Some(entity) = filter.entity {
                clauses.push(Box::new(
                    pattern!(facts, [{ ?observation @ schema::entity: entity }]),
                ));
            }
            if let Some(kind) = filter.kind {
                clauses.push(Box::new(
                    pattern!(facts, [{ ?observation @ metadata::tag: kind }]),
                ));
            }
            if let Some(start) = &filter.period_start {
                clauses.push(Box::new(
                    pattern!(facts, [{ ?observation @ schema::period_start: start }]),
                ));
            }
            if let Some(end) = &filter.period_end {
                clauses.push(Box::new(
                    pattern!(facts, [{ ?observation @ schema::period_end: end }]),
                ));
            }
            Arc::new(IntersectionConstraint::new(clauses))
        }).collect(),
        (Some(window), None) => find!((observation: Id, at: CivilDate), and!(
            {
                let mut clauses: Vec<Box<dyn Constraint<'_> + Send + Sync + '_>> = vec![Box::new(
                    pattern!(facts, [{ ?observation @ schema::entity: _?subject }]),
                )];
                if let Some(entity) = filter.entity {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::entity: entity }]),
                    ));
                }
                if let Some(kind) = filter.kind {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ metadata::tag: kind }]),
                    ));
                }
                if let Some(start) = &filter.period_start {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::period_start: start }]),
                    ));
                }
                if let Some(end) = &filter.period_end {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::period_end: end }]),
                    ));
                }
                Arc::new(IntersectionConstraint::new(clauses))
            },
            pattern!(facts, [{ ?observation @ schema::as_of: ?at }]),
            facts.value_in_range(at, (&window.from).to_inline(), (&window.through).to_inline()),
        )).map(|(id, _)| id).collect(),
        (None, Some(period)) => find!((observation: Id, start: CivilDate, end: CivilDate), and!(
            {
                let mut clauses: Vec<Box<dyn Constraint<'_> + Send + Sync + '_>> = vec![Box::new(
                    pattern!(facts, [{ ?observation @ schema::entity: _?subject }]),
                )];
                if let Some(entity) = filter.entity {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::entity: entity }]),
                    ));
                }
                if let Some(kind) = filter.kind {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ metadata::tag: kind }]),
                    ));
                }
                if let Some(start) = &filter.period_start {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::period_start: start }]),
                    ));
                }
                if let Some(end) = &filter.period_end {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::period_end: end }]),
                    ));
                }
                Arc::new(IntersectionConstraint::new(clauses))
            },
            pattern!(facts, [{ ?observation @ schema::period_start: ?start, schema::period_end: ?end }]),
            facts.value_in_range(start, minimum, civil_day::before(&period.end_exclusive)),
            facts.value_in_range(end, civil_day::after(&period.start), maximum),
        )).map(|(id, _, _)| id).collect(),
        (Some(window), Some(period)) => find!((observation: Id, at: CivilDate, start: CivilDate, end: CivilDate), and!(
            {
                let mut clauses: Vec<Box<dyn Constraint<'_> + Send + Sync + '_>> = vec![Box::new(
                    pattern!(facts, [{ ?observation @ schema::entity: _?subject }]),
                )];
                if let Some(entity) = filter.entity {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::entity: entity }]),
                    ));
                }
                if let Some(kind) = filter.kind {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ metadata::tag: kind }]),
                    ));
                }
                if let Some(start) = &filter.period_start {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::period_start: start }]),
                    ));
                }
                if let Some(end) = &filter.period_end {
                    clauses.push(Box::new(
                        pattern!(facts, [{ ?observation @ schema::period_end: end }]),
                    ));
                }
                Arc::new(IntersectionConstraint::new(clauses))
            },
            pattern!(facts, [{ ?observation @ schema::as_of: ?at,
                schema::period_start: ?start, schema::period_end: ?end }]),
            facts.value_in_range(at, (&window.from).to_inline(), (&window.through).to_inline()),
            facts.value_in_range(start, minimum, civil_day::before(&period.end_exclusive)),
            facts.value_in_range(end, civil_day::after(&period.start), maximum),
        )).map(|(id, _, _, _)| id).collect(),
    }
}

/// Print all stated alternatives. Unfamiliar attributes remain inspectable as
/// raw facts, including unsupported currencies; they never invalidate a read.
pub fn show<B: BlobStoreGet, W: Write>(
    facts: &TribleSet,
    blobs: &B,
    id: Id,
    out: &mut W,
) -> Result<()> {
    let raw: Vec<_> = find!((attribute: Id, value: Inline<UnknownInline>),
        pattern!(facts, [{ id @ ?attribute: ?value }]))
    .collect();
    if raw.is_empty() {
        bail!("no facts for observation {id:X}");
    }
    writeln!(out, "observation {id:X}")?;
    let mut displayed = BTreeSet::new();
    for (label, attribute) in [
        ("subject", &*schema::entity),
        ("kind", &*metadata::tag),
        ("measure", &*schema::measure),
        ("claimant", &*schema::claimant),
        ("source entity", &*schema::evidence),
        ("sender", &*schema::sender),
        ("receiver", &*schema::receiver),
    ] {
        displayed.insert(attribute.id());
        for value in find!(value: Id, pattern!(facts, [{ id @ attribute: ?value }])) {
            writeln!(out, "  {label}: {value:X}")?;
        }
    }
    for (label, attribute) in [
        ("source kind wording", &*schema::source_kind_wording),
        ("measure wording", &*schema::measure_label),
        ("claimant wording", &*schema::claimant_label),
        ("source URI", &*schema::source_uri),
        ("locator", &*schema::locator),
        ("scope", &*schema::scope),
        ("basis", &*schema::basis),
        ("classification (source claim)", &*schema::classification),
        ("amount wording", &*schema::amount_wording),
        ("period wording", &*schema::period_label),
        ("source date wording", &*schema::source_date_wording),
    ] {
        displayed.insert(attribute.id());
        for handle in find!(handle: TextHandle, pattern!(facts, [{ id @ attribute: ?handle }])) {
            match blobs.get::<View<str>, _>(handle) {
                Ok(text) => writeln!(out, "  {label}: {}", &*text)?,
                Err(error) => writeln!(out, "  {label}: [unavailable blob {handle:?}: {error}]")?,
            }
        }
    }
    for (label, attribute) in [
        ("source event date", &*schema::source_event_date),
        ("as of", &*schema::as_of),
        ("occurred", &*schema::occurred),
        ("period start (inclusive)", &*schema::period_start),
        ("period end (exclusive)", &*schema::period_end),
        ("due", &*schema::due),
        ("source publication date", &*schema::publication_date),
    ] {
        displayed.insert(attribute.id());
        for value in find!(value: CivilDate, pattern!(facts, [{ id @ attribute: ?value }])) {
            writeln!(out, "  {label}: {value}")?;
        }
    }
    for (label, attribute) in [("source observed at", &*schema::source_observed_at)] {
        displayed.insert(attribute.id());
        for value in find!(value: (hifitime::Epoch, hifitime::Epoch), pattern!(facts, [{ id @ attribute: ?value }]))
        {
            writeln!(out, "  {label}: {} .. {}", value.0, value.1)?;
        }
    }
    for (receipt, actor, at) in find!((receipt: Id, actor: Id, at: (hifitime::Epoch, hifitime::Epoch)),
        pattern!(facts, [{ ?receipt @ schema::recording_of: id,
            schema::recorded_by: ?actor, metadata::created_at: ?at }]))
    {
        writeln!(
            out,
            "  recorded by {actor:X} at {} .. {} (receipt {receipt:X})",
            at.0, at.1
        )?;
    }
    let amount_attribute = schema::amount::<Euro>();
    let mut decoded = false;
    for amount in find!(amount: Amount<Euro>, pattern!(facts, [{ id @ amount_attribute: ?amount }]))
    {
        decoded = true;
        writeln!(out, "  amount: {amount}")?;
    }
    if decoded {
        displayed.insert(amount_attribute.id());
    } else {
        writeln!(out, "  amount: no decoded EUR amount; may be unstated or unsupported (see wording/raw facts)")?;
    }
    writeln!(out, "  Omitted fields are unstated in this view; no zero, period, scope, or identity is inferred.")?;
    for (attribute, value) in raw {
        if !displayed.contains(&attribute) {
            writeln!(out, "  uninterpreted {attribute:X}: {:02X?}", value.raw)?;
        }
    }
    Ok(())
}
