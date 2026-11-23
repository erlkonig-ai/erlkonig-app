//! A conditional cash equation over explicitly selected evidence.
//!
//! These structures are calculation inputs and output, never a loaded-store
//! catalogue. No facts are changed, no claim is chosen by recency, and no
//! identity or comparability is inferred from wording. A missing or competing
//! modeled consequence is a finding, not a rejection of the collection.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use anyhow::{bail, Result};
use triblespace::core::inline::encodings::money::{Amount, CurrencyUnit};
use triblespace::core::metadata;
use triblespace::core::query::Constraint;
use triblespace::prelude::*;

use super::{civil_day, schema, CivilDate, CivilDay};

/// Explicit interpretation of this calculation, not an assertion already
/// established by a date-only `as_of` or by the general Transfer kind.
#[derive(Clone, Copy, Debug)]
pub enum CashConvention {
    /// Both balances are end-of-day. Movements occur in (from, through]. A
    /// positive amount follows sender -> receiver; a negative amount reverses it.
    EndOfDaySignedSenderToReceiver,
}

#[derive(Clone, Copy, Debug)]
pub enum MovementIdentity {
    /// Each selected transfer's subject identifies its movement. Independent
    /// claims sharing that opaque subject corroborate or disagree about one
    /// movement. This is NOT the meaning of every general observation subject.
    SubjectIdentifiesMovement,
}

/// Optional account-incidence hypothesis for a leg with exactly one known
/// endpoint, and that endpoint equal to the selected account. It neither
/// identifies the counterparty nor resolves legs whose known endpoint differs.
#[derive(Clone, Copy, Debug)]
pub enum CounterpartyHypothesis {
    UnknownCounterpartyIsNotSelectedAccount,
}

#[derive(Clone, Debug)]
pub struct CashSelection {
    pub account: Id,
    pub opening: Id,
    pub closing: Id,
    /// Claim IDs, not event IDs. An empty set explicitly selects no movements.
    pub movements: BTreeSet<Id>,
    pub balance_measure: Id,
    pub transfer_measure: Id,
    pub from: CivilDate,
    pub through: CivilDate,
    pub convention: CashConvention,
    pub movement_identity: MovementIdentity,
    pub counterparty_hypothesis: Option<CounterpartyHypothesis>,
}

#[derive(Debug)]
pub struct BalanceAlternative<C: CurrencyUnit> {
    pub subject: Id,
    pub measure: Id,
    pub date: CivilDate,
    pub amount: Amount<C>,
}

#[derive(Debug)]
pub struct BalanceFinding<C: CurrencyUnit> {
    pub claim: Id,
    pub alternatives: Vec<BalanceAlternative<C>>,
    pub amount: Option<Amount<C>>,
    pub unresolved: Vec<String>,
}

#[derive(Debug)]
pub struct TransferAlternative<C: CurrencyUnit> {
    pub subject: Id,
    pub measure: Id,
    pub date: CivilDate,
    pub sender: Option<Id>,
    pub receiver: Option<Id>,
    pub amount: Amount<C>,
    pub in_window: bool,
    pub counterparty_hypothesis_used: bool,
    pub contribution: Option<Amount<C>>,
    pub explanation: String,
}

#[derive(Debug)]
pub struct TransferFinding<C: CurrencyUnit> {
    pub claim: Id,
    pub subjects: BTreeSet<Id>,
    pub dates: BTreeSet<CivilDate>,
    pub temporally_excluded: bool,
    pub diagnostics: Vec<String>,
    pub alternatives: Vec<TransferAlternative<C>>,
    pub unresolved: Vec<String>,
}

#[derive(Debug)]
pub struct MovementFinding<C: CurrencyUnit> {
    pub subject: Id,
    pub claims: BTreeSet<Id>,
    pub alternatives: BTreeSet<Amount<C>>,
    pub contribution: Option<Amount<C>>,
    pub unresolved: Vec<String>,
}

#[derive(Debug)]
pub struct CashFinding<C: CurrencyUnit> {
    pub selection: CashSelection,
    pub opening: BalanceFinding<C>,
    pub closing: BalanceFinding<C>,
    pub selected_claims: Vec<TransferFinding<C>>,
    pub movements: Vec<MovementFinding<C>>,
    /// Sum of determined movements only; never a fallback for the full net.
    pub known_subtotal: Option<Amount<C>>,
    pub known_movement_count: usize,
    pub net_movement: Option<Amount<C>>,
    pub residual: Option<Amount<C>>,
    pub unresolved: Vec<String>,
}

fn balance<C: CurrencyUnit>(
    facts: &TribleSet,
    claim: Id,
    selection: &CashSelection,
    expected_date: &CivilDate,
) -> BalanceFinding<C> {
    let amount_attribute = schema::amount::<C>();
    let mut unresolved = Vec::new();
    if !exists!(pattern!(facts, [{ claim @ metadata::tag: schema::KIND_STOCK }])) {
        unresolved.push("selected claim has no Stock witness".into());
    }
    // Project every modeled alternative, not only the desired subject/date or
    // measure. A competing alternative cannot disappear behind a filter.
    let alternatives: BTreeSet<_> =
        find!((subject: Id, measure: Id, date: CivilDate, amount: Amount<C>),
        pattern!(facts, [{ claim @ schema::entity: ?subject,
            schema::measure: ?measure, schema::as_of: ?date,
            amount_attribute: ?amount }]))
        .collect();
    if alternatives.is_empty() {
        unresolved.push(format!("no complete typed stock projection: subject, measure, as-of date or {} amount is missing/unsupported", C::CODE));
    }
    let mut amounts = BTreeSet::new();
    let alternatives = alternatives.into_iter().map(|(subject, measure, date, amount)| {
        if subject != selection.account || measure != selection.balance_measure || &date != expected_date {
            unresolved.push(format!("stock alternative subject {subject:X}, measure {measure:X}, as-of {date} does not establish the selected account/measure/date hypothesis"));
        }
        amounts.insert(amount);
        BalanceAlternative { subject, measure, date, amount }
    }).collect();
    if amounts.len() > 1 {
        unresolved.push("selected stock has competing numeric amounts".into());
    }
    // This asks whether the equation has a determined value. It neither
    // declares a field singular nor rejects any source alternative.
    let amount = if unresolved.is_empty() {
        amounts.into_iter().next()
    } else {
        None
    };
    BalanceFinding {
        claim,
        alternatives,
        amount,
        unresolved,
    }
}

/// Compute a finding under explicit cash-account, measure, date/sign, and
/// movement-identity hypotheses. Caller selection is not verified accounting
/// equivalence: quoted scope and basis remain source evidence.
///
/// Only invalid request boundaries return an error. Missing/unsupported facts,
/// disagreement and arithmetic overflow return an uncomputed finding. The
/// absence of unselected movements is never asserted.
pub fn reconcile_cash<C: CurrencyUnit>(
    facts: &TribleSet,
    selection: &CashSelection,
) -> Result<CashFinding<C>> {
    if selection.from >= selection.through {
        bail!("cash window requires from before through; it denotes (from, through]");
    }
    // Both hypotheses have a single supported explicit variant. No default
    // exists, so callers must name the interpretation they are requesting.
    let CashConvention::EndOfDaySignedSenderToReceiver = selection.convention;
    let MovementIdentity::SubjectIdentifiesMovement = selection.movement_identity;
    let opening = balance::<C>(facts, selection.opening, selection, &selection.from);
    let closing = balance::<C>(facts, selection.closing, selection, &selection.through);
    let amount_attribute = schema::amount::<C>();
    let mut selected_claims = Vec::new();
    // This map groups derived numeric consequences of only the selected
    // evidence by the caller's explicitly hypothesized movement identity.
    let mut movements: BTreeMap<Id, MovementFinding<C>> = BTreeMap::new();
    let mut unresolved = Vec::new();
    for &claim in &selection.movements {
        let subjects: BTreeSet<_> = find!(subject: Id,
            pattern!(facts, [{ claim @ schema::entity: ?subject }]))
        .collect();
        let mut issues = Vec::new();
        if subjects.is_empty() {
            issues.push("selected claim has no typed movement subject".into());
        } else if subjects.len() > 1 {
            issues.push(
                "selected claim has distinct subject alternatives; movement identity is unresolved"
                    .into(),
            );
        }
        if !exists!(pattern!(facts, [{ claim @ metadata::tag: schema::KIND_TRANSFER }])) {
            issues.push("selected claim has no Transfer witness; provider events and other kinds are not cash movements by inference".into());
        }
        let measures: BTreeSet<_> = find!(measure: Id,
            pattern!(facts, [{ claim @ schema::measure: ?measure }]))
        .collect();
        if measures.is_empty()
            || measures
                .iter()
                .any(|measure| *measure != selection.transfer_measure)
        {
            issues
                .push("selected transfer has missing or incompatible measure alternatives".into());
        }
        let dates: BTreeSet<_> = find!(date: CivilDate,
            pattern!(facts, [{ claim @ schema::occurred: ?date }]))
        .collect();
        let in_window_dates: BTreeSet<_> = find!(date: CivilDate, and!(
            pattern!(facts, [{ claim @ schema::occurred: ?date }]),
            facts.value_in_range(date, civil_day::after(&selection.from), (&selection.through).to_inline()),
        )).collect();
        if dates.is_empty() {
            issues.push(
                "selected transfer has no typed occurred date; temporal effect is unresolved"
                    .into(),
            );
        }
        let sender_known =
            exists!((sender: Id), pattern!(facts, [{ claim @ schema::sender: ?sender }]));
        let receiver_known =
            exists!((receiver: Id), pattern!(facts, [{ claim @ schema::receiver: ?receiver }]));
        let amount_known =
            exists!((amount: Amount<C>), pattern!(facts, [{ claim @ amount_attribute: ?amount }]));
        let partial_endpoint_hypothesis =
            selection.counterparty_hypothesis.is_some() && sender_known != receiver_known;
        let mut diagnostics = Vec::new();
        if !sender_known {
            diagnostics.push("sender is missing/unsupported".into());
        }
        if !receiver_known {
            diagnostics.push("receiver is missing/unsupported".into());
        }
        if !amount_known {
            diagnostics.push(format!("{} amount is missing/unsupported", C::CODE));
        }
        // Temporal exclusion alone establishes no effect on this window. It
        // does not fill in an absent amount or endpoint, or certify the claim
        // complete. Every retained typed date must be outside; a competing
        // in-window date cannot be erased by this shortcut.
        let temporally_excluded =
            !dates.is_empty() && in_window_dates.is_empty() && issues.is_empty();
        if !temporally_excluded
            && (!amount_known
                || (!(sender_known && receiver_known) && !partial_endpoint_hypothesis))
        {
            issues.push(
                "in-window/undated evidence lacks a typed amount or endpoint; no zero inferred"
                    .into(),
            );
        }
        let mut alternatives = Vec::new();
        // Native date constraints split complete typed projections into inside
        // and outside alternatives. Out-of-window facts remain evidence, and
        // may disagree with another date on the same selected claim.
        for in_window in [true, false] {
            let mut variants: BTreeSet<_> = find!((subject: Id, measure: Id, date: CivilDate,
                sender: Id, receiver: Id, amount: Amount<C>), and!(
                pattern!(facts, [{ claim @ schema::entity: ?subject,
                    schema::measure: ?measure, schema::occurred: ?date,
                    schema::sender: ?sender, schema::receiver: ?receiver,
                    amount_attribute: ?amount }]),
                if in_window {
                    Box::new(facts.value_in_range(date, civil_day::after(&selection.from), (&selection.through).to_inline())) as Box<dyn Constraint<'_> + Send + Sync + '_>
                } else {
                    // Raw extrema are comparison sentinels only. Typed date
                    // projection still skips malformed calendar/padding rows.
                    Box::new(or!(
                        facts.value_in_range(date, Inline::<CivilDay>::new([0;32]), (&selection.from).to_inline()),
                        facts.value_in_range(date, civil_day::after(&selection.through), Inline::<CivilDay>::new([255;32])),
                    )) as Box<dyn Constraint<'_> + Send + Sync + '_>
                },
            )).map(|(subject, measure, date, sender, receiver, amount)|
                (subject, measure, date, Some(sender), Some(receiver), amount)).collect();
            if partial_endpoint_hypothesis {
                // Query the actual known endpoint alternatives. None remains
                // None; no fictional counterparty ID or equality is supplied.
                let endpoint = if sender_known {
                    &*schema::sender
                } else {
                    &*schema::receiver
                };
                variants.extend(find!((subject: Id, measure: Id, date: CivilDate,
                    known: Id, amount: Amount<C>), and!(
                    pattern!(facts, [{ claim @ schema::entity: ?subject,
                        schema::measure: ?measure, schema::occurred: ?date,
                        endpoint: ?known, amount_attribute: ?amount }]),
                    if in_window {
                        Box::new(facts.value_in_range(date, civil_day::after(&selection.from), (&selection.through).to_inline())) as Box<dyn Constraint<'_> + Send + Sync + '_>
                    } else {
                        Box::new(or!(
                            facts.value_in_range(date, Inline::<CivilDay>::new([0;32]), (&selection.from).to_inline()),
                            facts.value_in_range(date, civil_day::after(&selection.through), Inline::<CivilDay>::new([255;32])),
                        )) as Box<dyn Constraint<'_> + Send + Sync + '_>
                    },
                )).map(|(subject, measure, date, known, amount)| {
                    let (sender, receiver) = if sender_known { (Some(known), None) } else { (None, Some(known)) };
                    (subject, measure, date, sender, receiver, amount)
                }));
            }
            for (subject, measure, date, sender, receiver, amount) in variants {
                let (contribution, explanation) = if measure != selection.transfer_measure {
                    (None, "measure not selected".into())
                } else if !in_window {
                    (
                        Some(Amount::ZERO),
                        "known dated movement outside (from, through]; zero effect on this window"
                            .into(),
                    )
                } else if sender.is_some() && sender == receiver {
                    (Some(Amount::ZERO), "self-transfer; zero net effect".into())
                } else if receiver == Some(selection.account) {
                    (
                        Some(amount),
                        if sender.is_none() {
                            "hypothetical signed receiver contribution: unknown sender assumed not to be selected account".into()
                        } else {
                            "signed receiver contribution".into()
                        },
                    )
                } else if sender == Some(selection.account) {
                    match amount.checked_neg() {
                        Ok(value) => (
                            Some(value),
                            if receiver.is_none() {
                                "hypothetical signed sender contribution: unknown receiver assumed not to be selected account".into()
                            } else {
                                "signed sender contribution".into()
                            },
                        ),
                        Err(error) => {
                            issues.push(format!(
                                "exact sender contribution could not be computed: {error:?}"
                            ));
                            (None, "arithmetic overflow; contribution uncomputed".into())
                        }
                    }
                } else if sender.is_some() && receiver.is_some() {
                    (
                        Some(Amount::ZERO),
                        "neither stated endpoint is the selected account; zero net effect".into(),
                    )
                } else {
                    issues.push("known endpoint differs from selected account; missing endpoint could be selected account, so counterpart hypothesis does not resolve this leg".into());
                    (None, "unknown account incidence; no zero inferred".into())
                };
                alternatives.push(TransferAlternative {
                    subject,
                    measure,
                    date,
                    sender,
                    receiver,
                    amount,
                    in_window,
                    counterparty_hypothesis_used: in_window
                        && contribution.is_some()
                        && (sender.is_none() || receiver.is_none()),
                    contribution,
                    explanation,
                });
            }
        }
        if alternatives.is_empty() && !temporally_excluded {
            issues.push(format!("no supported typed transfer projection: subject, measure, occurred date, required endpoint or {} amount is missing/unsupported; no zero inferred", C::CODE));
        }
        if !issues.is_empty() {
            unresolved.push(format!(
                "selected movement claim {claim:X} has unresolved evidence"
            ));
        }
        // A claim with alternative subjects has no determined membership in
        // any movement group. Keep its alternatives in the claim report and
        // leave the full net unresolved, without poisoning known distinct
        // movements or assigning the ambiguous claim to every candidate.
        if subjects.len() == 1 {
            for &subject in &subjects {
                let finding = movements.entry(subject).or_insert_with(|| MovementFinding {
                    subject,
                    claims: BTreeSet::new(),
                    alternatives: BTreeSet::new(),
                    contribution: None,
                    unresolved: Vec::new(),
                });
                finding.claims.insert(claim);
                if temporally_excluded {
                    finding.alternatives.insert(Amount::ZERO);
                }
                finding.unresolved.extend(
                    issues
                        .iter()
                        .map(|issue| format!("claim {claim:X}: {issue}")),
                );
                for variant in &alternatives {
                    if variant.subject == subject {
                        if let Some(value) = variant.contribution {
                            finding.alternatives.insert(value);
                        }
                    }
                }
            }
        }
        selected_claims.push(TransferFinding {
            claim,
            subjects,
            dates,
            temporally_excluded,
            diagnostics,
            alternatives,
            unresolved: issues,
        });
    }
    for movement in movements.values_mut() {
        if movement.alternatives.len() > 1 {
            movement.unresolved.push(
                "selected evidence gives different numeric consequences for this movement".into(),
            );
        }
        if movement.unresolved.is_empty() {
            movement.contribution = movement.alternatives.iter().next().copied();
        }
        if movement.contribution.is_none() {
            unresolved.push(format!(
                "movement {:X} has no determined contribution",
                movement.subject
            ));
        }
    }
    if opening.amount.is_none() {
        unresolved.push("opening balance is unresolved".into());
    }
    if closing.amount.is_none() {
        unresolved.push("closing balance is unresolved".into());
    }
    // Keep the determined subset useful even when another movement or the
    // selected balance pair is unresolved. This is explicitly NOT the net of
    // all selected evidence and is never substituted into its residual.
    let known_movement_count = movements
        .values()
        .filter(|m| m.contribution.is_some())
        .count();
    let mut known_subtotal = Some(Amount::<C>::ZERO);
    for movement in movements.values() {
        if let (Some(total), Some(value)) = (known_subtotal, movement.contribution) {
            match total.checked_add(value) {
                Ok(sum) => known_subtotal = Some(sum),
                Err(error) => {
                    unresolved.push(format!(
                        "exact known-movement subtotal could not be computed: {error:?}"
                    ));
                    known_subtotal = None;
                }
            }
        }
    }
    let movements_determined = selected_claims
        .iter()
        .all(|claim| claim.unresolved.is_empty())
        && movements
            .values()
            .all(|movement| movement.contribution.is_some());
    let net_movement = if movements_determined {
        known_subtotal
    } else {
        None
    };
    let mut residual = None;
    if let (Some(open), Some(close), Some(net)) = (opening.amount, closing.amount, net_movement) {
        match close
            .checked_sub(open)
            .and_then(|difference| difference.checked_sub(net))
        {
            Ok(value) => residual = Some(value),
            Err(error) => unresolved.push(format!(
                "exact residual arithmetic could not be computed: {error:?}"
            )),
        }
    }
    Ok(CashFinding {
        selection: selection.clone(),
        opening,
        closing,
        selected_claims,
        movements: movements.into_values().collect(),
        known_subtotal,
        known_movement_count,
        net_movement,
        residual,
        unresolved,
    })
}

/// Render the calculation and variant-level evidence pointers. The CLI also
/// prints the original selected claims through `finance::show`, preserving
/// source locators, scope/basis wording, and unknown facts alongside this view.
pub fn write_finding<C: CurrencyUnit, W: Write>(
    finding: &CashFinding<C>,
    out: &mut W,
) -> Result<()> {
    let s = &finding.selection;
    writeln!(out, "Cash reconciliation hypothesis: account {:X}; currency {}; balance measure {:X}; transfer measure {:X}.", s.account, C::CODE, s.balance_measure, s.transfer_measure)?;
    writeln!(out, "Both balances are assumed end-of-day; movement window ({}, {}]. Positive amounts follow sender -> receiver; negative amounts reverse it.", s.from, s.through)?;
    writeln!(out, "Selected transfer subjects are assumed to identify movements. Shared subjects count once; distinct subjects count separately. Generic observation subjects do not establish this on their own.")?;
    writeln!(out, "Account and measure comparability are caller hypotheses. Source scope/basis wording is evidence, not verified equivalence. No completeness or missing-movement claim is made.")?;
    if s.counterparty_hypothesis.is_some() {
        writeln!(out, "Additional hypothesis: when exactly one endpoint is known and equals the selected account, its unknown counterparty is assumed not to be that account. The counterparty remains unknown. This does not resolve a known non-account endpoint plus a missing other endpoint.")?;
    }
    for (role, balance) in [("opening", &finding.opening), ("closing", &finding.closing)] {
        writeln!(out, "{role} claim {:X}", balance.claim)?;
        for v in &balance.alternatives {
            writeln!(
                out,
                "  subject {:X}; measure {:X}; as-of {}; amount {}",
                v.subject, v.measure, v.date, v.amount
            )?;
        }
        for issue in &balance.unresolved {
            writeln!(out, "  unresolved: {issue}")?;
        }
    }
    if s.movements.is_empty() {
        writeln!(
            out,
            "Movement evidence: explicitly selected no movement claims."
        )?;
    }
    for claim in &finding.selected_claims {
        writeln!(out, "selected transfer claim {:X}", claim.claim)?;
        for date in &claim.dates {
            writeln!(out, "  stated occurred date: {date}")?;
        }
        if claim.temporally_excluded {
            writeln!(out, "  all retained typed dates are outside the window: contribution 0 by temporal exclusion; missing fields remain unstated")?;
        }
        for diagnostic in &claim.diagnostics {
            writeln!(out, "  evidence diagnostic: {diagnostic}")?;
        }
        for v in &claim.alternatives {
            writeln!(out, "  movement {:X}; measure {:X}; occurred {}; sender {}; receiver {}; amount {}; {}; contribution {}",
                v.subject, v.measure, v.date,
                v.sender.map(|id| format!("{id:X}")).unwrap_or_else(|| "unknown".into()),
                v.receiver.map(|id| format!("{id:X}")).unwrap_or_else(|| "unknown".into()),
                v.amount, v.explanation,
                v.contribution.map(|amount| amount.to_string()).unwrap_or_else(|| "uncomputed".into()))?;
        }
        for issue in &claim.unresolved {
            writeln!(out, "  unresolved: {issue}")?;
        }
    }
    for movement in &finding.movements {
        writeln!(
            out,
            "movement {:X}: {} selected claim(s), contribution {}",
            movement.subject,
            movement.claims.len(),
            movement
                .contribution
                .map(|amount| amount.to_string())
                .unwrap_or_else(|| "uncomputed".into())
        )?;
        for issue in &movement.unresolved {
            writeln!(out, "  unresolved: {issue}")?;
        }
    }
    writeln!(out, "Known subtotal over {} determined movement(s): {}. This does not substitute for uncomputed movements.", finding.known_movement_count,
        finding.known_subtotal.map(|amount| amount.to_string()).unwrap_or_else(|| "uncomputed (arithmetic overflow)".into()))?;
    writeln!(
        out,
        "Net of all selected movement evidence: {}",
        finding
            .net_movement
            .map(|amount| amount.to_string())
            .unwrap_or_else(|| "uncomputed".into())
    )?;
    if let Some(residual) = finding.residual {
        writeln!(
            out,
            "Residual (closing - opening - net selected movements): {residual}"
        )?;
        if residual == Amount::ZERO {
            writeln!(out, "Zero residual: agreement of the selected evidence under these hypotheses; not proof of completeness.")?;
        } else {
            writeln!(out, "Nonzero residual: selected evidence does not balance under these hypotheses; no cause or adjustment is inferred.")?;
        }
    } else {
        writeln!(out, "Residual: uncomputed")?;
        for issue in &finding.unresolved {
            writeln!(out, "  unresolved: {issue}")?;
        }
    }
    Ok(())
}
