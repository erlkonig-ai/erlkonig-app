//! Thin ERP command boundary for financial assertions.
use std::io;
use std::path::Path;

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};
use hifitime::Epoch;
use triblespace::core::inline::TryToInline;
use triblespace::prelude::Id;

use super::*;

#[derive(Subcommand)]
pub enum Command {
    /// Record an attributed source claim. No settlement or legal inference.
    Record(RecordArgs),
    /// Reconcile explicitly selected cash evidence under declared hypotheses.
    ReconcileCash(ReconcileArgs),
    /// List observations, retaining unknowns and competing claims.
    List {
        #[arg(long, value_parser = parse_id)]
        entity: Option<Id>,
        /// stock, transfer, capacity, commitment, period-total, or an opaque kind ID.
        #[arg(long, value_parser = parse_kind)]
        kind: Option<Id>,
        /// Match this exact inclusive civil boundary; unknown boundaries do not match.
        #[arg(long, value_parser = CivilDate::parse)]
        period_start: Option<CivilDate>,
        /// Match this exact exclusive civil boundary.
        #[arg(long, value_parser = CivilDate::parse)]
        period_end: Option<CivilDate>,
        /// Inclusive lower bound on a stated stock as-of date.
        #[arg(long, requires = "as_of_through", value_parser = CivilDate::parse)]
        as_of_from: Option<CivilDate>,
        /// Inclusive upper bound on a stated stock as-of date.
        #[arg(long, requires = "as_of_from", value_parser = CivilDate::parse)]
        as_of_through: Option<CivilDate>,
        /// Start of requested half-open period; only stated boundaries prove overlap.
        #[arg(long, requires = "overlap_end", value_parser = CivilDate::parse)]
        overlap_start: Option<CivilDate>,
        /// Exclusive end of requested period; touching intervals do not overlap.
        #[arg(long, requires = "overlap_start", value_parser = CivilDate::parse)]
        overlap_end: Option<CivilDate>,
    },
    /// Show every stated alternative and any uninterpreted facts.
    Show {
        #[arg(value_parser = parse_id)]
        id: Id,
    },
}

#[derive(Args)]
pub struct ReconcileArgs {
    /// Exact cash-account ID; neither a party label nor a credit facility.
    #[arg(long, value_parser = parse_id)]
    account: Id,
    /// Explicit Stock observation ID; no automatic consecutive-balance choice.
    #[arg(long, value_parser = parse_id)]
    opening: Id,
    #[arg(long, value_parser = parse_id)]
    closing: Id,
    #[arg(long, value_parser = parse_id)]
    balance_measure: Id,
    #[arg(long, value_parser = parse_id)]
    transfer_measure: Id,
    /// Opening end-of-day date; movements on this date are excluded.
    #[arg(long, value_parser = CivilDate::parse)]
    from: CivilDate,
    /// Closing end-of-day date; movements on this date are included.
    #[arg(long, value_parser = CivilDate::parse)]
    through: CivilDate,
    /// Required hypothesis: end-of-day-signed-sender-to-receiver.
    #[arg(long, value_parser = parse_cash_convention)]
    convention: reconcile::CashConvention,
    /// Required hypothesis: subject-identifies-movement. General subjects do not imply this.
    #[arg(long, value_parser = parse_movement_identity)]
    movement_identity: reconcile::MovementIdentity,
    /// Opt-in only for legs whose one known endpoint is the selected account.
    #[arg(long, value_parser = parse_counterparty_hypothesis)]
    counterparty_hypothesis: Option<reconcile::CounterpartyHypothesis>,
    /// Selected Transfer claim ID. Repeat for each evidence claim.
    #[arg(long, value_parser = parse_id, required_unless_present = "no_movements", conflicts_with = "no_movements")]
    movement: Vec<Id>,
    /// Explicitly select no movement evidence; does not assert that no movement occurred.
    #[arg(long, conflicts_with = "movement")]
    no_movements: bool,
}

fn parse_cash_convention(text: &str) -> Result<reconcile::CashConvention> {
    match text {
        "end-of-day-signed-sender-to-receiver" => {
            Ok(reconcile::CashConvention::EndOfDaySignedSenderToReceiver)
        }
        _ => bail!("expected explicit end-of-day-signed-sender-to-receiver hypothesis"),
    }
}
fn parse_movement_identity(text: &str) -> Result<reconcile::MovementIdentity> {
    match text {
        "subject-identifies-movement" => Ok(reconcile::MovementIdentity::SubjectIdentifiesMovement),
        _ => bail!("expected explicit subject-identifies-movement hypothesis"),
    }
}

fn parse_counterparty_hypothesis(text: &str) -> Result<reconcile::CounterpartyHypothesis> {
    match text {
        "unknown-counterparty-is-not-selected-account" => {
            Ok(reconcile::CounterpartyHypothesis::UnknownCounterpartyIsNotSelectedAccount)
        }
        _ => bail!("expected explicit unknown-counterparty-is-not-selected-account hypothesis"),
    }
}

#[derive(Args)]
pub struct RecordArgs {
    /// Five known kind names, or an exact source-specific kind ID.
    #[arg(long, value_parser = parse_kind)]
    kind: Id,
    /// Required for an opaque source-specific kind; exact provider wording.
    #[arg(long)]
    kind_label: Option<String>,
    /// Source-specific event date; makes no claim about settlement.
    #[arg(long, value_parser = CivilDate::parse)]
    event_date: Option<CivilDate>,
    /// Exact opaque subject ID (organization, ERP party, account, etc.).
    #[arg(long, value_parser = parse_id)]
    entity: Id,
    /// Optional shared measure identity. Never inferred from wording.
    #[arg(long, value_parser = parse_id)]
    measure: Option<Id>,
    /// Source wording: e.g. "credit-limit increase", not merely "credit".
    #[arg(long)]
    measure_label: String,
    #[arg(long, value_parser = parse_id)]
    claimant: Option<Id>,
    /// Exact issuer wording, also required when an issuer ID is known.
    #[arg(long)]
    claimant_label: String,
    #[arg(
        long,
        required_unless_present = "source_entity",
        conflicts_with = "source_entity"
    )]
    source_uri: Option<String>,
    /// Exact evidence entity, e.g. an imported ERP snapshot, not its party.
    #[arg(long, value_parser = parse_id)]
    source_entity: Option<Id>,
    #[arg(long)]
    locator: String,
    #[arg(long, value_parser = CivilDate::parse)]
    publication_date: Option<CivilDate>,
    /// Source observation instant, ISO-8601 with explicit time scale/zone.
    #[arg(long, value_parser = parse_instant)]
    source_observed_at: Option<IntervalValue>,
    #[arg(long)]
    source_date_wording: Option<String>,
    /// Exact signed EUR figure, e.g. '-12.34 EUR' or '1/3 EUR'. No FX conversion.
    #[arg(long, allow_hyphen_values = true)]
    amount: Option<String>,
    /// Preserve original figure wording, including unsupported currencies.
    #[arg(long)]
    amount_wording: Option<String>,
    #[arg(long)]
    scope: Option<String>,
    #[arg(long)]
    basis: Option<String>,
    /// Source's attributed classification, including any legal wording.
    #[arg(long)]
    classification: Option<String>,
    #[arg(long, value_parser = CivilDate::parse)]
    as_of: Option<CivilDate>,
    #[arg(long, value_parser = CivilDate::parse)]
    occurred: Option<CivilDate>,
    #[arg(long, value_parser = parse_id)]
    sender: Option<Id>,
    #[arg(long, value_parser = parse_id)]
    receiver: Option<Id>,
    #[arg(long)]
    period_label: Option<String>,
    #[arg(long, value_parser = CivilDate::parse)]
    period_start: Option<CivilDate>,
    /// Exclusive end. Retain original period wording separately.
    #[arg(long, value_parser = CivilDate::parse)]
    period_end: Option<CivilDate>,
    #[arg(long, value_parser = CivilDate::parse)]
    due: Option<CivilDate>,
    /// Existing Relations persona ID. Labels and signing keys are not identities.
    #[arg(long, value_parser = parse_id)]
    actor: Id,
    /// Recording instant; defaults to now. Separate from source/reporting dates.
    #[arg(long, value_parser = parse_instant)]
    recorded_at: Option<IntervalValue>,
}

pub fn parse_id(text: &str) -> Result<Id> {
    Id::from_hex(text).ok_or_else(|| anyhow!("expected an exact 32-hex nonzero entity ID"))
}

fn parse_kind(text: &str) -> Result<Id> {
    Ok(match text {
        "stock" => schema::KIND_STOCK,
        "transfer" => schema::KIND_TRANSFER,
        "capacity" => schema::KIND_CAPACITY,
        "commitment" => schema::KIND_COMMITMENT,
        "period-total" => schema::KIND_PERIOD_TOTAL,
        other => return parse_id(other),
    })
}

fn parse_instant(text: &str) -> Result<IntervalValue> {
    // Require an explicit clock and zone rather than silently interpreting a
    // date-only report label as an instant. hifitime handles ISO/RFC3339 UTC.
    if !text.split_once('T').is_some_and(|(_, clock)| {
        clock.ends_with('Z') || clock.contains('+') || clock.contains('-')
    }) {
        bail!("expected ISO-8601 instant with T and explicit Z or signed offset");
    }
    let epoch = text
        .parse::<Epoch>()
        .map_err(|e| anyhow!("invalid instant: {e}"))?;
    (epoch, epoch)
        .try_to_inline()
        .map_err(|e| anyhow!("unrepresentable instant: {e:?}"))
}

impl RecordArgs {
    fn into_input(self) -> Result<(Observation, Recording)> {
        let known = [
            schema::KIND_STOCK,
            schema::KIND_TRANSFER,
            schema::KIND_CAPACITY,
            schema::KIND_COMMITMENT,
            schema::KIND_PERIOD_TOTAL,
        ]
        .contains(&self.kind);
        if known && (self.kind_label.is_some() || self.event_date.is_some()) {
            bail!("--kind-label and --event-date are for opaque source-specific kinds");
        }
        if self.kind != schema::KIND_STOCK && self.as_of.is_some() {
            bail!("--as-of is a stock measurement date");
        }
        if self.kind != schema::KIND_TRANSFER
            && (self.occurred.is_some() || self.sender.is_some() || self.receiver.is_some())
        {
            bail!("only transfer claims have --occurred, --sender, or --receiver");
        }
        if self.kind != schema::KIND_COMMITMENT && self.due.is_some() {
            bail!("--due belongs to a commitment");
        }
        let has_period = [
            schema::KIND_CAPACITY,
            schema::KIND_COMMITMENT,
            schema::KIND_PERIOD_TOTAL,
        ]
        .contains(&self.kind);
        if !has_period
            && (self.period_label.is_some()
                || self.period_start.is_some()
                || self.period_end.is_some())
        {
            bail!("period fields belong to capacity, commitment, or period-total claims");
        }
        let period = Period {
            wording: self.period_label,
            start: self.period_start,
            end_exclusive: self.period_end,
        };
        let kind = if self.kind == schema::KIND_STOCK {
            ObservationKind::Stock { as_of: self.as_of }
        } else if self.kind == schema::KIND_TRANSFER {
            ObservationKind::Transfer {
                occurred: self.occurred,
                sender: self.sender,
                receiver: self.receiver,
            }
        } else if self.kind == schema::KIND_CAPACITY {
            ObservationKind::Capacity { period }
        } else if self.kind == schema::KIND_COMMITMENT {
            ObservationKind::Commitment {
                period,
                due: self.due,
            }
        } else if self.kind == schema::KIND_PERIOD_TOTAL {
            ObservationKind::PeriodTotal { period }
        } else {
            ObservationKind::SourceSpecific {
                kind: self.kind,
                wording: self
                    .kind_label
                    .ok_or_else(|| anyhow!("opaque source kind requires --kind-label"))?,
                event_date: self.event_date,
            }
        };
        let reference = match (self.source_uri, self.source_entity) {
            (Some(uri), None) => EvidenceRef::Uri(uri),
            (None, Some(entity)) => EvidenceRef::Entity(entity),
            _ => bail!("supply exactly one source reference"),
        };
        let at = match self.recorded_at {
            Some(at) => at,
            None => {
                let now = Epoch::now().map_err(|e| anyhow!("read clock: {e}"))?;
                (now, now)
                    .try_to_inline()
                    .map_err(|e| anyhow!("unrepresentable current instant: {e:?}"))?
            }
        };
        let observation = Observation {
            entity: self.entity,
            measure: self.measure,
            measure_label: self.measure_label,
            claimant: self.claimant,
            claimant_label: self.claimant_label,
            evidence: Evidence {
                reference,
                locator: self.locator,
                publication_date: self.publication_date,
                observed_at: self.source_observed_at,
                date_wording: self.source_date_wording,
            },
            amount: self
                .amount
                .as_deref()
                .map(str::parse::<Amount<Euro>>)
                .transpose()?,
            amount_wording: self.amount_wording,
            scope: self.scope,
            basis: self.basis,
            classification: self.classification,
            kind,
        };
        Ok((
            observation,
            Recording {
                actor: self.actor,
                at,
            },
        ))
    }
}

pub fn run(
    command: Command,
    pile: &Path,
    key: Option<&Path>,
    handle: CollectionHandle,
) -> Result<()> {
    match command {
        Command::Record(args) => {
            let (input, recording) = args.into_input()?;
            let id = record(pile, key, handle, &input, &recording)?;
            println!("recorded financial observation {id:X}");
        }
        Command::ReconcileCash(args) => {
            if args.movement.is_empty() && !args.no_movements {
                bail!("supply --movement or explicit --no-movements");
            }
            let selection = reconcile::CashSelection {
                account: args.account,
                opening: args.opening,
                closing: args.closing,
                movements: args.movement.into_iter().collect(),
                balance_measure: args.balance_measure,
                transfer_measure: args.transfer_measure,
                from: args.from,
                through: args.through,
                convention: args.convention,
                movement_identity: args.movement_identity,
                counterparty_hypothesis: args.counterparty_hypothesis,
            };
            let snapshot = crate::revolver::read_snapshot(pile, key, handle)?;
            let finding = reconcile::reconcile_cash::<Euro>(&snapshot.facts, &selection)?;
            let mut out = io::stdout().lock();
            reconcile::write_finding(&finding, &mut out)?;
            writeln!(
                out,
                "Selected source evidence (including quoted scope and basis):"
            )?;
            let mut ids = selection.movements.clone();
            ids.extend([selection.opening, selection.closing]);
            for id in ids {
                if exists!((attribute: Id, value: Inline<UnknownInline>),
                    pattern!(&snapshot.facts, [{ id @ ?attribute: ?value }]))
                {
                    show(&snapshot.facts, &snapshot.reader, id, &mut out)?;
                } else {
                    writeln!(out, "  claim {id:X}: no source facts available")?;
                }
            }
        }
        Command::List {
            entity,
            kind,
            period_start,
            period_end,
            as_of_from,
            as_of_through,
            overlap_start,
            overlap_end,
        } => {
            let as_of = match (as_of_from, as_of_through) {
                (Some(from), Some(through)) => Some(AsOfWindow::new(from, through)?),
                (None, None) => None,
                _ => bail!("supply both --as-of-from and --as-of-through"),
            };
            let overlap = match (overlap_start, overlap_end) {
                (Some(start), Some(end)) => Some(PeriodOverlap::new(start, end)?),
                (None, None) => None,
                _ => bail!("supply both --overlap-start and --overlap-end"),
            };
            let snapshot = crate::revolver::read_snapshot(pile, key, handle)?;
            let ids = observations(
                &snapshot.facts,
                &Filter {
                    entity,
                    kind,
                    period_start,
                    period_end,
                    as_of,
                    overlap,
                },
            );
            let mut out = io::stdout().lock();
            for id in &ids {
                show(&snapshot.facts, &snapshot.reader, *id, &mut out)?;
            }
            writeln!(out, "{} observation(s)", ids.len())?;
        }
        Command::Show { id } => {
            let snapshot = crate::revolver::read_snapshot(pile, key, handle)?;
            show(
                &snapshot.facts,
                &snapshot.reader,
                id,
                &mut io::stdout().lock(),
            )?;
        }
    }
    Ok(())
}
