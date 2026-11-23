//! Synthetic assertions exercise meaning, open-world reads, and the actual
//! CLI/pile boundary.
use std::collections::BTreeSet;
use std::process::Command;

use faculties_erp::finance::{self, schema, *};
use faculties_erp::revolver::{parse_timestamp, IntervalValue};
use triblespace::core::inline::encodings::money::{Amount, Euro, UsDollar};
use triblespace::core::inline::TryToInline;
use triblespace::core::metadata;
use triblespace::core::repo::SnapshotSource;
use triblespace::prelude::*;

fn date(s: &str) -> CivilDate {
    CivilDate::parse(s).unwrap()
}
fn instant(s: &str) -> IntervalValue {
    parse_timestamp(s).unwrap().unwrap()
}
fn recording() -> Recording {
    Recording {
        actor: fucid().id,
        at: instant("2026-11-01 12:00:00"),
    }
}
fn claim(entity: Id) -> Observation {
    Observation {
        entity,
        measure: None,
        measure_label: "reported revenue".into(),
        claimant: None,
        claimant_label: "Synthetic issuer".into(),
        evidence: Evidence {
            reference: EvidenceRef::Uri("https://example.invalid/report".into()),
            locator: "page 4, table A, revenue row".into(),
            publication_date: Some(date("2026-03-01")),
            observed_at: Some(instant("2026-10-30 08:00:00")),
            date_wording: Some("published March 2026".into()),
        },
        amount: Some("-1/3 EUR".parse().unwrap()),
        amount_wording: Some("negative one third euro".into()),
        scope: Some("single entity, as stated".into()),
        basis: None,
        classification: Some("issuer calls this an investment grant".into()),
        kind: ObservationKind::PeriodTotal {
            period: Period {
                wording: Some("financial year 2024".into()),
                start: None,
                end_exclusive: None,
            },
        },
    }
}
fn rendered(fragment: &mut Fragment, id: Id) -> String {
    let blobs = fragment.blobs_mut().snapshot().unwrap();
    let mut out = Vec::new();
    finance::show(fragment.facts(), &blobs, id, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn exact_negative_amount_and_distinct_source_reporting_recording_times_survive() {
    let input = claim(fucid().id);
    let mut fragment = record_fragment(&input, &recording()).unwrap();
    let id = fragment.root().unwrap();
    let amount = schema::amount::<Euro>();
    let values: Vec<_> =
        find!(v: Amount<Euro>, pattern!(fragment.facts(), [{ id @ amount: ?v }])).collect();
    assert_eq!(values, vec!["-1/3 EUR".parse().unwrap()]);
    let output = rendered(&mut fragment, id);
    for text in [
        "-1/3 EUR",
        "financial year 2024",
        "2026-03-01",
        "source observed at",
        "recorded by",
        "classification (source claim)",
    ] {
        assert!(output.contains(text), "missing {text}: {output}");
    }
    assert!(!output.contains("period start (inclusive):"));
    assert!(observations(
        fragment.facts(),
        &Filter {
            period_start: Some(date("2024-01-01")),
            ..Filter::default()
        }
    )
    .is_empty());
}

#[test]
fn same_amount_and_year_does_not_identify_a_measure_or_a_source_claim() {
    let entity = fucid().id;
    let mut a = claim(entity);
    a.amount = Some("17 EUR".parse().unwrap());
    a.amount_wording = Some("seventeen tokens (rounded as published)".into());
    a.measure_label = "sample token purchases".into();
    let mut b = a.clone();
    b.measure_label = "sample token redemptions".into();
    b.evidence.locator = "example worksheet, cell B2".into();
    let first = record_fragment(&a, &recording()).unwrap();
    let second = record_fragment(&b, &recording()).unwrap();
    assert_ne!(first.root(), second.root());
    let mut union = first + second;
    let ids = observations(
        union.facts(),
        &Filter {
            entity: Some(entity),
            ..Filter::default()
        },
    );
    assert_eq!(ids.len(), 2);
    let output: String = ids.iter().map(|id| rendered(&mut union, *id)).collect();
    assert!(output.contains("sample token purchases"));
    assert!(output.contains("sample token redemptions"));
    assert!(output.contains("rounded as published"));
}

#[test]
fn contradictory_amounts_and_alias_named_entities_survive_union_without_adjudication() {
    let a_entity = fucid();
    let b_entity = fucid();
    let mut first = claim(a_entity.id);
    first.measure_label = "credit-limit increase".into();
    first.kind = ObservationKind::Capacity {
        period: Period::default(),
    };
    let mut changed = first.clone();
    changed.amount = Some("2 EUR".parse().unwrap());
    let mut alias = first.clone();
    alias.entity = b_entity.id;
    alias.measure_label = "credit-limit level".into();
    let mut union = record_fragment(&first, &recording()).unwrap()
        + record_fragment(&changed, &recording()).unwrap()
        + record_fragment(&alias, &recording()).unwrap();
    union += entity! { &a_entity @ metadata::name: "same display label" };
    union += entity! { &b_entity @ metadata::name: "same display label" };
    assert_eq!(observations(union.facts(), &Filter::default()).len(), 3);
    let a_ids = observations(
        union.facts(),
        &Filter {
            entity: Some(a_entity.id),
            kind: Some(schema::KIND_CAPACITY),
            ..Filter::default()
        },
    );
    assert_eq!(a_ids.len(), 2);
    for id in a_ids {
        assert!(rendered(&mut union, id).contains("credit-limit increase"));
    }
    assert_eq!(
        observations(
            union.facts(),
            &Filter {
                entity: Some(b_entity.id),
                ..Filter::default()
            }
        )
        .len(),
        1
    );
}

#[test]
fn unknown_kind_currency_and_extra_facts_are_queryable_under_random_subject_id() {
    let subject = fucid();
    let observation = fucid();
    let unknown_kind = fucid();
    let dollar_attribute = schema::amount::<UsDollar>();
    let dollars = "19.25 USD"
        .parse::<Amount<UsDollar>>()
        .unwrap()
        .try_to_inline()
        .unwrap();
    let mut fragment = entity! { &observation @
        schema::entity: subject.id,
        metadata::tag: unknown_kind.id,
        schema::measure_label: "unknown reader's measure",
        dollar_attribute: dollars,
        schema::amount_wording: "19.25 USD",
        metadata::description: "extension fact the finance reader does not understand",
    };
    let ids = observations(
        fragment.facts(),
        &Filter {
            kind: Some(unknown_kind.id),
            ..Filter::default()
        },
    );
    assert_eq!(ids, BTreeSet::from([observation.id]));
    let output = rendered(&mut fragment, observation.id);
    assert!(output.contains("no decoded EUR amount"));
    assert!(output.contains("unsupported"));
    assert!(output.contains("19.25 USD"));
    assert!(output.contains(&format!("uninterpreted {:X}", dollar_attribute.id())));
}

#[test]
fn random_replacement_of_derived_observation_id_preserves_queries() {
    let mut fragment = record_fragment(&claim(fucid().id), &recording()).unwrap();
    let original = fragment.root().unwrap();
    let replacement = fucid();
    let mut renamed = TribleSet::new();
    for trible in fragment.facts().iter() {
        let entity = if *trible.e() == original {
            replacement.id
        } else {
            *trible.e()
        };
        if *trible.a() == schema::recording_of.id() {
            let value: Inline<inlineencodings::GenId> = replacement.id.to_inline();
            renamed.insert(&Trible::force(&entity, trible.a(), &value));
        } else {
            renamed.insert(&Trible::force(
                &entity,
                trible.a(),
                trible.v::<UnknownInline>(),
            ));
        }
    }
    assert_eq!(
        observations(&renamed, &Filter::default()),
        BTreeSet::from([replacement.id])
    );
    assert!(!observations(&renamed, &Filter::default()).contains(&original));
    let blobs = fragment.blobs_mut().snapshot().unwrap();
    let mut out = Vec::new();
    finance::show(&renamed, &blobs, replacement.id, &mut out).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("reported revenue"));
}

#[test]
fn recording_identity_is_annotation_but_asserted_meaning_is_identity() {
    let input = claim(fucid().id);
    let first = record_fragment(&input, &recording()).unwrap();
    let later = Recording {
        actor: fucid().id,
        at: instant("2026-11-02 12:00:00"),
    };
    let second = record_fragment(&input, &later).unwrap();
    assert_eq!(first.root(), second.root());
    let mut changed = input.clone();
    changed.measure_label = "expenditure".into();
    assert_ne!(
        first.root(),
        record_fragment(&changed, &later).unwrap().root()
    );
}

#[test]
fn all_five_writer_views_keep_endpoints_specific_and_exact_period_filters() {
    let mut union = Fragment::empty();
    let entity = fucid().id;
    let period = Period {
        wording: Some("calendar year 2024".into()),
        start: Some(date("2024-01-01")),
        end_exclusive: Some(date("2025-01-01")),
    };
    let kinds = [
        ObservationKind::Stock {
            as_of: Some(date("2024-12-31")),
        },
        ObservationKind::Transfer {
            occurred: None,
            sender: Some(fucid().id),
            receiver: None,
        },
        ObservationKind::Capacity {
            period: period.clone(),
        },
        ObservationKind::Commitment {
            period: period.clone(),
            due: None,
        },
        ObservationKind::PeriodTotal { period },
    ];
    for kind in kinds {
        let mut input = claim(entity);
        input.kind = kind;
        input.amount = None;
        input.scope = None;
        union += record_fragment(&input, &recording()).unwrap();
    }
    assert_eq!(observations(union.facts(), &Filter::default()).len(), 5);
    assert_eq!(
        observations(
            union.facts(),
            &Filter {
                period_start: Some(date("2024-01-01")),
                period_end: Some(date("2025-01-01")),
                ..Filter::default()
            }
        )
        .len(),
        3
    );
    let senders: Vec<Id> =
        find!(id: Id, pattern!(union.facts(), [{ ?id @ schema::sender: _?sender }])).collect();
    assert_eq!(senders.len(), 1);
    assert!(observations(
        union.facts(),
        &Filter {
            kind: Some(schema::KIND_TRANSFER),
            ..Filter::default()
        }
    )
    .contains(&senders[0]));
}

#[test]
fn named_workspace_route_keeps_imported_payment_evidence_separate_from_claim_subject() {
    use faculties_erp::access::{initialize_signer, open_pile_strict};
    use faculties_erp::revolver::read_snapshot;
    use faculties_erp::revolver::schema::{payment, KIND_PAYMENT};
    use triblespace::core::collection::{policy::private_policy, CollectionStoreExt};
    let temp = tempfile::tempdir().unwrap();
    let pile = temp.path().join("test.pile");
    let key = temp.path().join("test.key");
    std::fs::File::create(&pile).unwrap();
    let signer = initialize_signer(&pile, Some(&key)).unwrap();
    let source = fucid();
    let amount = "23.00 EUR"
        .parse::<Amount<Euro>>()
        .unwrap()
        .try_to_inline()
        .unwrap();
    let imported = entity! { &source @ metadata::tag: KIND_PAYMENT, payment::amount: amount };
    let imported_facts = imported.facts().clone();
    let mut storage = open_pile_strict(&pile).unwrap();
    // Role binding is this exact descriptor, not its human-readable name.
    let handle = storage
        .collection("Exampleville/erp", private_policy(signer.verifying_key()))
        .unwrap()
        .handle();
    let snapshot = storage.snapshot().unwrap();
    let collection =
        faculties_erp::access::open_collection(&snapshot, handle, &signer, true).unwrap();
    storage.commit(collection, &signer, imported).unwrap();
    storage.close().unwrap();
    let subject = fucid().id;
    let mut input = claim(subject);
    input.evidence.reference = EvidenceRef::Entity(source.id);
    let id = record(&pile, Some(&key), handle, &input, &recording()).unwrap();
    let snapshot = read_snapshot(&pile, Some(&key), handle).unwrap();
    for fact in imported_facts.iter() {
        assert!(snapshot.facts.contains(fact));
    }
    assert_eq!(
        observations(
            &snapshot.facts,
            &Filter {
                entity: Some(subject),
                ..Filter::default()
            }
        ),
        BTreeSet::from([id])
    );
    assert!(!exists!(
        pattern!(&snapshot.facts, [{ id @ schema::sender: _?sender }])
    ));
    assert!(!exists!(
        pattern!(&snapshot.facts, [{ id @ schema::receiver: _?receiver }])
    ));
}

#[test]
fn cli_record_list_show_and_reject_silent_wrong_kind_fields() {
    let temp = tempfile::tempdir().unwrap();
    let pile = temp.path().join("cli.pile");
    let key = temp.path().join("cli.key");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_erp"))
            .args([
                "--pile",
                pile.to_str().unwrap(),
                "--key",
                key.to_str().unwrap(),
            ])
            .args(args)
            .env_remove("TRIBLESPACE_COLLECTION_ERP")
            .output()
            .unwrap()
    };
    let initialized = run(&["source", "revolver", "init"]);
    assert!(initialized.status.success());
    let init_text = String::from_utf8(initialized.stdout).unwrap();
    let route = init_text.split_whitespace().last().unwrap();
    let before = std::fs::metadata(&pile).unwrap().len();
    assert!(!run(&["finance", "list"]).status.success());
    assert_eq!(
        before,
        std::fs::metadata(&pile).unwrap().len(),
        "missing route must not create/rebind"
    );
    let actor = format!("{:X}", fucid().id);
    let subject = format!("{:X}", fucid().id);
    let args = [
        "--collection",
        route,
        "finance",
        "record",
        "--kind",
        "stock",
        "--entity",
        &subject,
        "--measure-label",
        "reported book value",
        "--claimant-label",
        "Synthetic issuer",
        "--source-uri",
        "https://example.invalid/report",
        "--locator",
        "page 3",
        "--amount",
        "-0.005 EUR",
        "--actor",
        &actor,
        "--as-of",
        "2024-12-31",
    ];
    let result = run(&args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).unwrap();
    let id = text.split_whitespace().last().unwrap();
    let shown = run(&["--collection", route, "finance", "show", id]);
    assert!(shown.status.success());
    assert!(String::from_utf8(shown.stdout)
        .unwrap()
        .contains("-0.005 EUR"));
    let listed = run(&[
        "--collection",
        route,
        "finance",
        "list",
        "--entity",
        &subject,
        "--kind",
        "stock",
    ]);
    assert!(listed.status.success());
    assert!(String::from_utf8(listed.stdout)
        .unwrap()
        .contains("1 observation(s)"));
    let ranged = run(&[
        "--collection",
        route,
        "finance",
        "list",
        "--as-of-from",
        "2024-12-31",
        "--as-of-through",
        "2024-12-31",
    ]);
    assert!(ranged.status.success());
    assert!(String::from_utf8(ranged.stdout)
        .unwrap()
        .contains("1 observation(s)"));
    assert!(!run(&[
        "--collection",
        route,
        "finance",
        "list",
        "--overlap-start",
        "2024-01-01"
    ])
    .status
    .success());
    let mut wrong = args.to_vec();
    wrong.extend(["--sender", &actor]);
    assert!(!run(&wrong).status.success());
}

#[test]
fn civil_dates_do_not_accept_invalid_days_or_invent_boundaries() {
    assert!(CivilDate::parse("2024-02-29").is_ok());
    for text in [
        "2023-02-29",
        "2024-04-31",
        "0000-01-01",
        "2024-01-00",
        "2024",
        "2024-1-01",
    ] {
        assert!(CivilDate::parse(text).is_err(), "accepted {text}");
    }
}

#[test]
fn provider_observations_preserve_exact_amount_or_unknown_without_decomposition() {
    let source_kind = fucid().id;
    for amount in [Some("-42.19 EUR".parse::<Amount<Euro>>().unwrap()), None] {
        let mut input = claim(fucid().id);
        input.kind = ObservationKind::SourceSpecific {
            kind: source_kind,
            wording: "provider card-charge entry".into(),
            event_date: Some(date("2026-10-07")),
        };
        input.measure_label = "provider reported signed amount".into();
        input.amount = amount;
        input.amount_wording = None;
        let mut fragment = record_fragment(&input, &recording()).unwrap();
        let id = fragment.root().unwrap();
        assert_eq!(
            observations(
                fragment.facts(),
                &Filter {
                    kind: Some(source_kind),
                    ..Filter::default()
                }
            ),
            BTreeSet::from([id])
        );
        for forbidden in [
            schema::KIND_STOCK,
            schema::KIND_TRANSFER,
            schema::KIND_CAPACITY,
            schema::KIND_COMMITMENT,
            schema::KIND_PERIOD_TOTAL,
        ] {
            assert!(observations(
                fragment.facts(),
                &Filter {
                    kind: Some(forbidden),
                    ..Filter::default()
                }
            )
            .is_empty());
        }
        for attribute in [&*schema::sender, &*schema::receiver] {
            assert!(!exists!(
                pattern!(fragment.facts(), [{ id @ attribute: _?endpoint }])
            ));
        }
        for attribute in [&*schema::due, &*schema::period_start, &*schema::period_end] {
            assert!(!exists!(
                pattern!(fragment.facts(), [{ id @ attribute: _?date }])
            ));
        }
        let output = rendered(&mut fragment, id);
        assert!(output.contains("provider card-charge entry"));
        assert!(output.contains("source event date: 2026-10-07"));
        assert_eq!(
            output.contains("amount: -42.19 EUR"),
            input.amount.is_some()
        );
    }
}

#[test]
fn recording_union_retains_actor_time_pairs_without_cross_product() {
    let input = claim(fucid().id);
    let first = recording();
    let second = Recording {
        actor: fucid().id,
        at: instant("2026-11-02 13:00:00"),
    };
    let a = record_fragment(&input, &first).unwrap();
    let b = record_fragment(&input, &second).unwrap();
    let id = a.root().unwrap();
    assert_eq!(Some(id), b.root());
    let union = a + b;
    let receipts: Vec<_> = find!((actor: Id, at: IntervalValue), pattern!(union.facts(), [
        { _?receipt @ schema::recording_of: id, schema::recorded_by: ?actor, metadata::created_at: ?at }
    ])).collect();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.contains(&(first.actor, first.at)));
    assert!(receipts.contains(&(second.actor, second.at)));
    assert!(!receipts.contains(&(first.actor, second.at)));
    assert!(!receipts.contains(&(second.actor, first.at)));
}

#[test]
fn sortable_civil_days_preserve_chronology_and_reject_noncanonical_values() {
    let dates = [
        "0001-01-01",
        "0999-12-31",
        "1000-01-01",
        "1900-02-28",
        "1900-03-01",
        "2000-02-29",
        "2024-02-29",
        "2024-03-01",
        "9999-12-31",
    ];
    let encoded: Vec<Inline<CivilDay>> = dates.iter().map(|s| date(s).to_inline()).collect();
    for pair in encoded.windows(2) {
        assert!(pair[0].raw < pair[1].raw);
    }
    for (text, value) in dates.iter().zip(&encoded) {
        assert_eq!(CivilDate::try_from_inline(value).unwrap().as_str(), *text);
        assert!(CivilDay::validate(*value).is_ok());
    }
    let leap: Inline<CivilDay> = date("2024-02-29").to_inline();
    assert_eq!(&leap.raw[..4], &[0x07, 0xe8, 2, 29]);
    assert_eq!(&leap.raw[4..], &[0; 28]);
    for (year, month, day) in [
        (0u16, 1, 1),
        (10000, 1, 1),
        (2024, 13, 1),
        (2024, 1, 0),
        (1900, 2, 29),
        (2023, 2, 29),
        (2024, 4, 31),
    ] {
        let mut raw = [0u8; 32];
        raw[..2].copy_from_slice(&year.to_be_bytes());
        raw[2] = month;
        raw[3] = day;
        let invalid = Inline::<CivilDay>::new(raw);
        assert!(CivilDay::validate(invalid).is_err());
        assert!(CivilDate::try_from_inline(&invalid).is_err());
    }
    let mut noncanonical = leap;
    noncanonical.raw[31] = 1;
    assert!(CivilDay::validate(noncanonical).is_err());
    assert!(CivilDate::try_from_inline(&noncanonical).is_err());
}

#[test]
fn native_as_of_window_is_inclusive_and_skips_malformed_dates() {
    let subject = fucid().id;
    let lower = fucid();
    let upper = fucid();
    let outside = fucid();
    let missing = fucid();
    let malformed = fucid();
    let alternatives = fucid();
    let mut facts = Fragment::empty();
    for (entity, day) in [
        (&lower, "2024-02-28"),
        (&upper, "2024-02-29"),
        (&outside, "2024-03-01"),
    ] {
        facts += entity! { entity @ schema::entity: subject, schema::as_of: date(day) };
    }
    facts += entity! { &missing @ schema::entity: subject };
    facts += entity! { &alternatives @ schema::entity: subject,
    schema::as_of*: [date("2024-02-29"), date("2024-03-01")] };
    let mut bad: Inline<CivilDay> = date("2024-02-28").to_inline();
    bad.raw[20] = 1;
    facts += entity! { &malformed @ schema::entity: subject, schema::as_of: bad };
    assert_eq!(observations(facts.facts(), &Filter::default()).len(), 6);
    let found = observations(
        facts.facts(),
        &Filter {
            as_of: Some(AsOfWindow::new(date("2024-02-28"), date("2024-02-29")).unwrap()),
            ..Filter::default()
        },
    );
    assert_eq!(found, BTreeSet::from([lower.id, upper.id, alternatives.id]));
    let point = observations(
        facts.facts(),
        &Filter {
            as_of: Some(AsOfWindow::new(date("2024-02-29"), date("2024-02-29")).unwrap()),
            ..Filter::default()
        },
    );
    assert_eq!(point, BTreeSet::from([upper.id, alternatives.id]));
    assert!(AsOfWindow::new(date("2024-03-01"), date("2024-02-29")).is_err());
}

#[test]
fn native_period_overlap_excludes_touching_and_keeps_stated_alternatives() {
    let subject = fucid().id;
    let left = fucid();
    let right = fucid();
    let leap = fucid();
    let enclosing = fucid();
    let missing = fucid();
    let alternatives = fucid();
    let malformed = fucid();
    let mut facts = Fragment::empty();
    for (entity, start, end) in [
        (&left, "2024-02-01", "2024-02-28"),
        (&right, "2024-03-01", "2024-04-01"),
        (&leap, "2024-02-29", "2024-03-01"),
        (&enclosing, "2024-01-01", "2025-01-01"),
    ] {
        facts += entity! { entity @ schema::entity: subject,
        schema::period_start: date(start), schema::period_end: date(end) };
    }
    facts +=
        entity! { &missing @ schema::entity: subject, schema::period_start: date("2024-02-29") };
    facts += entity! { &alternatives @ schema::entity: subject,
    schema::period_start*: [date("2024-02-29"), date("2024-03-01")],
    schema::period_end: date("2024-04-01"),
    schema::as_of: date("2024-02-29") };
    let mut bad: Inline<CivilDay> = date("2024-02-29").to_inline();
    bad.raw[3] = 30;
    facts += entity! { &malformed @ schema::entity: subject,
    schema::period_start: bad, schema::period_end: date("2024-03-01") };
    assert_eq!(observations(facts.facts(), &Filter::default()).len(), 7);
    let overlap = PeriodOverlap::new(date("2024-02-28"), date("2024-03-01")).unwrap();
    let found = observations(
        facts.facts(),
        &Filter {
            overlap: Some(overlap.clone()),
            ..Filter::default()
        },
    );
    assert_eq!(
        found,
        BTreeSet::from([leap.id, enclosing.id, alternatives.id])
    );
    let combined = observations(
        facts.facts(),
        &Filter {
            overlap: Some(overlap),
            as_of: Some(AsOfWindow::new(date("2024-02-29"), date("2024-02-29")).unwrap()),
            ..Filter::default()
        },
    );
    assert_eq!(combined, BTreeSet::from([alternatives.id]));
    assert!(PeriodOverlap::new(date("2024-02-29"), date("2024-02-29")).is_err());
}
