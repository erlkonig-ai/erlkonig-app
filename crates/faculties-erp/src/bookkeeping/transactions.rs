//! Amex provider-record identity is a convention, not proof of economic identity.
//! Account + reference identifies the record; observed payloads can disagree.
//! Export bytes, filenames and row ordering only identify provenance.

use super::*;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountMapping {
    pub cardholder: String,
    pub card_number: String,
    pub account: String,
}

#[derive(Debug, Serialize)]
pub struct SourceReview {
    pub id: String,
    pub contents: Vec<String>,
    pub rows: Vec<u64>,
    pub files: Vec<String>,
    pub cardholders: Vec<String>,
    pub card_numbers: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ObservationReview {
    pub id: String,
    pub dates: Vec<String>,
    pub amounts: Vec<String>,
    pub descriptions: Vec<String>,
    pub details: Vec<String>,
    pub sources: Vec<SourceReview>,
}

/// Caller-selected correlation, not a Cartesian inference from legacy facts.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Migration {
    pub legacy: String,
    pub content: String,
    pub row: u64,
    pub account: String,
}

#[derive(Debug, Serialize)]
pub struct MigrationPlan {
    pub legacy: String,
    pub transaction: String,
    pub evidence: String,
    pub original_claims: Vec<String>,
    pub projections: Vec<String>,
    pub applied: bool,
}

fn id(value: &str) -> Result<Id> {
    Id::from_hex(value).context("expected full nonzero 32-hex entity id")
}

impl Bookkeeping {
    /// Single-account mode is only for 11-column exports. 13-column exports
    /// require an exact holder/Konto pair mapping; account labels are not keys.
    pub fn import_amex_mapped(
        &self,
        bytes: Bytes,
        name: &str,
        single_account: Option<Id>,
        mappings: &[AccountMapping],
        currency: &str,
        month: Month,
        actor: Id,
    ) -> Result<Imported> {
        ensure!(
            single_account.is_some() != !mappings.is_empty(),
            "supply either a single account (11 columns) or card mappings (13 columns)"
        );
        let rows = amex::parse(&bytes, currency, month)?;
        let mut selected = Vec::new();
        for row in rows {
            ensure!(!row.provider_reference.trim().is_empty(),
                "row {} has no provider reference; no transaction identity was guessed and nothing was published; retain the original and resolve its identity explicitly", row.index);
            let account_id = match (&row.cardholder, &row.card_number) {
                (None, None) => {
                    single_account.context("11-column export requires a single explicit account")?
                }
                (Some(holder), Some(number)) => {
                    ensure!(
                        single_account.is_none(),
                        "13-column export requires explicit holder/Konto mappings"
                    );
                    let selected: BTreeSet<Id> = mappings
                        .iter()
                        .filter(|m| &m.cardholder == holder && &m.card_number == number)
                        .map(|m| id(&m.account))
                        .collect::<Result<_>>()?;
                    // Input routing is a caller instruction, not a cardinality
                    // constraint imposed on stored open-world facts.
                    ensure!(selected.len() == 1, "row {} needs one unambiguous mapping for its exact Karteninhaber and Konto # pair", row.index);
                    *selected.iter().next().unwrap()
                }
                _ => unreachable!("parser supplies card context as a pair"),
            };
            selected.push((row, account_id));
        }
        let mut original = faculties::files::stage(bytes, name, "text/csv")?;
        let file = original.root().expect("Files fragment root");
        let content = find!(
            content: Content,
            pattern!(original.facts(), [{ file @ faculties::schemas::files::file::content: ?content }])
        )
        .next()
        .context("Files content")?;
        original += entity! { ExclusiveId::force_ref(&file) @ board::by: actor };
        let mut change = Fragment::empty();
        let mut activities = Vec::new();
        for (row, account_id) in &selected {
            let (fragment, transaction_id, _) =
                stage_row(row, *account_id, content, &[file], actor)?;
            change += fragment;
            activities.push(hex(transaction_id));
        }
        self.with_view(true, |store, signer, collection, facts, snapshot| {
            for (_, account_id) in &selected {
                ensure!(
                    exists!(pattern!(facts, [{ account_id @ metadata::tag: KIND_ACCOUNT }])),
                    "unknown account; select an existing opaque account"
                );
            }
            let files_handle = self
                .storage
                .collection_routes()
                .and_then(|r| r.get(&faculties::schemas::files::DEFAULT_SCOPE_ID))
                .copied()
                .context("Files collection role is unconfigured")?;
            let files = open_workspace_write_in(snapshot, signer.verifying_key(), files_handle)?;
            store.commit(files, signer, original)?;
            if !selected.is_empty() {
                store.commit(collection, signer, change)?;
            }
            Ok(Imported {
                file: hex(file),
                rows: activities.len(),
                activities,
            })
        })
    }

    /// Read-only by default. Apply publishes the whole selected batch as one
    /// ERP commit; no Files rewrite and no mutation of any original decision.
    pub fn migrate_amex(
        &self,
        selections: &[Migration],
        month: Month,
        actor: Id,
        apply: bool,
    ) -> Result<Vec<MigrationPlan>> {
        self.with_view(apply, |store, signer, collection, facts, reader| {
            let mut change = Fragment::empty();
            let mut plans = Vec::new();
            for selection in selections {
                let old = id(&selection.legacy)?;
                let account_id = id(&selection.account)?;
                let content = original_content(&selection.content)?;
                ensure!(exists!(pattern!(facts, [{ old @ metadata::tag: KIND_ACTIVITY,
                    account: account_id, source_bytes: content, row_index: selection.row }])),
                    "selected legacy activity does not state the supplied account/content/row evidence");
                let original: Bytes = BlobStoreGet::get(reader, content)
                    .context("selected legacy original unavailable; migration wrote nothing")?;
                let row = amex::parse(&original, "EUR", month)?.into_iter()
                    .find(|r| r.index == selection.row).context("selected original row is outside the month or absent")?;
                ensure!(!row.provider_reference.trim().is_empty(), "selected original has no provider reference; migration wrote nothing");
                // Legacy Files annotations remain on the legacy entity. They
                // are not correlated with the selected content by that schema,
                // so do not copy them onto this selected source occurrence.
                let (fragment, target, source) = stage_row(&row, account_id, content, &[], actor)?;
                change += fragment;
                let mut association = entity! { legacy_activity: old, transaction: target, migration_evidence: source };
                let association_id = association.root().unwrap();
                association += entity! { ExclusiveId::force_ref(&association_id) @ board::by: actor };
                change += association;
                let mut originals = Vec::new();
                let mut projections = Vec::new();
                // The original claim is the correlation unit. Do not copy a
                // value/author product onto a newly invented judgment.
                for claim in find!(claim: Id, pattern!(facts, [{ ?claim @ metadata::tag: KIND_PURPOSE, purpose_of: old }])) {
                    let mut projection = entity! { metadata::tag: KIND_PURPOSE, purpose_of: target,
                        original_claim: claim };
                    let projected = projection.root().unwrap();
                    projection += entity! { ExclusiveId::force_ref(&projected) @ board::by: actor, migration_evidence: association_id };
                    change += projection;
                    originals.push(hex(claim)); projections.push(hex(projected));
                }
                for claim in find!(claim: Id, pattern!(facts, [{ ?claim @ metadata::tag: KIND_MATCH, activity: old }])) {
                    let mut projection = entity! { metadata::tag: KIND_MATCH, activity: target,
                        original_claim: claim };
                    let projected = projection.root().unwrap();
                    projection += entity! { ExclusiveId::force_ref(&projected) @ board::by: actor, migration_evidence: association_id };
                    change += projection;
                    originals.push(hex(claim)); projections.push(hex(projected));
                }
                plans.push(MigrationPlan { legacy: hex(old), transaction: hex(target), evidence: hex(association_id),
                    original_claims: originals, projections, applied: apply });
            }
            if apply && !selections.is_empty() { store.commit(collection, signer, change)?; }
            Ok(plans)
        })
    }
}

/// One parsed source row is staged for import or explicit migration. This is
/// not a lookup: consumers always discover opaque ids through the relations.
fn stage_row(
    row: &amex::Row,
    account_id: Id,
    content: Content,
    files: &[Id],
    actor: Id,
) -> Result<(Fragment, Id, Id)> {
    let mut change = entity! { metadata::tag: KIND_AMEX_TRANSACTION, account: account_id,
    provider_reference: row.provider_reference.as_str() };
    let target = change.root().unwrap();
    change += entity! { ExclusiveId::force_ref(&target) @ board::by: actor };
    let amount: Inline<Currency<Euro>> = row.amount.try_to_inline()?;
    let mut observation = entity! { metadata::tag: KIND_AMEX_OBSERVATION, transaction: target,
    payment::date: date(&row.date)?, payment::amount: amount,
    payment::memo: row.description.as_str(), metadata::description: row.details.as_str() };
    let observation_id = observation.root().unwrap();
    observation += entity! { ExclusiveId::force_ref(&observation_id) @ board::by: actor };
    change += observation;
    let mut source = entity! { account: account_id, source_bytes: content, row_index: row.index };
    let source_id = source.root().unwrap();
    source += entity! { ExclusiveId::force_ref(&source_id) @ source_file*: files.iter(), observed: observation_id,
    cardholder?: row.cardholder.as_deref(), card_number?: row.card_number.as_deref(), board::by: actor };
    change += source;
    Ok((change, target, source_id))
}

pub(super) fn review(
    facts: &TribleSet,
    reader: &FacultySnapshot,
    min: Date,
    max: Date,
) -> Result<Vec<ActivityReview>> {
    let mut result = Vec::new();
    let order = ObservationOrder::new(facts, metadata::supersedes.id());
    let subjects: BTreeSet<Id> = find!(subject: Id, or!(
        pattern!(facts, [{ ?subject @ metadata::tag: KIND_AMEX_TRANSACTION }]),
        pattern!(facts, [{ ?subject @ metadata::tag: KIND_ACTIVITY }]),
    ))
    .collect();
    for subject in subjects {
        let legacy = exists!(pattern!(facts, [{ subject @ metadata::tag: KIND_ACTIVITY }]));
        if legacy && exists!(pattern!(facts, [{ legacy_activity: subject, transaction: _?target }]))
        {
            continue;
        }
        let observations: BTreeSet<Id> = if legacy {
            BTreeSet::from([subject])
        } else {
            find!(observation: Id, pattern!(facts, [{ ?observation @ metadata::tag: KIND_AMEX_OBSERVATION, transaction: subject }])).collect()
        };
        if !observations.iter().any(|o| {
            exists!((day: Date), and!(
                pattern!(facts, [{ o @ payment::date: ?day }]), value_range(day, min, max),
            ))
        }) {
            continue;
        }
        let accounts: BTreeSet<Id> =
            find!(a: Id, pattern!(facts, [{ subject @ account: ?a }])).collect();
        let mut labels = BTreeSet::new();
        for a in &accounts {
            labels.extend(text_values(facts, reader, *a, &metadata::name)?);
        }
        let mut reviews = Vec::new();
        let mut files = BTreeSet::new();
        let mut descriptions: BTreeSet<String> =
            text_values(facts, reader, subject, &payment::memo)?
                .into_iter()
                .collect();
        let mut details: BTreeSet<String> =
            text_values(facts, reader, subject, &metadata::description)?
                .into_iter()
                .collect();
        let mut candidates = BTreeSet::new();
        for o in observations {
            // Include all alternatives once any observation is in the selected
            // month, including a conflicting correction into a different month.
            let dates = find!(day: Date, pattern!(facts, [{ o @ payment::date: ?day }]))
                .map(date_text)
                .collect::<Result<Vec<_>>>()?;
            let amounts: Vec<Euros> =
                find!(amount: Euros, pattern!(facts, [{ o @ payment::amount: ?amount }])).collect();
            for amount in &amounts {
                let value: Inline<Currency<Euro>> = amount.try_to_inline()?;
                candidates.extend(find!((i: Id, claim: Id), and!(pattern!(facts, [
                    { ?i @ metadata::tag: KIND_INVOICE },
                    { ?claim @ metadata::tag: KIND_INVOICE_METADATA, invoice: ?i, purchase::gross_total: value },
                ]), maximal(claim, &order))).map(|(i, _)| hex(i)));
            }
            let source_ids: BTreeSet<Id> = if legacy {
                BTreeSet::from([o])
            } else {
                find!(s: Id, pattern!(facts, [{ ?s @ observed: o }])).collect()
            };
            let mut sources = Vec::new();
            for s in source_ids {
                let source_files = source_files(facts, s);
                files.extend(source_files.iter().cloned());
                sources.push(SourceReview {
                    id: hex(s),
                    files: source_files,
                    contents: find!(c: Content, pattern!(facts, [{ s @ source_bytes: ?c }]))
                        .map(content_hex)
                        .collect(),
                    rows: find!(r: u64, pattern!(facts, [{ s @ row_index: ?r }])).collect(),
                    cardholders: text_values(facts, reader, s, &cardholder)?,
                    card_numbers: text_values(facts, reader, s, &card_number)?,
                });
            }
            let ds = text_values(facts, reader, o, &payment::memo)?;
            let dt = text_values(facts, reader, o, &metadata::description)?;
            descriptions.extend(ds.iter().cloned());
            details.extend(dt.iter().cloned());
            reviews.push(ObservationReview {
                id: hex(o),
                dates,
                amounts: amounts.into_iter().map(|a| a.to_string()).collect(),
                descriptions: ds,
                details: dt,
                sources,
            });
        }
        let mut purposes = purpose_claims(facts, reader, subject)?;
        let mut matches = match_claims(facts, reader, subject)?;
        purposes.extend(projected_claims(facts, reader, subject, true)?);
        matches.extend(projected_claims(facts, reader, subject, false)?);
        let legacy_ids: BTreeSet<Id> =
            find!(old: Id, pattern!(facts, [{ legacy_activity: ?old, transaction: subject }]))
                .collect();
        let mut legacy_sources = Vec::new();
        for old in legacy_ids {
            legacy_sources.push(SourceReview {
                id: hex(old),
                contents: find!(c: Content, pattern!(facts, [{ old @ source_bytes: ?c }]))
                    .map(content_hex)
                    .collect(),
                rows: find!(r: u64, pattern!(facts, [{ old @ row_index: ?r }])).collect(),
                files: source_files(facts, old),
                cardholders: text_values(facts, reader, old, &cardholder)?,
                card_numbers: text_values(facts, reader, old, &card_number)?,
            });
        }
        result.push(ActivityReview {
            id: hex(subject),
            accounts: accounts.into_iter().map(hex).collect(),
            account_labels: labels.into_iter().collect(),
            legacy,
            observations: reviews,
            legacy_sources,
            source_files: files.into_iter().collect(),
            descriptions: descriptions.into_iter().collect(),
            details: details.into_iter().collect(),
            provider_references: text_values(facts, reader, subject, &provider_reference)?,
            purposes,
            matches,
            candidates: candidates.into_iter().collect(),
        });
    }
    Ok(result)
}

fn content_hex(content: Content) -> String {
    inlineencodings::Hash::<inlineencodings::Blake3>::to_hex(&inlineencodings::Handle::to_hash(
        content,
    ))
}

fn projected_claims(
    facts: &TribleSet,
    reader: &FacultySnapshot,
    subject: Id,
    is_purpose: bool,
) -> Result<Vec<Claim>> {
    let order = ObservationOrder::new(facts, metadata::supersedes.id());
    let target = if is_purpose { &purpose_of } else { &activity };
    let kind = if is_purpose { KIND_PURPOSE } else { KIND_MATCH };
    let value_attr = if is_purpose { &purpose } else { &invoice };
    let mut claims = Vec::new();
    for (projection, original) in find!((p: Id, o: Id), and!(pattern!(facts, [
        { ?p @ metadata::tag: kind, target: subject, original_claim: ?o },
        { ?o @ metadata::tag: kind },
    ]), maximal(p, &order), maximal(o, &order)))
    {
        claims.push(Claim {
            id: hex(projection),
            values: find!(v: Id, pattern!(facts, [{ original @ value_attr: ?v }]))
                .map(|value| {
                    if is_purpose && value == COMPANY {
                        "company".into()
                    } else if is_purpose && value == PRIVATE {
                        "private".into()
                    } else {
                        hex(value)
                    }
                })
                .collect(),
            actors: find!(a: Id, pattern!(facts, [{ original @ board::by: ?a }]))
                .map(hex)
                .collect(),
            notes: text_values(facts, reader, original, &metadata::description)?,
            original_claim: Some(hex(original)),
            migration_actors: find!(a: Id, pattern!(facts, [{ projection @ board::by: ?a }]))
                .map(hex)
                .collect(),
        });
    }
    // Late claims authored by older readers remain visible immediately through
    // the explicit association. Once projected, the projection's own frontier
    // governs visibility; never resurrect a superseded projection via fallback.
    let late: BTreeSet<Id> = find!(original: Id, temp!((old), and!(pattern!(facts, [
        { legacy_activity: ?old, transaction: subject },
        { ?original @ metadata::tag: kind, target: ?old },
    ]), maximal(original, &order))))
    .collect();
    for original in late {
        if exists!(
            pattern!(facts, [{ metadata::tag: kind, target: subject, original_claim: original }])
        ) {
            continue;
        }
        let migration_actors: BTreeSet<String> = find!(a: Id, pattern!(facts, [
            { original @ target: _?old },
            { legacy_activity: _?old, transaction: subject, board::by: ?a },
        ]))
        .map(hex)
        .collect();
        claims.push(Claim {
            id: hex(original),
            original_claim: Some(hex(original)),
            values: find!(v: Id, pattern!(facts, [{ original @ value_attr: ?v }]))
                .map(|v| {
                    if is_purpose && v == COMPANY {
                        "company".into()
                    } else if is_purpose && v == PRIVATE {
                        "private".into()
                    } else {
                        hex(v)
                    }
                })
                .collect(),
            actors: find!(a: Id, pattern!(facts, [{ original @ board::by: ?a }]))
                .map(hex)
                .collect(),
            notes: text_values(facts, reader, original, &metadata::description)?,
            migration_actors: migration_actors.into_iter().collect(),
        });
    }
    Ok(claims)
}

pub(super) fn matched_transactions(facts: &TribleSet, invoice_id: Id) -> BTreeSet<String> {
    let order = ObservationOrder::new(facts, metadata::supersedes.id());
    let mut result: BTreeSet<String> =
        find!((target: Id, projection: Id, original: Id), and!(pattern!(facts, [
        { ?projection @ metadata::tag: KIND_MATCH, activity: ?target, original_claim: ?original },
        { ?original @ metadata::tag: KIND_MATCH, invoice: invoice_id },
    ]), maximal(projection, &order), maximal(original, &order)))
        .map(|(target, _, _)| hex(target))
        .collect();
    for (target, original) in find!((target: Id, original: Id), temp!((old), and!(pattern!(facts, [
        { legacy_activity: ?old, transaction: ?target },
        { ?original @ metadata::tag: KIND_MATCH, activity: ?old, invoice: invoice_id },
    ]), maximal(original, &order))))
    {
        if !exists!(
            pattern!(facts, [{ metadata::tag: KIND_MATCH, activity: target, original_claim: original }])
        ) {
            result.insert(hex(target));
        }
    }
    result
}
