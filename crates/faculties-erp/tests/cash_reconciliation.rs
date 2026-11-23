//! Synthetic cash reconciliation tests use opaque IDs and exact rational amounts.
use std::collections::BTreeSet;
use std::process::Command;

use faculties_erp::finance::{reconcile::*, schema, *};
use faculties_erp::revolver::parse_timestamp;
use triblespace::core::inline::encodings::money::{Amount, Euro, UsDollar};
use triblespace::core::inline::TryToInline;
use triblespace::core::metadata;
use triblespace::prelude::*;

fn date(text: &str) -> CivilDate {
    CivilDate::parse(text).unwrap()
}
fn euro(text: &str) -> Amount<Euro> {
    text.parse().unwrap()
}
fn recorder() -> Recording {
    Recording {
        actor: fucid().id,
        at: parse_timestamp("2026-11-01 12:00:00").unwrap().unwrap(),
    }
}
fn observation(
    subject: Id,
    measure: Id,
    amount: Option<&str>,
    kind: ObservationKind,
) -> Observation {
    Observation {
        entity: subject,
        measure: Some(measure),
        measure_label: "source quantity wording".into(),
        claimant: None,
        claimant_label: "Synthetic source".into(),
        evidence: Evidence {
            reference: EvidenceRef::Uri("https://example.invalid/synthetic-cash".into()),
            locator: format!("independent claim {:X}", fucid().id),
            publication_date: None,
            observed_at: None,
            date_wording: None,
        },
        amount: amount.map(euro),
        amount_wording: amount.map(str::to_owned),
        scope: Some("source scope; not verified equivalence".into()),
        basis: None,
        classification: None,
        kind,
    }
}
struct Fixture {
    data: Fragment,
    selection: CashSelection,
    peer: Id,
}
impl Fixture {
    fn new(open: &str, close: &str) -> Self {
        let account = fucid().id;
        let balance_measure = fucid().id;
        let transfer_measure = fucid().id;
        let opening = record_fragment(
            &observation(
                account,
                balance_measure,
                Some(open),
                ObservationKind::Stock {
                    as_of: Some(date("2024-02-28")),
                },
            ),
            &recorder(),
        )
        .unwrap();
        let closing = record_fragment(
            &observation(
                account,
                balance_measure,
                Some(close),
                ObservationKind::Stock {
                    as_of: Some(date("2024-03-01")),
                },
            ),
            &recorder(),
        )
        .unwrap();
        let selection = CashSelection {
            account,
            opening: opening.root().unwrap(),
            closing: closing.root().unwrap(),
            movements: BTreeSet::new(),
            balance_measure,
            transfer_measure,
            from: date("2024-02-28"),
            through: date("2024-03-01"),
            convention: CashConvention::EndOfDaySignedSenderToReceiver,
            movement_identity: MovementIdentity::SubjectIdentifiesMovement,
            counterparty_hypothesis: None,
        };
        Self {
            data: opening + closing,
            selection,
            peer: fucid().id,
        }
    }
    fn movement(
        &self,
        event: Id,
        sender: Option<Id>,
        receiver: Option<Id>,
        amount: Option<&str>,
        occurred: Option<&str>,
    ) -> Observation {
        observation(
            event,
            self.selection.transfer_measure,
            amount,
            ObservationKind::Transfer {
                sender,
                receiver,
                occurred: occurred.map(date),
            },
        )
    }
    fn select(&mut self, input: Observation) -> Id {
        let fragment = record_fragment(&input, &recorder()).unwrap();
        let id = fragment.root().unwrap();
        self.data += fragment;
        self.selection.movements.insert(id);
        id
    }
    fn result(&self) -> CashFinding<Euro> {
        reconcile_cash(self.data.facts(), &self.selection).unwrap()
    }
}

#[test]
fn selected_cash_equation_reports_agreement_or_residual_without_explaining_a_cause() {
    let mut f = Fixture::new("1000 EUR", "1350 EUR");
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("500 EUR"),
        Some("2024-02-29"),
    ));
    f.select(f.movement(
        fucid().id,
        Some(f.selection.account),
        Some(f.peer),
        Some("150 EUR"),
        Some("2024-03-01"),
    ));
    assert_eq!(f.result().residual, Some(Amount::ZERO));
    let mut wrong = Fixture::new("1000 EUR", "1350 EUR");
    wrong.select(wrong.movement(
        fucid().id,
        Some(wrong.selection.account),
        Some(wrong.peer),
        Some("350 EUR"),
        Some("2024-02-29"),
    ));
    let finding = wrong.result();
    assert_eq!(finding.residual, Some(euro("700 EUR")));
    let mut output = Vec::new();
    write_finding(&finding, &mut output).unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("no cause or adjustment is inferred"));
}

#[test]
fn explicit_empty_selection_is_not_a_completeness_claim() {
    let mut f = Fixture::new("1000 EUR", "1000 EUR");
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("250 EUR"),
        Some("2024-02-29"),
    ));
    f.select(f.movement(
        fucid().id,
        Some(f.selection.account),
        Some(f.peer),
        Some("250 EUR"),
        Some("2024-02-29"),
    ));
    f.selection.movements.clear(); // deliberately unselected construction-only events
    let finding = f.result();
    assert_eq!(finding.residual, Some(Amount::ZERO));
    assert!(finding.movements.is_empty());
    let mut output = Vec::new();
    write_finding(&finding, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("explicitly selected no movement claims"));
    assert!(output.contains("not proof of completeness"));
}

#[test]
fn signed_reversals_self_transfers_and_noncent_rationals_keep_exact_consequences() {
    let mut f = Fixture::new("0 EUR", "1/3 EUR");
    f.select(f.movement(
        fucid().id,
        Some(f.selection.account),
        Some(f.peer),
        Some("-2/3 EUR"),
        Some("2024-02-29"),
    ));
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("-1/3 EUR"),
        Some("2024-02-29"),
    ));
    f.select(f.movement(
        fucid().id,
        Some(f.selection.account),
        Some(f.selection.account),
        Some("-1000 EUR"),
        Some("2024-03-01"),
    ));
    let finding = f.result();
    assert_eq!(finding.net_movement, Some(euro("1/3 EUR")));
    assert_eq!(finding.residual, Some(Amount::ZERO));
    assert!(finding
        .selected_claims
        .iter()
        .flat_map(|claim| &claim.alternatives)
        .any(|v| v.explanation.contains("self-transfer")));
}

#[test]
fn end_of_day_cutoff_excludes_opening_day_and_includes_closing_day_and_leap_day() {
    let mut f = Fixture::new("0 EUR", "3 EUR");
    for (day, amount) in [
        ("2024-02-27", "100 EUR"),
        ("2024-02-28", "200 EUR"),
        ("2024-02-29", "1 EUR"),
        ("2024-03-01", "2 EUR"),
        ("2024-03-02", "400 EUR"),
    ] {
        f.select(f.movement(
            fucid().id,
            Some(f.peer),
            Some(f.selection.account),
            Some(amount),
            Some(day),
        ));
    }
    let finding = f.result();
    assert_eq!(finding.residual, Some(Amount::ZERO));
    assert_eq!(
        finding
            .selected_claims
            .iter()
            .filter(|c| c.temporally_excluded)
            .count(),
        3
    );
}

#[test]
fn outside_only_dates_determine_zero_without_imputing_missing_amount_or_endpoints() {
    let mut f = Fixture::new("1 EUR", "1 EUR");
    let claim = f.select(f.movement(fucid().id, None, None, None, Some("2024-02-28")));
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::occurred: date("2024-03-02") };
    let finding = f.result();
    assert_eq!(finding.residual, Some(Amount::ZERO));
    assert!(finding.selected_claims[0].temporally_excluded);
    assert_eq!(finding.selected_claims[0].diagnostics.len(), 3);
    assert!(finding.selected_claims[0].alternatives.is_empty());
    // Add a retained in-window date: neither missing financial data nor the
    // inconvenient date may disappear behind the outside-window evidence.
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::occurred: date("2024-02-29") };
    assert!(f.result().residual.is_none());
}

#[test]
fn conflicting_cutoff_dates_are_unresolved_but_same_zero_consequences_are_not() {
    let mut f = Fixture::new("0 EUR", "5 EUR");
    let claim = f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("5 EUR"),
        Some("2024-02-29"),
    ));
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::occurred: date("2024-03-02") };
    let finding = f.result();
    assert!(finding.residual.is_none());
    assert_eq!(
        finding.movements[0].alternatives,
        BTreeSet::from([Amount::ZERO, euro("5 EUR")])
    );
    assert_eq!(finding.selected_claims[0].alternatives.len(), 2);
    let mut zero = Fixture::new("0 EUR", "0 EUR");
    let claim = zero.select(zero.movement(
        fucid().id,
        Some(zero.selection.account),
        Some(zero.selection.account),
        Some("5 EUR"),
        Some("2024-02-29"),
    ));
    zero.data += entity! { ExclusiveId::force_ref(&claim) @ schema::occurred: date("2024-03-02") };
    assert_eq!(zero.result().residual, Some(Amount::ZERO));
}

#[test]
fn corroborating_claims_count_one_event_while_distinct_equal_events_count_twice() {
    let mut f = Fixture::new("0 EUR", "20 EUR");
    let event = fucid().id;
    let first = f.movement(
        event,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    );
    f.select(first.clone());
    let mut corroborating = first;
    corroborating.evidence.locator = "independent corroborating statement".into();
    corroborating.claimant_label = "Second issuer".into();
    f.select(corroborating);
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    ));
    let finding = f.result();
    assert_eq!(finding.selected_claims.len(), 3);
    assert_eq!(finding.movements.len(), 2);
    assert_eq!(finding.residual, Some(Amount::ZERO));
    assert_eq!(
        finding
            .movements
            .iter()
            .find(|m| m.subject == event)
            .unwrap()
            .claims
            .len(),
        2
    );
}

#[test]
fn conflicting_event_claims_and_competing_balances_remain_evidence_not_collection_errors() {
    let mut f = Fixture::new("0 EUR", "10 EUR");
    let event = fucid().id;
    f.select(f.movement(
        event,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    ));
    f.select(f.movement(
        event,
        Some(f.peer),
        Some(f.selection.account),
        Some("11 EUR"),
        Some("2024-02-29"),
    ));
    assert!(f.result().residual.is_none());
    assert_eq!(f.result().movements[0].alternatives.len(), 2);
    f.selection.movements.clear();
    let amount = schema::amount::<Euro>();
    let closing = f.selection.closing;
    f.data += entity! { ExclusiveId::force_ref(&closing) @ amount: euro("12 EUR").try_to_inline().unwrap() };
    let finding = f.result();
    assert!(finding.residual.is_none());
    assert_eq!(finding.closing.alternatives.len(), 2);
}

#[test]
fn ambiguous_subject_identity_is_unresolved_even_if_numeric_consequences_agree() {
    let mut f = Fixture::new("0 EUR", "10 EUR");
    let claim = f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    ));
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::entity: fucid().id };
    let finding = f.result();
    assert_eq!(finding.selected_claims[0].subjects.len(), 2);
    assert!(finding.residual.is_none());
    assert!(finding.selected_claims[0]
        .unresolved
        .iter()
        .any(|reason| reason.contains("identity is unresolved")));
}

#[test]
fn missing_dates_endpoints_amounts_and_source_specific_events_are_not_silently_zero() {
    for missing in 0..5 {
        let mut f = Fixture::new("0 EUR", "0 EUR");
        let mut input = f.movement(
            fucid().id,
            Some(f.peer),
            Some(f.selection.account),
            Some("10 EUR"),
            Some("2024-02-29"),
        );
        match missing {
            0 => {
                input.kind = ObservationKind::Transfer {
                    occurred: None,
                    sender: Some(f.peer),
                    receiver: Some(f.selection.account),
                }
            }
            1 => {
                input.kind = ObservationKind::Transfer {
                    occurred: Some(date("2024-02-29")),
                    sender: None,
                    receiver: Some(f.selection.account),
                }
            }
            2 => input.amount = None,
            3 => {
                input.kind = ObservationKind::SourceSpecific {
                    kind: fucid().id,
                    wording: "provider card charge".into(),
                    event_date: Some(date("2024-02-29")),
                }
            }
            4 => input.measure = None,
            _ => unreachable!(),
        }
        f.select(input);
        assert!(f.result().residual.is_none(), "missing case {missing}");
    }
}

#[test]
fn extra_unknown_facts_are_harmless_and_random_claim_identity_still_queries() {
    let mut f = Fixture::new("0 EUR", "10 EUR");
    let claim = f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    ));
    let replacement = fucid();
    // Copy the assertion facts to an unrelated opaque identity. The record's
    // intrinsic hash is never recomputed as a query condition.
    let mut copied = TribleSet::new();
    for (attribute, value) in find!((attribute: Id, value: Inline<UnknownInline>),
        pattern!(f.data.facts(), [{ claim @ ?attribute: ?value }]))
    {
        copied.insert(&Trible::new(&replacement, &attribute, &value));
    }
    let extra = entity! { &replacement @ metadata::tag: fucid().id, metadata::name: "same label as unrelated account" };
    let mut facts = f.data.facts().clone();
    facts.union(copied);
    facts.union(extra.facts().clone());
    f.selection.movements = BTreeSet::from([replacement.id]);
    let finding = reconcile_cash::<Euro>(&facts, &f.selection).unwrap();
    assert_eq!(finding.residual, Some(Amount::ZERO));
}

#[test]
fn cash_like_labels_do_not_replace_account_measure_or_stock_kind_witnesses() {
    let mut f = Fixture::new("0 EUR", "0 EUR");
    let other = observation(
        fucid().id,
        f.selection.balance_measure,
        Some("0 EUR"),
        ObservationKind::Stock {
            as_of: Some(date("2024-02-28")),
        },
    );
    let fragment = record_fragment(&other, &recorder()).unwrap();
    f.selection.opening = fragment.root().unwrap();
    f.data += fragment;
    assert!(f.result().residual.is_none());
    let mut capacity = observation(
        f.selection.account,
        f.selection.balance_measure,
        Some("1000 EUR"),
        ObservationKind::Capacity {
            period: Period::default(),
        },
    );
    capacity.measure_label = "cash available".into();
    let fragment = record_fragment(&capacity, &recorder()).unwrap();
    f.selection.opening = fragment.root().unwrap();
    f.data += fragment;
    assert!(f.result().residual.is_none());
}

#[test]
fn unsupported_currency_does_not_become_euros_and_generic_dollar_query_is_exact() {
    let mut f = Fixture::new("0 EUR", "1 EUR");
    let input = f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        None,
        Some("2024-02-29"),
    );
    let claim = f.select(input);
    let dollar = schema::amount::<UsDollar>();
    let one: Amount<UsDollar> = "1 USD".parse().unwrap();
    f.data += entity! { ExclusiveId::force_ref(&claim) @ dollar: one.try_to_inline().unwrap() };
    assert!(f.result().residual.is_none());
    let opening = f.selection.opening;
    let closing = f.selection.closing;
    f.data += entity! { ExclusiveId::force_ref(&opening) @ dollar: Amount::<UsDollar>::ZERO.try_to_inline().unwrap() };
    f.data += entity! { ExclusiveId::force_ref(&closing) @ dollar: one.try_to_inline().unwrap() };
    let dollars = reconcile_cash::<UsDollar>(f.data.facts(), &f.selection).unwrap();
    assert_eq!(dollars.residual, Some(Amount::ZERO));
    assert!(f.result().residual.is_none());
}

#[test]
fn arithmetic_overflow_is_an_uncomputed_finding() {
    let mut f = Fixture::new("0 EUR", "0 EUR");
    let maximum = format!("{} EUR", i128::MAX);
    for _ in 0..2 {
        f.select(f.movement(
            fucid().id,
            Some(f.peer),
            Some(f.selection.account),
            Some(&maximum),
            Some("2024-02-29"),
        ));
    }
    let finding = f.result();
    assert!(finding.residual.is_none());
    assert!(finding
        .unresolved
        .iter()
        .any(|reason| reason.contains("Overflow")));
    let f = Fixture::new(&format!("{} EUR", -i128::MAX), &maximum);
    let finding = f.result();
    assert!(finding.residual.is_none());
    assert!(finding
        .unresolved
        .iter()
        .any(|reason| reason.contains("Overflow")));
}

#[test]
fn endpoint_alternatives_with_equal_effect_compute_but_opposing_effects_remain_unresolved() {
    let mut f = Fixture::new("0 EUR", "10 EUR");
    let claim = f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    ));
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::sender: fucid().id };
    let finding = f.result();
    assert_eq!(finding.selected_claims[0].alternatives.len(), 2);
    assert_eq!(finding.residual, Some(Amount::ZERO));
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::sender: f.selection.account };
    assert!(f.result().residual.is_none());
}

#[test]
fn invalid_inline_dates_skip_typed_projection_without_panicking_or_making_a_zero() {
    let mut f = Fixture::new("0 EUR", "0 EUR");
    let claim = f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        None,
    ));
    let mut invalid: Inline<CivilDay> = date("2024-02-29").to_inline();
    invalid.raw[3] = 30;
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::occurred: invalid };
    let finding = f.result();
    assert!(finding.residual.is_none());
    assert!(finding.selected_claims[0].dates.is_empty());
    // Valid later evidence remains queryable, despite the uninterpretable row.
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::occurred: date("2024-03-02") };
    assert_eq!(f.result().residual, Some(Amount::ZERO));
}

#[test]
fn cli_requires_identity_and_date_sign_hypotheses_and_intentional_movement_selection() {
    use faculties_erp::access::{
        initialize_collection, initialize_signer, open_collection, open_pile_strict,
    };
    use triblespace::core::collection::CollectionStoreExt;
    use triblespace::core::repo::SnapshotSource;
    let mut f = Fixture::new("1000 EUR", "1000 EUR");
    let partial_claim = f.select(f.movement(
        fucid().id,
        None,
        Some(f.selection.account),
        Some("100 EUR"),
        Some("2024-02-29"),
    ));
    let temp = tempfile::tempdir().unwrap();
    let pile = temp.path().join("cash.pile");
    let key = temp.path().join("cash.key");
    std::fs::File::create(&pile).unwrap();
    let signer = initialize_signer(&pile, Some(&key)).unwrap();
    let handle = initialize_collection(&pile, Some(&key)).unwrap();
    let mut storage = open_pile_strict(&pile).unwrap();
    let snapshot = storage.snapshot().unwrap();
    let collection = open_collection(&snapshot, handle, &signer, true).unwrap();
    storage.commit(collection, &signer, f.data).unwrap();
    storage.close().unwrap();
    let route: String = handle
        .raw
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let run = |args: &[String]| {
        Command::new(env!("CARGO_BIN_EXE_erp"))
            .args([
                "--pile",
                pile.to_str().unwrap(),
                "--key",
                key.to_str().unwrap(),
                "--collection",
                &route,
                "finance",
                "reconcile-cash",
            ])
            .args(args)
            .env_remove("TRIBLESPACE_COLLECTION_ERP")
            .output()
            .unwrap()
    };
    let mut args = vec![
        "--account".into(),
        format!("{:X}", f.selection.account),
        "--opening".into(),
        format!("{:X}", f.selection.opening),
        "--closing".into(),
        format!("{:X}", f.selection.closing),
        "--balance-measure".into(),
        format!("{:X}", f.selection.balance_measure),
        "--transfer-measure".into(),
        format!("{:X}", f.selection.transfer_measure),
        "--from".into(),
        "2024-02-28".into(),
        "--through".into(),
        "2024-03-01".into(),
    ];
    let bytes_before = std::fs::read(&pile).unwrap();
    assert!(!run(&args).status.success());
    args.extend([
        "--convention".into(),
        "end-of-day-signed-sender-to-receiver".into(),
    ]);
    assert!(!run(&args).status.success());
    args.extend([
        "--movement-identity".into(),
        "subject-identifies-movement".into(),
    ]);
    assert!(!run(&args).status.success());
    args.push("--no-movements".into());
    let output = run(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Residual (closing - opening - net selected movements): 0.00 EUR"), "{text}");
    assert!(text.contains("not proof of completeness"));
    assert!(text.contains("Selected transfer subjects are assumed to identify movements"));
    assert!(text.contains("source scope; not verified equivalence"));
    args.extend(["--movement".into(), format!("{:X}", fucid().id)]);
    assert!(
        !run(&args).status.success(),
        "explicit empty selection must conflict with selected claims"
    );
    args.retain(|arg| arg != "--no-movements");
    let output = run(&args);
    assert!(
        output.status.success(),
        "unresolved evidence is a finding, not command failure"
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Residual: uncomputed"));
    *args.last_mut().unwrap() = format!("{partial_claim:X}");
    let without = run(&args);
    assert!(without.status.success());
    assert!(String::from_utf8(without.stdout)
        .unwrap()
        .contains("Residual: uncomputed"));
    args.extend([
        "--counterparty-hypothesis".into(),
        "unknown-counterparty-is-not-selected-account".into(),
    ]);
    let with = run(&args);
    assert!(
        with.status.success(),
        "{}",
        String::from_utf8_lossy(&with.stderr)
    );
    let text = String::from_utf8(with.stdout).unwrap();
    assert!(text.contains("Residual (closing - opening - net selected movements): -100.00 EUR"), "{text}");
    assert!(text.contains("sender unknown"));
    assert!(text.contains("hypothetical signed receiver contribution"));
    assert_eq!(
        std::fs::read(&pile).unwrap(),
        bytes_before,
        "calculation must not mutate the pile"
    );
}

#[test]
fn determined_subtotal_remains_useful_when_other_evidence_or_balances_are_unresolved() {
    let mut f = Fixture::new("1000 EUR", "1150 EUR");
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("50 EUR"),
        Some("2024-02-29"),
    ));
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("40 EUR"),
        None,
    ));
    let finding = f.result();
    assert_eq!(finding.known_subtotal, Some(euro("50 EUR")));
    assert_eq!(finding.known_movement_count, 1);
    assert!(finding.net_movement.is_none());
    assert!(finding.residual.is_none());
    f.selection.movements = finding
        .selected_claims
        .iter()
        .filter(|claim| claim.unresolved.is_empty())
        .map(|claim| claim.claim)
        .collect();
    let amount = schema::amount::<Euro>();
    let closing = f.selection.closing;
    f.data += entity! { ExclusiveId::force_ref(&closing) @ amount: euro("1200 EUR").try_to_inline().unwrap() };
    let finding = f.result();
    assert_eq!(finding.net_movement, Some(euro("50 EUR")));
    assert!(
        finding.residual.is_none(),
        "known movement net does not resolve competing balances"
    );
}

#[test]
fn alternative_closing_claims_can_be_selected_without_averaging_or_privileging_zero() {
    let mut f = Fixture::new("1000 EUR", "1100 EUR");
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("100 EUR"),
        Some("2024-02-29"),
    ));
    let alternative = record_fragment(
        &observation(
            f.selection.account,
            f.selection.balance_measure,
            Some("1150 EUR"),
            ObservationKind::Stock {
                as_of: Some(f.selection.through.clone()),
            },
        ),
        &recorder(),
    )
    .unwrap();
    let alternative_id = alternative.root().unwrap();
    f.data += alternative;
    assert_eq!(f.result().residual, Some(Amount::ZERO));
    f.selection.closing = alternative_id;
    assert_eq!(f.result().residual, Some(euro("50 EUR")));
}

#[test]
fn aggregates_capacity_and_commitments_are_not_added_to_selected_endpoint_details() {
    let mut f = Fixture::new("1000 EUR", "1200 EUR");
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("300 EUR"),
        Some("2024-02-29"),
    ));
    f.select(f.movement(
        fucid().id,
        Some(f.selection.account),
        Some(f.peer),
        Some("100 EUR"),
        Some("2024-02-29"),
    ));
    let selected_transfers = f.selection.movements.clone();
    let total = observation(
        f.selection.account,
        f.selection.transfer_measure,
        Some("300 EUR"),
        ObservationKind::PeriodTotal {
            period: Period::default(),
        },
    );
    let total_id = f.select(total);
    let capacity = observation(
        fucid().id,
        f.selection.transfer_measure,
        Some("5000 EUR"),
        ObservationKind::Capacity {
            period: Period::default(),
        },
    );
    f.select(capacity);
    let commitment = observation(
        fucid().id,
        f.selection.transfer_measure,
        Some("70 EUR"),
        ObservationKind::Commitment {
            period: Period::default(),
            due: None,
        },
    );
    f.select(commitment);
    let finding = f.result();
    assert_eq!(finding.known_subtotal, Some(euro("200 EUR")));
    assert!(
        finding.residual.is_none(),
        "explicitly selected non-transfers require a diagnostic, not silent exclusion"
    );
    assert!(finding
        .selected_claims
        .iter()
        .find(|claim| claim.claim == total_id)
        .unwrap()
        .unresolved
        .iter()
        .any(|issue| issue.contains("no Transfer witness")));
    f.selection.movements = selected_transfers;
    assert_eq!(
        f.result().residual,
        Some(Amount::ZERO),
        "unselected non-transfer facts do not alter the calculation"
    );
}

#[test]
fn ambiguous_event_membership_does_not_destroy_the_known_distinct_event_subtotal() {
    let mut f = Fixture::new("0 EUR", "20 EUR");
    let first_event = fucid().id;
    f.select(f.movement(
        first_event,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    ));
    f.select(f.movement(
        fucid().id,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    ));
    let ambiguous = f.select(f.movement(
        first_event,
        Some(f.peer),
        Some(f.selection.account),
        Some("10 EUR"),
        Some("2024-02-29"),
    ));
    f.data += entity! { ExclusiveId::force_ref(&ambiguous) @ schema::entity: fucid().id };
    let finding = f.result();
    assert_eq!(finding.known_subtotal, Some(euro("20 EUR")));
    assert_eq!(finding.known_movement_count, 2);
    assert!(finding.net_movement.is_none());
    assert!(finding.residual.is_none());
}

#[test]
fn opted_in_unknown_counterparty_keeps_unknown_identity_and_signed_contribution() {
    for (known_receiver, signed, expected) in [
        (true, "5 EUR", "5 EUR"),
        (true, "-5 EUR", "-5 EUR"),
        (false, "5 EUR", "-5 EUR"),
        (false, "-5 EUR", "5 EUR"),
    ] {
        let mut f = Fixture::new("0 EUR", expected);
        let (sender, receiver) = if known_receiver {
            (None, Some(f.selection.account))
        } else {
            (Some(f.selection.account), None)
        };
        f.select(f.movement(
            fucid().id,
            sender,
            receiver,
            Some(signed),
            Some("2024-02-29"),
        ));
        assert!(
            f.result().residual.is_none(),
            "no opt-in means self-transfer remains possible"
        );
        f.selection.counterparty_hypothesis =
            Some(CounterpartyHypothesis::UnknownCounterpartyIsNotSelectedAccount);
        let finding = f.result();
        assert_eq!(finding.residual, Some(Amount::ZERO));
        let alternative = &finding.selected_claims[0].alternatives[0];
        assert_eq!(
            (alternative.sender, alternative.receiver),
            (sender, receiver)
        );
        assert!(alternative.counterparty_hypothesis_used);
        assert_eq!(alternative.contribution, Some(euro(expected)));
        assert!(
            !finding.selected_claims[0].diagnostics.is_empty(),
            "missing endpoint remains diagnostic"
        );
        let mut output = Vec::new();
        write_finding(&finding, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("hypothetical signed"));
        assert!(text.contains(if known_receiver {
            "sender unknown"
        } else {
            "receiver unknown"
        }));
    }
}

#[test]
fn counterpart_opt_in_does_not_resolve_both_missing_or_a_known_nonaccount_endpoint() {
    for direction in 0..3 {
        let mut f = Fixture::new("0 EUR", "0 EUR");
        f.selection.counterparty_hypothesis =
            Some(CounterpartyHypothesis::UnknownCounterpartyIsNotSelectedAccount);
        let (sender, receiver) = match direction {
            0 => (None, None),
            1 => (Some(f.peer), None),
            _ => (None, Some(f.peer)),
        };
        f.select(f.movement(
            fucid().id,
            sender,
            receiver,
            Some("5 EUR"),
            Some("2024-02-29"),
        ));
        assert!(f.result().residual.is_none());
        assert!(f.result().net_movement.is_none());
    }
}

#[test]
fn counterpart_opt_in_preserves_known_alternatives_and_leaves_complete_self_transfers_unchanged() {
    let mut f = Fixture::new("0 EUR", "-5 EUR");
    f.selection.counterparty_hypothesis =
        Some(CounterpartyHypothesis::UnknownCounterpartyIsNotSelectedAccount);
    let claim = f.select(f.movement(
        fucid().id,
        Some(f.selection.account),
        None,
        Some("5 EUR"),
        Some("2024-02-29"),
    ));
    assert_eq!(f.result().residual, Some(Amount::ZERO));
    f.data += entity! { ExclusiveId::force_ref(&claim) @ schema::sender: f.peer };
    let finding = f.result();
    assert!(finding.residual.is_none());
    assert_eq!(finding.selected_claims[0].alternatives.len(), 2);
    assert!(finding.selected_claims[0]
        .alternatives
        .iter()
        .any(|v| v.sender == Some(f.peer) && v.contribution.is_none()));
    let mut self_transfer = Fixture::new("0 EUR", "0 EUR");
    self_transfer.selection.counterparty_hypothesis = f.selection.counterparty_hypothesis;
    self_transfer.select(self_transfer.movement(
        fucid().id,
        Some(self_transfer.selection.account),
        Some(self_transfer.selection.account),
        Some("5 EUR"),
        Some("2024-02-29"),
    ));
    let finding = self_transfer.result();
    assert_eq!(finding.residual, Some(Amount::ZERO));
    assert!(!finding.selected_claims[0].alternatives[0].counterparty_hypothesis_used);
}
