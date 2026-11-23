//! Bookkeeping operations over explicit ERP and Files workspace routes.
//! Row occurrences, economic matches, and expense purpose are different facts.
//! No collection-wide Rust catalog or closed-world validation is maintained.

pub mod cli;
pub mod mcp;
pub mod schema;
mod transactions;
pub use transactions::{AccountMapping, Migration, MigrationPlan, ObservationReview, SourceReview};

use anybytes::{Bytes, View};
use anyhow::{ensure, Context, Result};
use ed25519_dalek::SigningKey;
use faculties::collection_names::{open_workspace_read_in, open_workspace_write_in};
use faculties::schemas::compass::board;
use faculties::storage::{FacultySnapshot, FacultyStore, Storage};
use hifitime::{Duration, Epoch};
use serde::Serialize;
use triblespace::core::collection::{Collection, CollectionHandle, CollectionStoreExt};
use triblespace::core::inline::encodings::money::{Amount, Currency, Euro, UsDollar};
use triblespace::core::inline::TryToInline;
use triblespace::core::metadata;
use triblespace::core::query::rangeconstraint::value_range;
use triblespace::core::query::register::{maximal, ObservationOrder};
use triblespace::core::repo::{BlobStoreGet, SnapshotSource};
use triblespace::prelude::*;

use crate::amex::{self, Month};
use crate::revolver::schema::{payment, purchase, Euros};
use schema::*;

/// Stable ERP role used by the workspace router.
pub const DEFAULT_SCOPE_ID: Id = id_hex!("192E78206293D3AAFE47D2FF53A9D647");
pub const COLLECTION_NAME: &str = "bookkeeping";
type Text = Inline<inlineencodings::Handle<blobencodings::UTF8String>>;
type Content = Inline<inlineencodings::Handle<blobencodings::RawBytes>>;
type Date = Inline<inlineencodings::NsTAIInterval>;

/// Caller-selected original evidence, not an assertion of tax-invoice status.
/// Existing Files references select content explicitly; no first-value lookup.
pub enum InvoiceOriginal<'a> {
    Pdf {
        bytes: Bytes,
        name: &'a str,
    },
    File {
        file: Id,
        content: faculties::files::ContentHandle,
        media_type: &'a str,
    },
}

pub(super) fn original_content(value: &str) -> Result<Content> {
    let value = value.strip_prefix("blake3:").unwrap_or(value);
    ensure!(
        value.len() == 64,
        "original content requires a full 64-hex RawBytes handle"
    );
    let hash = inlineencodings::Hash::<inlineencodings::Blake3>::from_hex(value)
        .context("original content requires a full 64-hex RawBytes handle")?;
    Ok(inlineencodings::Handle::from_hash(hash))
}

fn check_original_format(bytes: &[u8], media_type: &str) -> Result<()> {
    match media_type {
        "application/pdf" => ensure!(
            bytes.starts_with(b"%PDF-"),
            "invoice original must be PDF bytes"
        ),
        "text/html" => {
            let text = std::str::from_utf8(bytes).context("HTML original must be UTF-8")?;
            ensure!(!text.contains('\0'), "HTML original contains NUL bytes");
            let mut start = text.trim_start_matches('\u{feff}').trim_start();
            while let Some(comment) = start.strip_prefix("<!--") {
                let (_, rest) = comment
                    .split_once("-->")
                    .context("unterminated leading HTML comment")?;
                start = rest.trim_start();
            }
            // A format guard only, not a renderer, extractor, or assertion that
            // the document is a supplier invoice rather than a receipt/login.
            ensure!(
                ["<!doctype html", "<html"].iter().any(|prefix| {
                    start
                        .get(..prefix.len())
                        .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
                        && start
                            .as_bytes()
                            .get(prefix.len())
                            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>')
                }),
                "HTML original must begin with an HTML doctype or html element"
            );
        }
        _ => anyhow::bail!("original media type must be application/pdf or text/html"),
    }
    Ok(())
}

/// A clone shares the caller's store owner. Construction never performs I/O.
#[derive(Clone, Debug)]
pub struct Bookkeeping {
    storage: Storage,
}

#[derive(Debug, Serialize)]
pub struct Imported {
    pub file: String,
    pub activities: Vec<String>,
    pub rows: usize,
}
#[derive(Debug, Serialize)]
pub struct InvoiceAdded {
    pub file: String,
    pub invoice: String,
    pub metadata_claim: String,
}
#[derive(Debug, Serialize)]
pub struct Claim {
    pub id: String,
    pub values: Vec<String>,
    pub actors: Vec<String>,
    pub notes: Vec<String>,
    /// A migration projects the original decision; actor above remains its author.
    pub original_claim: Option<String>,
    pub migration_actors: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct ActivityReview {
    pub id: String,
    pub accounts: Vec<String>,
    pub account_labels: Vec<String>,
    /// True only for historical source-row entities not explicitly migrated.
    pub legacy: bool,
    /// Alternatives are not ordered, selected, or summed. Fields on one
    /// observation are sets, never expanded into synthetic correlated pairs.
    pub observations: Vec<ObservationReview>,
    /// Historical provenance remains explicitly separate from the selected
    /// migration source; its repeated coordinates are not paired by the reader.
    pub legacy_sources: Vec<SourceReview>,
    pub source_files: Vec<String>,
    pub descriptions: Vec<String>,
    /// Verbatim Weitere Details, including any unparsed foreign amount/rate.
    pub details: Vec<String>,
    pub provider_references: Vec<String>,
    pub purposes: Vec<Claim>,
    pub matches: Vec<Claim>,
    /// Equal currency and signed amount only, not a match or expense inference.
    pub candidates: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct InvoiceReview {
    pub id: String,
    pub metadata_claim: String,
    /// Values of one correlated immutable assertion. External multivalued
    /// annotations are disclosed as sets, never expanded into synthetic pairs.
    pub dates: Vec<String>,
    pub amounts: Vec<String>,
    pub actors: Vec<String>,
    pub numbers: Vec<String>,
    pub suppliers: Vec<String>,
    pub source_files: Vec<String>,
    pub purposes: Vec<Claim>,
    pub matched_activities: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct AccountReview {
    pub id: String,
    pub labels: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Review {
    pub accounts: Vec<AccountReview>,
    pub activities: Vec<ActivityReview>,
    pub invoices: Vec<InvoiceReview>,
}

fn hex(id: Id) -> String {
    format!("{id:X}")
}

fn date(iso: &str) -> Result<Date> {
    let epoch = Epoch::from_gregorian_utc(
        iso[..4].parse()?,
        iso[5..7].parse()?,
        iso[8..].parse()?,
        0,
        0,
        0,
        0,
    );
    (epoch, epoch)
        .try_to_inline()
        .map_err(|e| anyhow::anyhow!("encode calendar date: {e:?}"))
}
fn date_text(value: Date) -> Result<String> {
    let (epoch, _): (Epoch, Epoch) = value
        .try_from_inline()
        .map_err(|e| anyhow::anyhow!("decode calendar date: {e:?}"))?;
    Ok(hifitime::efmt::Formatter::new(epoch, hifitime::efmt::consts::ISO8601_DATE).to_string())
}
fn text_values(
    facts: &TribleSet,
    reader: &FacultySnapshot,
    subject: Id,
    attr: &Attribute<inlineencodings::Handle<blobencodings::UTF8String>>,
) -> Result<Vec<String>> {
    let mut values = Vec::new();
    for handle in find!(value: Text, pattern!(facts, [{ subject @ attr: ?value }])) {
        match BlobStoreGet::get::<View<str>, _>(reader, handle) {
            Ok(value) => values.push(value.to_string()),
            // Optional text evidence may be unavailable or undecodable. This
            // edge is disclosed without invalidating unrelated domain rows.
            Err(_) => values.push(format!("[unavailable text attachment {handle:?}]")),
        }
    }
    values.sort();
    values.dedup();
    Ok(values)
}
fn source_files(facts: &TribleSet, subject: Id) -> Vec<String> {
    find!(file: Id, pattern!(facts, [{ subject @ source_file: ?file }]))
        .map(hex)
        .collect()
}

impl Bookkeeping {
    pub fn with_storage(storage: Storage) -> Self {
        Self { storage }
    }

    fn handle(&self) -> Result<CollectionHandle> {
        self.storage
            .collection_routes()
            .and_then(|r| r.get(&DEFAULT_SCOPE_ID))
            .copied()
            .context("ERP collection role is unconfigured; explicit routing is required")
    }

    /// One storage boundary, not a second data model. The snapshot stays frozen
    /// while domain functions query its native facts where they need an answer.
    fn with_view<T>(
        &self,
        write: bool,
        f: impl FnOnce(
            &mut FacultyStore,
            &SigningKey,
            Collection<blobencodings::SimpleArchive>,
            &TribleSet,
            &FacultySnapshot,
        ) -> Result<T>,
    ) -> Result<T> {
        let handle = self.handle()?; // Fail missing routes before key/pile access.
        self.storage.with_store(|store, signer, _| {
            let snapshot = store.snapshot()?;
            let collection = open_workspace_read_in(&snapshot, signer.verifying_key(), handle)?;
            if write {
                open_workspace_write_in(&snapshot, signer.verifying_key(), handle)?;
            }
            let facts = collection.read(&snapshot)?;
            f(store, signer, collection, &facts, &snapshot)
        })
    }

    /// New account identity is opaque and independent of its display label.
    pub fn add_account(&self, label: &str, actor: Id) -> Result<Id> {
        ensure!(!label.trim().is_empty(), "account label must not be empty");
        let id = genid();
        let change =
            entity! { &id @ metadata::tag: KIND_ACCOUNT, metadata::name: label, board::by: actor };
        self.with_view(true, |store, signer, collection, _, _| {
            store.commit(collection, signer, change)?;
            Ok(id.id)
        })
    }

    pub fn import_amex(
        &self,
        bytes: Bytes,
        name: &str,
        account_id: Id,
        currency: &str,
        month: Month,
        actor: Id,
    ) -> Result<Imported> {
        self.import_amex_mapped(bytes, name, Some(account_id), &[], currency, month, actor)
    }

    /// Caller supplies observed document metadata; the original is evidence, not a
    /// source of inferred business purpose, tax treatment or legal conclusions.
    pub fn add_invoice(
        &self,
        bytes: Bytes,
        name: &str,
        invoice_date: &str,
        number: &str,
        supplier: &str,
        total: &str,
        currency: &str,
        actor: Id,
        supersedes: &[Id],
    ) -> Result<InvoiceAdded> {
        self.add_invoice_original(
            InvoiceOriginal::Pdf { bytes, name },
            invoice_date,
            number,
            supplier,
            total,
            currency,
            actor,
            supersedes,
        )
    }

    /// Register explicit metadata for one selected original. The Files variant
    /// requires only read access to its existing routed Files collection.
    pub fn add_invoice_original(
        &self,
        original: InvoiceOriginal<'_>,
        invoice_date: &str,
        number: &str,
        supplier: &str,
        total: &str,
        currency: &str,
        actor: Id,
        supersedes: &[Id],
    ) -> Result<InvoiceAdded> {
        ensure!(
            matches!(currency, "EUR" | "USD"),
            "explicit invoice currency must be EUR or USD"
        );
        let invoice_date = date(&amex::iso_date(invoice_date)?)?;
        let units = amex::decimal_comma_hundredths(total)?;
        let (eur_total, usd_total): (
            Option<Inline<Currency<Euro>>>,
            Option<Inline<Currency<UsDollar>>>,
        ) = match currency {
            "EUR" => (
                Some(
                    Amount::<Euro>::from_units(units, 2)?
                        .try_to_inline()
                        .context("encode exact invoice EUR total")?,
                ),
                None,
            ),
            "USD" => (
                None,
                Some(
                    Amount::<UsDollar>::from_units(units, 2)?
                        .try_to_inline()
                        .context("encode exact invoice USD total")?,
                ),
            ),
            _ => unreachable!("invoice currency was checked before parsing"),
        };
        let (file, content, original, media_type) = match original {
            InvoiceOriginal::Pdf { bytes, name } => {
                check_original_format(&bytes, "application/pdf")?;
                let mut original = faculties::files::stage(bytes, name, "application/pdf")?;
                let file = original
                    .root()
                    .expect("canonical Files fragment has a root");
                let content = find!(
                    content: Content,
                    pattern!(original.facts(), [{ file @ faculties::schemas::files::file::content: ?content }])
                )
                .next()
                .context("invoice original has content")?;
                original += entity! { ExclusiveId::force_ref(&file) @ board::by: actor };
                (file, content, Some(original), "application/pdf")
            }
            InvoiceOriginal::File {
                file,
                content,
                media_type,
            } => {
                ensure!(
                    matches!(media_type, "application/pdf" | "text/html"),
                    "original media type must be application/pdf or text/html"
                );
                (file, content, None, media_type)
            }
        };
        let mut change = entity! { metadata::tag: KIND_INVOICE, source_bytes: content };
        let invoice_id = change.root().expect("invoice has a root");
        change += entity! { ExclusiveId::force_ref(&invoice_id) @
            source_file: file, board::by: actor,
        };
        let observation = entity! {
            metadata::tag: KIND_INVOICE_METADATA, invoice: invoice_id,
            purchase::date: invoice_date, purchase::external_number: number,
            metadata::name: supplier, purchase::gross_total?: eur_total,
            gross_total_usd?: usd_total,
            board::by: actor, metadata::supersedes*: supersedes.iter(),
        };
        let metadata_claim = observation.root().expect("metadata assertion has a root");
        change += observation;
        self.with_view(true, |store, signer, collection, facts, snapshot| {
            for previous in supersedes {
                ensure!(exists!(pattern!(facts, [{ previous @ metadata::tag: KIND_INVOICE_METADATA, invoice: invoice_id }])), "superseded metadata claim is not for this invoice");
            }
            let files_handle = self
                .storage
                .collection_routes()
                .and_then(|r| r.get(&faculties::schemas::files::DEFAULT_SCOPE_ID))
                .copied()
                .context("Files collection role is unconfigured")?;
            if let Some(original) = original {
                let files = open_workspace_write_in(snapshot, signer.verifying_key(), files_handle)?;
                store.commit(files, signer, original)?;
            } else {
                use faculties::schemas::files::{file as file_schema, KIND_FILE, KIND_MEDIA_TYPE};
                let files = open_workspace_read_in(snapshot, signer.verifying_key(), files_handle)?;
                let files_facts: TribleSet = files.read(snapshot)?;
                ensure!(exists!(pattern!(&files_facts, [
                    { file @ metadata::tag: KIND_FILE, file_schema::content: content,
                        file_schema::media_type: _?format },
                    { _?format @ metadata::tag: KIND_MEDIA_TYPE, metadata::name: media_type },
                ])), "selected original file/content/media type is not present in routed Files");
                let bytes: Bytes = BlobStoreGet::get(snapshot, content)
                    .context("read selected original RawBytes")?;
                check_original_format(&bytes, media_type)?;
            }
            store.commit(collection, signer, change)?;
            Ok(InvoiceAdded {
                file: hex(file),
                invoice: hex(invoice_id),
                metadata_claim: hex(metadata_claim),
            })
        })
    }

    /// Purpose attaches to the WHOLE subject. Unknown is absence of a claim;
    /// contradictory live claims remain visible. Supersession is explicit.
    pub fn classify(
        &self,
        subject: Id,
        purpose_value: Id,
        actor: Id,
        note: &str,
        supersedes: &[Id],
    ) -> Result<Id> {
        ensure!(
            [COMPANY, PRIVATE].contains(&purpose_value),
            "purpose must be company or private"
        );
        ensure!(!note.trim().is_empty(), "classification needs a reason");
        self.with_view(true, |store, signer, collection, facts, _| {
            ensure!(exists!(pattern!(facts, [{ subject @ metadata::tag: KIND_ACTIVITY }])) || exists!(pattern!(facts, [{ subject @ metadata::tag: KIND_AMEX_TRANSACTION }])) || exists!(pattern!(facts, [{ subject @ metadata::tag: KIND_INVOICE }])), "unknown activity or invoice");
            for previous in supersedes {
                ensure!(exists!(pattern!(facts, [{ previous @ metadata::tag: KIND_PURPOSE, purpose_of: subject }])) || exists!(pattern!(facts, [
                    { previous @ metadata::tag: KIND_PURPOSE, purpose_of: _?old },
                    { legacy_activity: _?old, transaction: subject },
                ])), "superseded purpose claim is not for this subject or an explicitly associated legacy activity");
            }
            let mut change = entity! {
                metadata::tag: KIND_PURPOSE, purpose_of: subject,
                purpose: purpose_value, board::by: actor, metadata::supersedes*: supersedes.iter(),
            };
            let claim = change.root().expect("purpose claim has a root");
            change += entity! { ExclusiveId::force_ref(&claim) @ metadata::description: note };
            store.commit(collection, signer, change)?;
            Ok(claim)
        })
    }

    /// Explicit documentary association only. No allocation, settlement state,
    /// ownership or expense purpose is implied by matching a card activity.
    pub fn match_invoice(
        &self,
        activity_id: Id,
        invoice_id: Id,
        actor: Id,
        note: &str,
        supersedes: &[Id],
    ) -> Result<Id> {
        ensure!(!note.trim().is_empty(), "match needs a reason");
        self.with_view(true, |store, signer, collection, facts, _| {
            ensure!(exists!(pattern!(facts, [{ activity_id @ metadata::tag: KIND_ACTIVITY }])) || exists!(pattern!(facts, [{ activity_id @ metadata::tag: KIND_AMEX_TRANSACTION }])), "unknown card activity");
            ensure!(exists!(pattern!(facts, [{ invoice_id @ metadata::tag: KIND_INVOICE }])), "unknown invoice");
            for previous in supersedes {
                ensure!(exists!(pattern!(facts, [{ previous @ metadata::tag: KIND_MATCH, activity: activity_id }])) || exists!(pattern!(facts, [
                    { previous @ metadata::tag: KIND_MATCH, activity: _?old },
                    { legacy_activity: _?old, transaction: activity_id },
                ])), "superseded match is not for this activity or an explicitly associated legacy activity");
            }
            let mut change = entity! {
                metadata::tag: KIND_MATCH, activity: activity_id, invoice: invoice_id,
                board::by: actor, metadata::supersedes*: supersedes.iter(),
            };
            let claim = change.root().expect("match claim has a root");
            change += entity! { ExclusiveId::force_ref(&claim) @ metadata::description: note };
            store.commit(collection, signer, change)?;
            Ok(claim)
        })
    }

    pub fn review(&self, month: Month) -> Result<Review> {
        let min = date(&month.start())?;
        let (end, _): (Epoch, Epoch) = date(&month.end())?
            .try_from_inline()
            .map_err(|e| anyhow::anyhow!("decode month boundary: {e:?}"))?;
        let last = end - Duration::from_parts(0, 1);
        let max: Date = (last, last)
            .try_to_inline()
            .map_err(|e| anyhow::anyhow!("encode month boundary: {e:?}"))?;
        self.with_view(false, |_, _, _, facts, reader| {
            let order = ObservationOrder::new(facts, metadata::supersedes.id());
            let accounts = find!(account_id: Id, pattern!(facts, [{ ?account_id @ metadata::tag: KIND_ACCOUNT }]))
                .map(|id| Ok(AccountReview { id: hex(id), labels: text_values(facts, reader, id, &metadata::name)? }))
                .collect::<Result<Vec<_>>>()?;
            let activities = transactions::review(facts, reader, min, max)?;
            let mut invoices = Vec::new();
            for (id, observation) in find!((id: Id, observation: Id), and!(
                pattern!(facts, [
                    { ?id @ metadata::tag: KIND_INVOICE },
                    { ?observation @ metadata::tag: KIND_INVOICE_METADATA, invoice: ?id },
                ]), maximal(observation, &order),
            )) {
                if !exists!((day: Date), and!(pattern!(facts, [{ observation @ purchase::date: ?day }]), value_range(day, min, max))) { continue; }
                let dates = find!(day: Date, pattern!(facts, [{ observation @ purchase::date: ?day }])).map(date_text).collect::<Result<Vec<_>>>()?;
                // Each typed attribute carries its own denomination. Query
                // independently so external multivalues remain a set, never
                // synthetic EUR/USD pairs or a currency-free numeric total.
                let mut amounts: Vec<String> = find!(amount: Euros, pattern!(facts, [{ observation @ purchase::gross_total: ?amount }])).map(|v| v.to_string()).collect();
                amounts.extend(find!(amount: Amount<UsDollar>, pattern!(facts, [{ observation @ gross_total_usd: ?amount }])).map(|v| v.to_string()));
                amounts.sort();
                amounts.dedup();
                let actors = find!(actor: Id, pattern!(facts, [{ observation @ board::by: ?actor }])).map(hex).collect();
                let mut matched_activities: std::collections::BTreeSet<String> = find!(activity_id: Id, temp!((claim), and!(
                    pattern!(facts, [{ ?claim @ metadata::tag: KIND_MATCH, invoice: id, activity: ?activity_id }]),
                    maximal(claim, &order),
                ))).filter(|subject| !exists!(pattern!(facts, [{ legacy_activity: *subject, transaction: _?target }]))).map(hex).collect();
                matched_activities.extend(transactions::matched_transactions(facts, id));
                invoices.push(InvoiceReview {
                    id: hex(id), metadata_claim: hex(observation), dates, amounts, actors,
                    numbers: text_values(facts, reader, observation, &purchase::external_number)?, suppliers: text_values(facts, reader, observation, &metadata::name)?,
                    source_files: source_files(facts, id), purposes: purpose_claims(facts, reader, id)?, matched_activities: matched_activities.into_iter().collect(),
                });
            }
            Ok(Review { accounts, activities, invoices })
        })
    }
}

fn purpose_claims(facts: &TribleSet, reader: &FacultySnapshot, subject: Id) -> Result<Vec<Claim>> {
    let order = ObservationOrder::new(facts, metadata::supersedes.id());
    find!(claim: Id, and!(
        pattern!(facts, [{ ?claim @ metadata::tag: KIND_PURPOSE, purpose_of: subject }]),
        maximal(claim, &order),
    ))
    .filter(|id| !exists!(pattern!(facts, [{ id @ original_claim: _?original }])))
    .map(|id| {
        Ok(Claim {
            id: hex(id),
            values: find!(value: Id, pattern!(facts, [{ id @ purpose: ?value }]))
                .map(|value| {
                    if value == COMPANY {
                        "company".into()
                    } else if value == PRIVATE {
                        "private".into()
                    } else {
                        hex(value)
                    }
                })
                .collect(),
            actors: find!(actor: Id, pattern!(facts, [{ id @ board::by: ?actor }]))
                .map(hex)
                .collect(),
            notes: text_values(facts, reader, id, &metadata::description)?,
            original_claim: None,
            migration_actors: vec![],
        })
    })
    .collect()
}
fn match_claims(facts: &TribleSet, reader: &FacultySnapshot, subject: Id) -> Result<Vec<Claim>> {
    let order = ObservationOrder::new(facts, metadata::supersedes.id());
    find!(claim: Id, and!(
        pattern!(facts, [{ ?claim @ metadata::tag: KIND_MATCH, activity: subject }]),
        maximal(claim, &order),
    ))
    .filter(|id| !exists!(pattern!(facts, [{ id @ original_claim: _?original }])))
    .map(|id| {
        Ok(Claim {
            id: hex(id),
            values: find!(value: Id, pattern!(facts, [{ id @ invoice: ?value }]))
                .map(hex)
                .collect(),
            actors: find!(actor: Id, pattern!(facts, [{ id @ board::by: ?actor }]))
                .map(hex)
                .collect(),
            notes: text_values(facts, reader, id, &metadata::description)?,
            original_claim: None,
            migration_actors: vec![],
        })
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use tempfile::TempDir;
    use triblespace::core::collection::{AdmissionPolicy, CollectionPolicy};

    struct Fixture {
        storage: Storage,
        path: std::path::PathBuf,
        _dir: TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("synthetic.pile");
            let key = dir.path().join("synthetic.key");
            fs::File::create(&path).unwrap();
            faculties::storage::initialize_signer(&path, Some(&key)).unwrap();
            let storage = Storage::shared_local(path.clone(), Some(key));
            let routes = storage
                .with_pile(|store, signer| {
                    let policy = AdmissionPolicy::direct(signer.verifying_key());
                    let policy = CollectionPolicy::new(policy.clone(), policy);
                    let erp = store.collection("synthetic bookkeeping", policy.clone())?;
                    let files = store.collection("synthetic files", policy)?;
                    Ok(BTreeMap::from([
                        (DEFAULT_SCOPE_ID, erp.handle()),
                        (faculties::schemas::files::DEFAULT_SCOPE_ID, files.handle()),
                    ]))
                })
                .unwrap();
            Self {
                storage: storage.with_collections(routes),
                path,
                _dir: dir,
            }
        }
        fn ops(&self) -> Bookkeeping {
            Bookkeeping::with_storage(self.storage.clone())
        }
        fn length(&self) -> u64 {
            fs::metadata(&self.path).unwrap().len()
        }
        fn put_file(&self, bytes: &[u8], name: &str, mime: &str) -> (Id, Content) {
            let original =
                faculties::files::stage(Bytes::from(bytes.to_vec()), name, mime).unwrap();
            let file = original.root().unwrap();
            let content = find!(
                content: Content,
                pattern!(original.facts(), [{ file @ faculties::schemas::files::file::content: ?content }])
            )
            .next()
            .expect("staged test original has content");
            self.extend_files(original);
            (file, content)
        }
        fn extend_files(&self, fragment: Fragment) {
            self.ops()
                .with_view(false, |store, signer, _, _, snapshot| {
                    let handle = self.storage.collection_routes().unwrap()
                        [&faculties::schemas::files::DEFAULT_SCOPE_ID];
                    let files = open_workspace_write_in(snapshot, signer.verifying_key(), handle)?;
                    store.commit(files, signer, fragment)?;
                    Ok(())
                })
                .unwrap();
        }
        fn file_facts(&self) -> TribleSet {
            self.ops()
                .with_view(false, |_, signer, _, _, snapshot| {
                    let handle = self.storage.collection_routes().unwrap()
                        [&faculties::schemas::files::DEFAULT_SCOPE_ID];
                    let files = open_workspace_read_in(snapshot, signer.verifying_key(), handle)?;
                    Ok(files.read(snapshot)?)
                })
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.storage.close().unwrap();
        }
    }
    fn selected() -> Month {
        Month::parse("2026-09").unwrap()
    }
    fn csv(rows: &str) -> Bytes {
        Bytes::from(format!("{}\n{rows}", amex::HEADERS.join(",")).into_bytes())
    }
    fn identical_rows() -> Bytes {
        csv("01/09/2026,Synthetic shop,\"7,05\",,,,,,,same-reference,\n01/09/2026,Synthetic shop,\"7,05\",,,,,,,distinct-reference,\n")
    }

    fn extended_csv(rows: &str) -> Bytes {
        Bytes::from(
            format!("{},Karteninhaber,Konto #\n{rows}", amex::HEADERS.join(",")).into_bytes(),
        )
    }

    #[test]
    fn amex_identity_overlap_conflicting_observations_and_explicit_card_mapping() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let a = ops.add_account("Synthetic subcard", actor).unwrap();
        let b = ops.add_account("Synthetic company card", actor).unwrap();
        let first = ops.import_amex(csv("01/09/2026,Shop,\"7,05\",detail,,,,,,r1,\n02/09/2026,Shop,\"7,05\",,,,,,,r2,\n"), "old.csv", a, "EUR", selected(), actor).unwrap();
        let overlap = ops.import_amex(csv("02/09/2026,Shop,\"7,05\",,,,,,,r2,\n01/09/2026,Shop,\"7,05\",detail,,,,,,r1,\n03/09/2026,Another,\"1,00\",,,,,,,r3,\n"), "new.csv", a, "EUR", selected(), actor).unwrap();
        assert_eq!(first.activities[0], overlap.activities[1]);
        assert_eq!(first.activities[1], overlap.activities[0]);
        assert_ne!(first.activities[0], first.activities[1]);
        let changed = ops
            .import_amex(
                csv("01/10/2026,Corrected shop,\"8,05\",changed detail,,,,,,r1,\n"),
                "correction.csv",
                a,
                "EUR",
                Month::parse("2026-10").unwrap(),
                actor,
            )
            .unwrap();
        assert_eq!(first.activities[0], changed.activities[0]);
        let extended = extended_csv("01/09/2026,Shop,\"7,05\",detail,,,,,,r1,,Synthetic Holder A,-0000\n01/09/2026,Shop,\"7,05\",detail,,,,,,r1,,Synthetic Holder B,-0000\n");
        let mappings = vec![
            AccountMapping {
                cardholder: "Synthetic Holder A".into(),
                card_number: "-0000".into(),
                account: hex(a),
            },
            AccountMapping {
                cardholder: "Synthetic Holder B".into(),
                card_number: "-0000".into(),
                account: hex(b),
            },
        ];
        let before = f.length();
        assert!(ops
            .import_amex(extended.clone(), "bad.csv", a, "EUR", selected(), actor)
            .is_err());
        assert!(ops
            .import_amex_mapped(
                extended.clone(),
                "bad.csv",
                None,
                &mappings[..1],
                "EUR",
                selected(),
                actor
            )
            .is_err());
        assert_eq!(before, f.length());
        let mapped = ops
            .import_amex_mapped(
                extended.clone(),
                "family.csv",
                None,
                &mappings,
                "EUR",
                selected(),
                actor,
            )
            .unwrap();
        assert_eq!(first.activities[0], mapped.activities[0]);
        assert_ne!(mapped.activities[0], mapped.activities[1]);
        let before = f.length();
        let retry = ops
            .import_amex_mapped(
                extended,
                "family.csv",
                None,
                &mappings,
                "EUR",
                selected(),
                actor,
            )
            .unwrap();
        assert_eq!(mapped.activities, retry.activities);
        assert_eq!(before, f.length());
        let review = ops.review(selected()).unwrap();
        assert_eq!(review.activities.len(), 4);
        let transaction_review = review
            .activities
            .iter()
            .find(|r| r.id == first.activities[0])
            .unwrap();
        assert!(!transaction_review.legacy);
        assert_eq!(transaction_review.observations.len(), 2);
        assert!(transaction_review
            .observations
            .iter()
            .any(|o| o.dates == ["2026-09-01"] && o.amounts == ["7.05 EUR"]));
        assert!(transaction_review
            .observations
            .iter()
            .any(|o| o.dates == ["2026-10-01"] && o.amounts == ["8.05 EUR"]));
        assert!(transaction_review
            .observations
            .iter()
            .flat_map(|o| &o.sources)
            .any(|s| s.cardholders == ["Synthetic Holder A"]));
        assert!(
            transaction_review.purposes.is_empty(),
            "card ownership cannot classify an expense"
        );
        let original_observation = transaction_review
            .observations
            .iter()
            .find(|o| o.dates == ["2026-09-01"])
            .unwrap();
        assert_eq!(original_observation.sources.len(), 3);
        assert_eq!(
            ops.review(Month::parse("2026-10").unwrap())
                .unwrap()
                .activities
                .len(),
            1
        );
    }

    #[test]
    fn absent_reference_refuses_entire_import_without_publication() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let a = ops.add_account("Synthetic account", actor).unwrap();
        let before = f.length();
        let error = ops
            .import_amex(
                csv(
                    "01/09/2026,Valid,\"1,00\",,,,,,,r1,\n01/09/2026,Unresolved,\"2,00\",,,,,,,,\n",
                ),
                "blank.csv",
                a,
                "EUR",
                selected(),
                actor,
            )
            .unwrap_err();
        assert!(error.to_string().contains("no provider reference"));
        assert_eq!(before, f.length());
        assert!(ops.review(selected()).unwrap().activities.is_empty());
    }

    #[test]
    fn explicit_legacy_migration_preserves_original_frontier_and_late_writers() {
        let f = Fixture::new();
        let ops = f.ops();
        let original_actor = genid().id;
        let migration_actor = genid().id;
        let account_id = ops
            .add_account("Synthetic old subcard", original_actor)
            .unwrap();
        let bytes = csv("01/09/2026,Shop,\"7,05\",detail,,,,,,r1,\n");
        let (file, content) = f.put_file(&bytes, "legacy.csv", "text/csv");
        let (unrelated_file, unrelated_content) =
            f.put_file(b"unrelated", "unrelated.csv", "text/csv");
        let old = genid(); // A random legacy ID must remain readable.
        let amount: Inline<Currency<Euro>> = amex::euros("7,05").unwrap().try_to_inline().unwrap();
        ops.with_view(true, |store, signer, collection, _, _| {
            store.commit(collection, signer, entity! { &old @ metadata::tag: KIND_ACTIVITY,
                account: account_id, source_bytes*: [content, unrelated_content], row_index*: [1_u64, 2_u64], source_file*: [file, unrelated_file],
                provider_reference: "r1", payment::date: date("2026-09-01")?, payment::amount: amount })?;
            Ok(())
        }).unwrap();
        let invoice_added = ops
            .add_invoice(
                Bytes::from(b"%PDF-1.7\nsynthetic".to_vec()),
                "synthetic.pdf",
                "01/09/2026",
                "S1",
                "Supplier",
                "7,05",
                "EUR",
                original_actor,
                &[],
            )
            .unwrap();
        let invoice_id = Id::from_hex(&invoice_added.invoice).unwrap();
        let a = ops
            .classify(old.id, PRIVATE, original_actor, "First decision", &[])
            .unwrap();
        let b = ops
            .classify(old.id, COMPANY, original_actor, "Revised decision", &[a])
            .unwrap();
        let c = ops
            .classify(old.id, PRIVATE, genid().id, "Concurrent disagreement", &[])
            .unwrap();
        let matched = ops
            .match_invoice(
                old.id,
                invoice_id,
                original_actor,
                "Inspected original",
                &[],
            )
            .unwrap();
        let selection = Migration {
            legacy: hex(old.id),
            content: inlineencodings::Hash::<inlineencodings::Blake3>::to_hex(
                &inlineencodings::Handle::to_hash(content),
            ),
            row: 1,
            account: hex(account_id),
        };
        let before = f.length();
        let plan = ops
            .migrate_amex(&[selection], selected(), migration_actor, false)
            .unwrap();
        assert_eq!(before, f.length(), "dry run publishes no data");
        assert!(!plan[0].applied);
        assert_eq!(
            plan[0].original_claims.len(),
            4,
            "history is retained, not just its current frontier"
        );
        let selection = || Migration {
            legacy: hex(old.id),
            content: inlineencodings::Hash::<inlineencodings::Blake3>::to_hex(
                &inlineencodings::Handle::to_hash(content),
            ),
            row: 1,
            account: hex(account_id),
        };
        let invalid = Migration {
            legacy: hex(genid().id),
            ..selection()
        };
        assert!(ops
            .migrate_amex(&[selection(), invalid], selected(), migration_actor, true)
            .is_err());
        assert_eq!(
            before,
            f.length(),
            "invalid final selection makes the batch a no-op"
        );
        let applied = ops
            .migrate_amex(&[selection()], selected(), migration_actor, true)
            .unwrap();
        assert_eq!(plan[0].transaction, applied[0].transaction);
        assert_eq!(plan[0].projections, applied[0].projections);
        let before = f.length();
        ops.migrate_amex(&[selection()], selected(), migration_actor, true)
            .unwrap();
        assert_eq!(before, f.length(), "repeat migration adds no facts");
        let review = ops.review(selected()).unwrap();
        assert_eq!(review.activities.len(), 1);
        let row = &review.activities[0];
        assert_eq!(row.id, plan[0].transaction);
        assert_eq!(row.purposes.len(), 2);
        assert!(row
            .purposes
            .iter()
            .all(|p| p.migration_actors == [hex(migration_actor)]));
        assert!(row.purposes.iter().any(|p| p.original_claim == Some(hex(b))
            && p.actors == [hex(original_actor)]
            && p.notes == ["Revised decision"]));
        assert!(row
            .purposes
            .iter()
            .any(|p| p.original_claim == Some(hex(c))));
        assert_eq!(row.matches[0].original_claim, Some(hex(matched)));
        assert_eq!(
            review.invoices[0].matched_activities,
            [plan[0].transaction.clone()]
        );
        assert!(
            row.observations
                .iter()
                .flat_map(|o| &o.sources)
                .all(|s| s.files.is_empty()),
            "uncorrelated legacy Files alternatives must not migrate"
        );
        let projected: Vec<Id> = row
            .purposes
            .iter()
            .map(|p| Id::from_hex(&p.id).unwrap())
            .collect();
        let target = Id::from_hex(&row.id).unwrap();
        ops.classify(
            target,
            COMPANY,
            original_actor,
            "Explicit new-subject resolution",
            &projected,
        )
        .unwrap();
        ops.migrate_amex(&[selection()], selected(), migration_actor, true)
            .unwrap();
        assert_eq!(
            ops.review(selected()).unwrap().activities[0].purposes.len(),
            1,
            "replay cannot revive superseded projections"
        );
        let late = ops
            .classify(old.id, PRIVATE, genid().id, "Late legacy writer", &[])
            .unwrap();
        let late_match = ops
            .match_invoice(old.id, invoice_id, genid().id, "Late legacy match", &[])
            .unwrap();
        let review = ops.review(selected()).unwrap();
        assert!(review.activities[0]
            .purposes
            .iter()
            .any(|p| p.original_claim == Some(hex(late))));
        assert!(review.activities[0]
            .matches
            .iter()
            .any(|p| p.original_claim == Some(hex(late_match))));
        assert_eq!(review.invoices[0].matched_activities, [hex(target)]);
        ops.migrate_amex(&[selection()], selected(), migration_actor, true)
            .unwrap();
        let review = ops.review(selected()).unwrap();
        assert_eq!(
            review.activities[0]
                .purposes
                .iter()
                .filter(|p| p.original_claim == Some(hex(late)))
                .count(),
            1
        );
        assert_eq!(
            review.activities[0]
                .matches
                .iter()
                .filter(|p| p.original_claim == Some(hex(late_match)))
                .count(),
            1
        );
        // Claims emitted by old writers after migration can be resolved from
        // exactly the IDs returned by new review, without a migration rerun.
        let late2 = ops
            .classify(old.id, PRIVATE, genid().id, "Another late decision", &[])
            .unwrap();
        let late_match2 = ops
            .match_invoice(old.id, invoice_id, genid().id, "Another late match", &[])
            .unwrap();
        let second_migrator = genid().id;
        let extra_association = genid();
        ops.with_view(true, |store, signer, collection, _, _| {
            store.commit(collection, signer, entity! { &extra_association @ legacy_activity: old.id, transaction: target, board::by: second_migrator })?;
            Ok(())
        }).unwrap();
        let review = ops.review(selected()).unwrap();
        let late_review = review.activities[0]
            .purposes
            .iter()
            .find(|p| p.id == hex(late2))
            .unwrap();
        let actors: std::collections::BTreeSet<_> =
            late_review.migration_actors.iter().cloned().collect();
        assert_eq!(
            actors,
            std::collections::BTreeSet::from([hex(migration_actor), hex(second_migrator)])
        );
        ops.classify(
            target,
            COMPANY,
            original_actor,
            "Resolve reviewed late ID",
            &[late2],
        )
        .unwrap();
        ops.match_invoice(
            target,
            invoice_id,
            original_actor,
            "Resolve reviewed late match ID",
            &[late_match2],
        )
        .unwrap();
        let review = ops.review(selected()).unwrap();
        assert!(!review.activities[0]
            .purposes
            .iter()
            .any(|p| p.original_claim == Some(hex(late2))));
        assert!(!review.activities[0]
            .matches
            .iter()
            .any(|p| p.original_claim == Some(hex(late_match2))));
        let old2 = genid();
        ops.with_view(true, |store, signer, collection, _, _| {
            store.commit(
                collection,
                signer,
                entity! { &old2 @ metadata::tag: KIND_ACTIVITY,
                account: account_id, source_bytes: content, row_index: 1_u64,
                payment::date: date("2026-09-01")?, payment::amount: amount },
            )?;
            Ok(())
        })
        .unwrap();
        let second_original = ops
            .classify(
                old2.id,
                PRIVATE,
                second_migrator,
                "Independent original",
                &[],
            )
            .unwrap();
        assert!(ops
            .classify(
                target,
                COMPANY,
                original_actor,
                "Must not cross unrelated subjects",
                &[second_original]
            )
            .is_err());
        ops.migrate_amex(
            &[Migration {
                legacy: hex(old2.id),
                ..selection()
            }],
            selected(),
            migration_actor,
            true,
        )
        .unwrap();
        let review = ops.review(selected()).unwrap();
        assert_eq!(
            review.activities.len(),
            1,
            "two legacy sources map to one transaction"
        );
        assert!(review.activities[0]
            .purposes
            .iter()
            .any(|p| p.original_claim == Some(hex(second_original))));
        assert_eq!(review.invoices[0].matched_activities, [hex(target)]);
        ops.with_view(false, |_, _, _, facts, _| {
            assert!(exists!(
                pattern!(facts, [{ a @ purpose_of: old.id, purpose: PRIVATE }])
            ));
            assert!(exists!(
                pattern!(facts, [{ b @ purpose_of: old.id, metadata::supersedes: a }])
            ));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn same_reference_repeated_sources_and_opaque_open_world_observations() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let account_id = ops.add_account("Synthetic", actor).unwrap();
        let input = ops
            .import_amex(
                csv("01/09/2026,Shop,\"1,00\",,,,,,,r,\n01/09/2026,Shop,\"1,00\",,,,,,,r,\n"),
                "duplicate.csv",
                account_id,
                "EUR",
                selected(),
                actor,
            )
            .unwrap();
        assert_eq!(input.activities[0], input.activities[1]);
        let review = ops.review(selected()).unwrap();
        assert_eq!(review.activities.len(), 1);
        assert_eq!(review.activities[0].observations.len(), 1);
        assert_eq!(review.activities[0].observations[0].sources.len(), 2);
        let opaque = genid();
        let observation = genid();
        let claim = genid();
        let other_actor = genid().id;
        let amounts: [Inline<Currency<Euro>>; 2] = [
            amex::euros("1,00").unwrap().try_to_inline().unwrap(),
            amex::euros("2,00").unwrap().try_to_inline().unwrap(),
        ];
        ops.with_view(true, |store, signer, collection, _, _| {
            let change = entity! { &opaque @ metadata::tag: KIND_AMEX_TRANSACTION, account: account_id, provider_reference: "opaque" }
                + entity! { &observation @ metadata::tag: KIND_AMEX_OBSERVATION, transaction: opaque.id,
                    payment::date*: [date("2026-09-01")?, date("2026-10-01")?],
                    payment::amount*: amounts, metadata::description: "Unrelated annotations remain permitted" }
                + entity! { &claim @ metadata::tag: KIND_PURPOSE, purpose_of: opaque.id, purpose*: [COMPANY, PRIVATE], board::by*: [actor, other_actor] };
            store.commit(collection, signer, change)?;
            Ok(())
        }).unwrap();
        let review = ops.review(selected()).unwrap();
        let row = review
            .activities
            .iter()
            .find(|r| r.id == hex(opaque.id))
            .unwrap();
        assert_eq!(
            row.observations.len(),
            1,
            "multivalues are sets, never four invented pairs"
        );
        assert_eq!(row.observations[0].dates.len(), 2);
        assert_eq!(row.observations[0].amounts.len(), 2);
        assert_eq!(row.purposes.len(), 1);
        assert_eq!(row.purposes[0].values.len(), 2);
        assert_eq!(row.purposes[0].actors.len(), 2);
    }

    #[test]
    fn originals_occurrences_retry_aliases_accounts_and_months() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let a = ops.add_account("Example Person — Amex", actor).unwrap();
        let b = ops.add_account("Example Person — Amex", actor).unwrap();
        assert_ne!(a, b, "labels must never identify accounts");
        let empty = ops.review(selected()).unwrap();
        assert_eq!(
            empty.accounts.len(),
            2,
            "zero-activity accounts remain discoverable"
        );
        assert!(empty
            .accounts
            .iter()
            .all(|r| r.labels == ["Example Person — Amex"]));
        let input = identical_rows();
        let first = ops
            .import_amex(input.clone(), "activity.csv", a, "EUR", selected(), actor)
            .unwrap();
        assert_eq!(first.rows, 2);
        assert_ne!(
            first.activities[0], first.activities[1],
            "equal legitimate rows survive"
        );
        let before = f.length();
        let again = ops
            .import_amex(input.clone(), "activity.csv", a, "EUR", selected(), actor)
            .unwrap();
        assert_eq!(first.activities, again.activities);
        assert_eq!(before, f.length(), "exact retry must not append");
        let renamed = ops
            .import_amex(input.clone(), "renamed.csv", a, "EUR", selected(), actor)
            .unwrap();
        assert_eq!(
            first.activities, renamed.activities,
            "leaf filename is not occurrence identity"
        );
        let second_account = ops
            .import_amex(input, "activity.csv", b, "EUR", selected(), actor)
            .unwrap();
        assert!(first
            .activities
            .iter()
            .all(|a| !second_account.activities.contains(a)));
        // A differently exported file is independent evidence for the same
        // provider record under the explicit account/reference convention.
        let other = csv("01/09/2026,Synthetic shop,\"7,05\",,,,,,,same-reference,\n");
        let overlap = ops
            .import_amex(other, "overlap.csv", a, "EUR", selected(), actor)
            .unwrap();
        assert!(first.activities.contains(&overlap.activities[0]));
        let months = csv("31/08/2026,August,\"1,00\",,,,,,,aug,\n01/09/2026,September,\"2,00\",,,,,,,sep,\n01/10/2026,October,\"3,00\",,,,,,,oct,\n");
        let sep = ops
            .import_amex(months.clone(), "months.csv", a, "EUR", selected(), actor)
            .unwrap();
        let aug = ops
            .import_amex(
                months.clone(),
                "months.csv",
                a,
                "EUR",
                Month::parse("2026-08").unwrap(),
                actor,
            )
            .unwrap();
        assert_eq!(sep.rows, 1);
        assert_eq!(aug.rows, 1);
        assert_ne!(sep.activities, aug.activities);
        let file = Id::from_hex(&sep.file).unwrap();
        f.storage.with_store(|store, signer, _| {
            let snapshot = store.snapshot()?;
            let files = open_workspace_read_in(&snapshot, signer.verifying_key(), f.storage.collection_routes().unwrap()[&faculties::schemas::files::DEFAULT_SCOPE_ID])?;
            let facts: TribleSet = files.read(&snapshot)?;
            for content in find!(content: Content, pattern!(&facts, [{ file @ faculties::schemas::files::file::content: ?content }])) {
                let original: Bytes = BlobStoreGet::get(&snapshot, content)?;
                assert_eq!(original.as_ref(), months.as_ref());
            }
            Ok(())
        }).unwrap();
        let before_read = f.length();
        let review = ops.review(selected()).unwrap();
        assert_eq!(review.activities.len(), 5);
        assert!(review
            .activities
            .iter()
            .all(|r| r.purposes.is_empty() && r.matches.is_empty()));
        assert_eq!(before_read, f.length(), "review is read-only");
    }

    #[test]
    fn invalid_input_and_unconfigured_files_publish_nothing() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let account_id = ops.add_account("Synthetic account", actor).unwrap();
        let before = f.length();
        assert!(ops
            .import_amex(
                identical_rows(),
                "activity.csv",
                account_id,
                "USD",
                selected(),
                actor
            )
            .is_err());
        assert!(ops
            .import_amex(
                csv("31/09/2026,shop,\"1,00\",,,,,,,ref,\n"),
                "bad.csv",
                account_id,
                "EUR",
                selected(),
                actor
            )
            .is_err());
        assert!(ops
            .import_amex(
                identical_rows(),
                "activity.csv",
                genid().id,
                "EUR",
                selected(),
                actor
            )
            .is_err());
        let routes = BTreeMap::from([(
            DEFAULT_SCOPE_ID,
            f.storage.collection_routes().unwrap()[&DEFAULT_SCOPE_ID],
        )]);
        let without_files = Bookkeeping::with_storage(f.storage.clone().with_collections(routes));
        assert!(without_files
            .import_amex(
                identical_rows(),
                "activity.csv",
                account_id,
                "EUR",
                selected(),
                actor
            )
            .is_err());
        assert!(ops
            .add_invoice(
                Bytes::from(b"%PDF-1.7\nsynthetic foreign invoice".to_vec()),
                "foreign.pdf",
                "01/09/2026",
                "SYN-USD",
                "Synthetic supplier",
                "8,19",
                "JPY",
                actor,
                &[]
            )
            .is_err());
        for currency in ["EUR", "USD"] {
            for total in [
                "8.19",
                "8,1",
                "1.000,00",
                "1,000",
                "NaN",
                "-",
                " 8,19",
                "170141183460469231731687303715884105727,00",
            ] {
                assert!(ops
                    .add_invoice(
                        Bytes::from(b"%PDF-1.7\nsynthetic malformed amount".to_vec()),
                        "malformed.pdf",
                        "01/09/2026",
                        "SYN-BAD",
                        "Synthetic supplier",
                        total,
                        currency,
                        actor,
                        &[],
                    )
                    .is_err());
            }
        }
        assert_eq!(before, f.length());
    }

    #[test]
    fn original_usd_preserves_eur_identity_and_documentary_matching() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let account_id = ops.add_account("Synthetic EUR card", actor).unwrap();
        let imported = ops
            .import_amex(
                identical_rows(),
                "activity.csv",
                account_id,
                "EUR",
                selected(),
                actor,
            )
            .unwrap();
        let activity_id = Id::from_hex(&imported.activities[0]).unwrap();
        let pdf = Bytes::from(b"%PDF-1.7\nsynthetic original USD receipt".to_vec());
        let first = ops
            .add_invoice(
                pdf.clone(),
                "original.pdf",
                "01/09/2026",
                "SYN-CURRENCY",
                "Synthetic supplier",
                "7,05",
                "USD",
                actor,
                &[],
            )
            .unwrap();
        let invoice_id = Id::from_hex(&first.invoice).unwrap();
        let before_retry = f.length();
        let retry = ops
            .add_invoice(
                pdf.clone(),
                "original.pdf",
                "01/09/2026",
                "SYN-CURRENCY",
                "Synthetic supplier",
                "7,05",
                "USD",
                actor,
                &[],
            )
            .unwrap();
        assert_eq!(first.file, retry.file);
        assert_eq!(first.invoice, retry.invoice);
        assert_eq!(first.metadata_claim, retry.metadata_claim);
        assert_eq!(before_retry, f.length(), "exact USD retry must not append");
        let before_review = f.length();
        let review = ops.review(selected()).unwrap();
        assert_eq!(review.invoices[0].amounts, ["7.05 USD"]);
        assert!(
            review.activities.iter().all(|r| r.candidates.is_empty()),
            "equal numeric amounts in different currencies are not candidates"
        );
        assert_eq!(before_review, f.length());
        ops.match_invoice(
            activity_id,
            invoice_id,
            actor,
            "Inspected synthetic originals",
            &[],
        )
        .unwrap();
        let matched = ops.review(selected()).unwrap();
        assert_eq!(matched.invoices[0].matched_activities, [hex(activity_id)]);
        assert!(matched.invoices[0].purposes.is_empty());
        assert!(matched.activities.iter().all(|r| r.purposes.is_empty()));

        // An independently authored EUR assertion remains a correlated claim,
        // not a reinterpretation of the existing USD assertion.
        let eur = ops
            .add_invoice(
                pdf.clone(),
                "original.pdf",
                "02/09/2026",
                "SYN-CURRENCY",
                "Synthetic supplier",
                "8,05",
                "EUR",
                actor,
                &[],
            )
            .unwrap();
        assert_eq!(first.invoice, eur.invoice);
        let old_total: Inline<Currency<Euro>> =
            amex::euros("8,05").unwrap().try_to_inline().unwrap();
        let historical_shape = entity! {
            metadata::tag: KIND_INVOICE_METADATA, invoice: invoice_id,
            purchase::date: date("2026-09-02").unwrap(),
            purchase::external_number: "SYN-CURRENCY",
            metadata::name: "Synthetic supplier", purchase::gross_total: old_total,
            board::by: actor, metadata::supersedes*: std::iter::empty::<Id>(),
        };
        assert_eq!(
            eur.metadata_claim,
            hex(historical_shape.root().unwrap()),
            "adding USD support must not alter the historical EUR claim fact set or identity"
        );
        let concurrent = ops.review(selected()).unwrap();
        assert_eq!(concurrent.invoices.len(), 2);
        assert!(concurrent
            .invoices
            .iter()
            .any(|r| r.dates == ["2026-09-01"] && r.amounts == ["7.05 USD"]));
        assert!(concurrent
            .invoices
            .iter()
            .any(|r| r.dates == ["2026-09-02"] && r.amounts == ["8.05 EUR"]));
        let corrected = ops
            .add_invoice(
                pdf,
                "original.pdf",
                "03/09/2026",
                "SYN-CURRENCY",
                "Synthetic supplier",
                "-8,05",
                "USD",
                actor,
                &[
                    Id::from_hex(&first.metadata_claim).unwrap(),
                    Id::from_hex(&eur.metadata_claim).unwrap(),
                ],
            )
            .unwrap();
        let current = ops.review(selected()).unwrap();
        assert_eq!(current.invoices.len(), 1);
        assert_eq!(current.invoices[0].dates, ["2026-09-03"]);
        assert_eq!(current.invoices[0].amounts, ["-8.05 USD"]);
        assert_eq!(current.invoices[0].matched_activities, [hex(activity_id)]);
        let corrected_id = Id::from_hex(&corrected.metadata_claim).unwrap();
        let eur_id = Id::from_hex(&eur.metadata_claim).unwrap();
        ops.with_view(false, |_, _, _, facts, _| {
            assert!(exists!(
                pattern!(facts, [{ eur_id @ purchase::gross_total: old_total }])
            ));
            assert!(
                historical_shape.facts().difference(facts).is_empty(),
                "historical EUR facts remain present after explicit supersession"
            );
            Ok(())
        })
        .unwrap();

        // Readers accept additive unknown and multivalued external facts. The
        // writer's one-currency choice is not a closed-world pile constraint.
        let unknown =
            Attribute::<inlineencodings::Handle<blobencodings::UTF8String>>::anchored(genid().id);
        let unrelated = genid();
        let extra_usd: Inline<Currency<UsDollar>> = Amount::<UsDollar>::from_minor(905)
            .unwrap()
            .try_to_inline()
            .unwrap();
        let extra_eur: Inline<Currency<Euro>> = Amount::<Euro>::from_minor(305)
            .unwrap()
            .try_to_inline()
            .unwrap();
        ops.with_view(true, |store, signer, collection, _, _| {
            let change = entity! { ExclusiveId::force_ref(&corrected_id) @
                unknown: "additive external fact", gross_total_usd: extra_usd,
                purchase::gross_total: extra_eur,
            } + entity! { &unrelated @ metadata::tag: KIND_INVOICE_METADATA };
            store.commit(collection, signer, change)?;
            Ok(())
        })
        .unwrap();
        let open = ops.review(selected()).unwrap();
        assert_eq!(open.invoices.len(), 1);
        assert_eq!(
            open.invoices[0].amounts,
            ["-8.05 USD", "3.05 EUR", "9.05 USD"]
        );
        assert!(open.activities.iter().all(|r| r.candidates.is_empty()));
    }

    #[test]
    fn explicit_matching_binary_purpose_conflicts_and_open_world() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let account_id = ops.add_account("Synthetic account", actor).unwrap();
        let input = ops
            .import_amex(
                identical_rows(),
                "activity.csv",
                account_id,
                "EUR",
                selected(),
                actor,
            )
            .unwrap();
        let activity_id = Id::from_hex(&input.activities[0]).unwrap();
        let invoice_added = ops
            .add_invoice(
                Bytes::from(b"%PDF-1.7\nsynthetic invoice evidence".to_vec()),
                "supplier.pdf",
                "01/09/2026",
                "SYN-1",
                "Synthetic supplier",
                "7,05",
                "EUR",
                actor,
                &[],
            )
            .unwrap();
        let invoice_id = Id::from_hex(&invoice_added.invoice).unwrap();
        let review = ops.review(selected()).unwrap();
        let row = review
            .activities
            .iter()
            .find(|r| r.id == input.activities[0])
            .unwrap();
        assert_eq!(row.candidates, [invoice_added.invoice]);
        assert!(row.purposes.is_empty() && row.matches.is_empty());
        let company = ops
            .classify(
                invoice_id,
                COMPANY,
                actor,
                "Explicit synthetic decision",
                &[],
            )
            .unwrap();
        let private = ops
            .classify(
                invoice_id,
                PRIVATE,
                genid().id,
                "Independent contrary decision",
                &[],
            )
            .unwrap();
        assert_eq!(
            ops.review(selected()).unwrap().invoices[0].purposes.len(),
            2
        );
        ops.classify(
            invoice_id,
            COMPANY,
            actor,
            "Reconcile explicitly",
            &[company, private],
        )
        .unwrap();
        let matched = ops
            .match_invoice(activity_id, invoice_id, actor, "Inspected original", &[])
            .unwrap();
        assert!(ops
            .classify(genid().id, COMPANY, actor, "Unknown subject", &[])
            .is_err());
        assert!(ops
            .match_invoice(activity_id, genid().id, actor, "Unknown invoice", &[])
            .is_err());
        assert!(ops
            .classify(invoice_id, COMPANY, actor, "Wrong claim family", &[matched])
            .is_err());
        let after = ops.review(selected()).unwrap();
        assert_eq!(after.invoices[0].purposes.len(), 1);
        let row = after
            .activities
            .iter()
            .find(|r| r.id == input.activities[0])
            .unwrap();
        assert_eq!(row.matches.len(), 1);
        assert!(
            row.purposes.is_empty(),
            "matching never classifies the payment"
        );
        // Open-world annotations and unrelated incomplete records do not make
        // known records disappear or force a catalog validation failure.
        f.storage.with_store(|store, signer, _| {
            let snapshot = store.snapshot()?;
            let collection = open_workspace_write_in(&snapshot, signer.verifying_key(), f.storage.collection_routes().unwrap()[&DEFAULT_SCOPE_ID])?;
            let unrelated = genid();
            let absent: Text = Inline::new([0x55; 32]);
            let change = entity! { &unrelated @ metadata::tag: KIND_ACTIVITY, metadata::description: "Incomplete future shape" }
                + entity! { ExclusiveId::force_ref(&activity_id) @ metadata::description: "Additive external annotation", payment::memo: absent };
            store.commit(collection, signer, change)?;
            Ok(())
        }).unwrap();
        let open = ops.review(selected()).unwrap();
        assert_eq!(open.activities.len(), 2);
        assert!(open
            .activities
            .iter()
            .find(|r| r.id == input.activities[0])
            .unwrap()
            .descriptions
            .iter()
            .any(|v| v.starts_with("[unavailable text attachment")));
        let other_invoice = ops
            .add_invoice(
                Bytes::from(b"%PDF-1.7\nsecond synthetic invoice".to_vec()),
                "other.pdf",
                "01/09/2026",
                "SYN-2",
                "Other synthetic supplier",
                "7,05",
                "EUR",
                actor,
                &[],
            )
            .unwrap();
        let other_invoice_id = Id::from_hex(&other_invoice.invoice).unwrap();
        let contrary = ops
            .match_invoice(
                activity_id,
                other_invoice_id,
                genid().id,
                "Independent match disagreement",
                &[],
            )
            .unwrap();
        let disputed = ops.review(selected()).unwrap();
        assert_eq!(
            disputed
                .activities
                .iter()
                .find(|r| r.id == input.activities[0])
                .unwrap()
                .matches
                .len(),
            2
        );
        ops.match_invoice(
            activity_id,
            invoice_id,
            actor,
            "Reconcile the exact observed match claims",
            &[matched, contrary],
        )
        .unwrap();
        let resolved = ops.review(selected()).unwrap();
        assert_eq!(
            resolved
                .activities
                .iter()
                .find(|r| r.id == input.activities[0])
                .unwrap()
                .matches
                .len(),
            1
        );
    }

    #[test]
    fn invoice_metadata_corrections_do_not_fabricate_cartesian_pairs() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let pdf = Bytes::from(b"%PDF-1.7\nsynthetic correction source".to_vec());
        let first = ops
            .add_invoice(
                pdf.clone(),
                "supplier.pdf",
                "01/09/2026",
                "SYN-1",
                "Synthetic supplier",
                "7,05",
                "EUR",
                actor,
                &[],
            )
            .unwrap();
        let second = ops
            .add_invoice(
                pdf.clone(),
                "supplier.pdf",
                "02/09/2026",
                "SYN-1",
                "Synthetic supplier",
                "8,05",
                "EUR",
                actor,
                &[],
            )
            .unwrap();
        assert_eq!(first.invoice, second.invoice);
        let review = ops.review(selected()).unwrap();
        assert_eq!(
            review.invoices.len(),
            2,
            "two correlated assertions, not four synthetic date/amount pairs"
        );
        assert!(review
            .invoices
            .iter()
            .any(|r| r.dates == ["2026-09-01"] && r.amounts == ["7.05 EUR"]));
        assert!(review
            .invoices
            .iter()
            .any(|r| r.dates == ["2026-09-02"] && r.amounts == ["8.05 EUR"]));
        let predecessors = [
            Id::from_hex(&first.metadata_claim).unwrap(),
            Id::from_hex(&second.metadata_claim).unwrap(),
        ];
        let reconciled = ops
            .add_invoice(
                pdf,
                "supplier.pdf",
                "02/09/2026",
                "SYN-1",
                "Synthetic supplier",
                "8,05",
                "EUR",
                actor,
                &predecessors,
            )
            .unwrap();
        let after = ops.review(selected()).unwrap();
        assert_eq!(after.invoices.len(), 1);
        assert_eq!(after.invoices[0].metadata_claim, reconciled.metadata_claim);
    }

    #[test]
    fn html_original_is_read_only_idempotent_and_metadata_remains_correlated() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let html =
            b"<!DOCTYPE html>\r\n<html><body>Synthetic original &amp; invoice</body></html>\r\n";
        let (file, content) = f.put_file(html, "original.html", "text/html");
        let before_files = f.file_facts();
        let add = |total, supersedes: &[Id]| {
            ops.add_invoice_original(
                InvoiceOriginal::File {
                    file,
                    content,
                    media_type: "text/html",
                },
                "01/09/2026",
                "SYN-HTML",
                "Synthetic supplier",
                total,
                "USD",
                actor,
                supersedes,
            )
            .unwrap()
        };
        let first = add("10,00", &[]);
        let before_retry = f.length();
        let retry = add("10,00", &[]);
        assert_eq!(first.invoice, retry.invoice);
        assert_eq!(first.metadata_claim, retry.metadata_claim);
        assert_eq!(first.file, hex(file));
        assert_eq!(
            before_retry,
            f.length(),
            "exact original retry appends nothing"
        );
        let second = add("11,00", &[]);
        assert_eq!(first.invoice, second.invoice);
        let review = ops.review(selected()).unwrap();
        assert_eq!(review.invoices.len(), 2);
        assert!(review.invoices.iter().all(|i| i.source_files == [hex(file)]
            && i.purposes.is_empty()
            && i.matched_activities.is_empty()));
        assert!(review.invoices.iter().any(|i| i.amounts == ["10.00 USD"]));
        assert!(review.invoices.iter().any(|i| i.amounts == ["11.00 USD"]));
        let reconciled = add(
            "11,00",
            &[
                Id::from_hex(&first.metadata_claim).unwrap(),
                Id::from_hex(&second.metadata_claim).unwrap(),
            ],
        );
        assert_eq!(
            ops.review(selected()).unwrap().invoices[0].metadata_claim,
            reconciled.metadata_claim
        );
        assert_eq!(
            before_files,
            f.file_facts(),
            "referencing Files must not author facts there"
        );
        ops.with_view(false, |_, _, _, _, snapshot| {
            let original: Bytes = BlobStoreGet::get(snapshot, content)?;
            assert_eq!(original.as_ref(), html);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn existing_pdf_reference_preserves_native_invoice_and_claim_identity() {
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let bytes = b"%PDF-1.7\nSynthetic original receipt, not a tax assertion";
        let first = ops
            .add_invoice(
                Bytes::from(bytes.to_vec()),
                "original.pdf",
                "01/09/2026",
                "SYN-PDF",
                "Synthetic supplier",
                "10,00",
                "EUR",
                actor,
                &[],
            )
            .unwrap();
        let file = Id::from_hex(&first.file).unwrap();
        let content = find!(
            content: Content,
            pattern!(&f.file_facts(), [{ file @ faculties::schemas::files::file::content: ?content }])
        )
        .next()
        .expect("test invoice original has content");
        let before = f.length();
        let second = ops
            .add_invoice_original(
                InvoiceOriginal::File {
                    file,
                    content,
                    media_type: "application/pdf",
                },
                "01/09/2026",
                "SYN-PDF",
                "Synthetic supplier",
                "10,00",
                "EUR",
                actor,
                &[],
            )
            .unwrap();
        assert_eq!(first.invoice, second.invoice);
        assert_eq!(first.metadata_claim, second.metadata_claim);
        assert_eq!(before, f.length());
    }

    #[test]
    fn exact_file_selection_is_open_world_and_does_not_derive_file_identity() {
        use faculties::schemas::files::{file as fschema, KIND_FILE};
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let (first_file, first) = f.put_file(
            b"<html>First synthetic original</html>",
            "first.html",
            "text/html",
        );
        let (_, second) = f.put_file(
            b"<html>Second synthetic original</html>",
            "second.html",
            "text/html",
        );
        // Opaque alias, multiple contents and MIME annotations, unknown facts:
        // exact selection is a positive query, never a closed-world validator.
        let file = genid();
        let mut fragment = entity! { &file @ metadata::tag: KIND_FILE,
            fschema::content*: [first, second], metadata::name: "unmodeled annotation",
            fschema::media_type*: faculties::files::media_type_fragment("text/html").unwrap(),
        };
        fragment += entity! { &file @ fschema::media_type*: faculties::files::media_type_fragment("application/pdf").unwrap() };
        f.extend_files(fragment);
        let add = |file, content| {
            ops.add_invoice_original(
                InvoiceOriginal::File {
                    file,
                    content,
                    media_type: "text/html",
                },
                "01/09/2026",
                "SYN-OPEN",
                "Synthetic supplier",
                "10,00",
                "EUR",
                actor,
                &[],
            )
            .unwrap()
        };
        let first_original = add(first_file, first);
        let alias = add(file.id, first);
        let other_content = add(file.id, second);
        assert_eq!(first_original.invoice, alias.invoice);
        assert_eq!(first_original.metadata_claim, alias.metadata_claim);
        assert_ne!(alias.invoice, other_content.invoice);
        assert_eq!(ops.review(selected()).unwrap().invoices.len(), 2);
    }

    #[test]
    fn invalid_original_references_publish_nothing() {
        use faculties::schemas::files::{file as fschema, KIND_FILE};
        let f = Fixture::new();
        let ops = f.ops();
        let actor = genid().id;
        let (html, html_content) = f.put_file(
            b"<html>Synthetic original</html>",
            "original.html",
            "text/html",
        );
        let (pdf, pdf_content) = f.put_file(
            b"%PDF-1.7\nSynthetic original",
            "original.pdf",
            "application/pdf",
        );
        let (wrong, wrong_content) = f.put_file(b"%PDF-1.7\nWrong MIME", "wrong.html", "text/html");
        let (bad_utf8, bad_utf8_content) =
            f.put_file(b"<html>\xff</html>", "bad.html", "text/html");
        let mut dangling = Fragment::empty();
        let missing_content = dangling
            .put::<blobencodings::RawBytes, _>(b"<html>Unavailable original</html>".as_slice());
        let missing = genid();
        f.extend_files(
            entity! { &missing @ metadata::tag: KIND_FILE, fschema::content: missing_content,
                fschema::media_type*: faculties::files::media_type_fragment("text/html").unwrap(),
            },
        );
        let before = f.length();
        for (file, content, media_type) in [
            (genid().id, html_content, "text/html"),
            (html, pdf_content, "text/html"),
            (pdf, pdf_content, "text/html"),
            (html, html_content, "application/pdf"),
            (wrong, wrong_content, "text/html"),
            (bad_utf8, bad_utf8_content, "text/html"),
            (missing.id, missing_content, "text/html"),
            (html, html_content, "text/plain"),
        ] {
            assert!(ops
                .add_invoice_original(
                    InvoiceOriginal::File {
                        file,
                        content,
                        media_type
                    },
                    "01/09/2026",
                    "SYN-BAD",
                    "Synthetic supplier",
                    "10,00",
                    "EUR",
                    actor,
                    &[]
                )
                .is_err());
            assert_eq!(before, f.length());
        }
        assert!(ops
            .add_invoice_original(
                InvoiceOriginal::File {
                    file: html,
                    content: html_content,
                    media_type: "text/html"
                },
                "01/09/2026",
                "SYN-BAD",
                "Synthetic supplier",
                "10,00",
                "EUR",
                actor,
                &[genid().id]
            )
            .is_err());
        let mut routes = f.storage.collection_routes().unwrap().clone();
        routes.remove(&faculties::schemas::files::DEFAULT_SCOPE_ID);
        let unrouted = Bookkeeping::with_storage(f.storage.clone().with_collections(routes));
        assert!(unrouted
            .add_invoice_original(
                InvoiceOriginal::File {
                    file: html,
                    content: html_content,
                    media_type: "text/html"
                },
                "01/09/2026",
                "SYN-BAD",
                "Synthetic supplier",
                "10,00",
                "EUR",
                actor,
                &[]
            )
            .is_err());
        assert_eq!(before, f.length());
        assert!(ops.review(selected()).unwrap().invoices.is_empty());
    }

    #[test]
    fn existing_original_requires_files_read_not_write_permission() {
        let f = Fixture::new();
        let original = faculties::files::stage(
            b"<html>Synthetic read-only original</html>".as_slice(),
            "read-only.html",
            "text/html",
        )
        .unwrap();
        let file = original.root().unwrap();
        let content = find!(
            content: Content,
            pattern!(original.facts(), [{ file @ faculties::schemas::files::file::content: ?content }])
        )
        .next()
        .expect("staged test original has content");
        let files = f
            .storage
            .with_pile(|store, signer| {
                let other = SigningKey::from_bytes(&[31; 32]);
                let policy = CollectionPolicy::new(
                    AdmissionPolicy::direct(signer.verifying_key()),
                    AdmissionPolicy::direct(other.verifying_key()),
                );
                let files = store.collection("synthetic read-only files", policy)?;
                let handle = files.handle();
                store.commit(files, &other, original)?;
                Ok(handle)
            })
            .unwrap();
        let mut routes = f.storage.collection_routes().unwrap().clone();
        routes.insert(faculties::schemas::files::DEFAULT_SCOPE_ID, files);
        let ops = Bookkeeping::with_storage(f.storage.clone().with_collections(routes));
        ops.add_invoice_original(
            InvoiceOriginal::File {
                file,
                content,
                media_type: "text/html",
            },
            "01/09/2026",
            "SYN-READ",
            "Synthetic supplier",
            "10,00",
            "EUR",
            genid().id,
            &[],
        )
        .unwrap();
        let before = f.length();
        assert!(ops
            .add_invoice(
                Bytes::from(b"%PDF-1.7\nSynthetic upload".to_vec()),
                "new.pdf",
                "01/09/2026",
                "SYN-READ",
                "Synthetic supplier",
                "10,00",
                "EUR",
                genid().id,
                &[]
            )
            .is_err());
        assert_eq!(before, f.length());
    }

    #[test]
    fn native_adapter_registers_html_reference_and_keeps_pdf_path_import_working() {
        use faculties::mcp::Faculty;
        use faculties::out::{Out, Part};
        let f = Fixture::new();
        let adapter = mcp::Erp::with_storage(f.storage.clone());
        let original = f._dir.path().join("native.pdf");
        fs::write(&original, b"%PDF-1.7\nSynthetic native PDF").unwrap();
        let (file, content) = f.put_file(
            b"<html>Synthetic native original</html>",
            "native.html",
            "text/html",
        );
        for source in [
            serde_json::json!({"file": hex(file), "content": faculties::files::content_hash_hex(content), "media_type": "text/html"}),
            serde_json::json!({"path": original, "name": "native.pdf"}),
        ] {
            let mut args = serde_json::json!({"date":"01/09/2026", "number":"SYN-NATIVE", "supplier":"Synthetic supplier", "total":"10,00", "currency":"USD", "actor":hex(genid().id)});
            args.as_object_mut()
                .unwrap()
                .extend(source.as_object().unwrap().clone());
            let args = Bytes::from(serde_json::to_vec(&args).unwrap());
            let mut outputs = Vec::new();
            let mut collect = |part| {
                outputs.push(part);
                Ok(())
            };
            let mut out = Out::new(&mut collect);
            adapter
                .call("erp_invoice_add", args.clone(), &mut out)
                .unwrap();
            let before = f.length();
            adapter.call("erp_invoice_add", args, &mut out).unwrap();
            assert_eq!(before, f.length());
            assert_eq!(outputs.len(), 2);
            assert_eq!(outputs[0], outputs[1]);
            let Part::Text { text } = &outputs[0] else {
                panic!("expected JSON text")
            };
            let response: serde_json::Value = serde_json::from_str(text).unwrap();
            assert!(f
                .ops()
                .review(selected())
                .unwrap()
                .invoices
                .iter()
                .any(|i| i.id == response["invoice"]
                    && i.metadata_claim == response["metadata_claim"]));
        }
    }

    #[test]
    fn native_amex_path_and_exact_file_reference_keep_original_identity() {
        use faculties::mcp::Faculty;
        use faculties::out::{Out, Part};
        let f = Fixture::new();
        let actor = genid().id;
        let account_id = f.ops().add_account("Synthetic card", actor).unwrap();
        let original = csv("01/09/2026,Synthetic shop,\"7,05\",,,,,,,synthetic-reference,\n");
        let path = f._dir.path().join("statement.csv");
        fs::write(&path, original.as_ref()).unwrap();
        let (file, _) = f.put_file(original.as_ref(), "statement.csv", "text/csv");
        let adapter = mcp::Erp::with_storage(f.storage.clone());
        let mut responses = Vec::new();
        for source in [serde_json::json!({"path":path}), serde_json::json!({"file_id":hex(file)})] {
            let mut args = serde_json::json!({"name":"statement.csv", "account":hex(account_id),
                "currency":"EUR", "month":"2026-09", "actor":hex(actor)});
            args.as_object_mut().unwrap().extend(source.as_object().unwrap().clone());
            let mut parts = Vec::new();
            adapter.call("erp_amex_import", Bytes::from(serde_json::to_vec(&args).unwrap()),
                &mut Out::new(&mut |part| { parts.push(part); Ok(()) })).unwrap();
            let Part::Text { text } = &parts[0] else { panic!("expected JSON"); };
            responses.push(serde_json::from_str::<serde_json::Value>(text).unwrap());
        }
        assert_eq!(responses[0]["activities"], responses[1]["activities"]);
        assert_eq!(responses[0]["file"], responses[1]["file"]);
        assert_eq!(responses[0]["rows"], 1);
        let folder = f._dir.path().join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("statement.csv"), original.as_ref()).unwrap();
        let files = faculties::files::operations::Files::with_storage(f.storage.clone());
        assert_eq!(files.get(responses[0]["file"].as_str().unwrap()).unwrap().bytes.as_ref(), original.as_ref(),
            "the budgeted adapter preserves the exact original CSV bytes");
        let mut imported = Vec::new();
        files.add_path(&folder, None, &[], false,
            &mut Out::new(&mut |part| { imported.push(part); Ok(()) })).unwrap();
        let folder_id = imported.iter().find_map(|part| match part {
            Part::Text { text } => text.lines().find_map(|line| line.strip_prefix("Import: ").map(str::to_owned)),
            _ => None,
        }).expect("folder import reference");
        let before = fs::read(&f.path).unwrap();
        let args = serde_json::json!({"file_id":folder_id, "name":"statement.csv", "account":hex(account_id),
            "currency":"EUR", "month":"2026-09", "actor":hex(actor)});
        let error = adapter.call("erp_amex_import", Bytes::from(serde_json::to_vec(&args).unwrap()),
            &mut Out::new(&mut |_| Ok(()))).unwrap_err();
        assert!(error.downcast_ref::<faculties::mcp::input::NotAFile>().is_some(), "{error:#}");
        assert_eq!(fs::read(&f.path).unwrap(), before, "a folder reference never flattens or imports an activity");
    }

    #[test]
    fn native_erp_path_inputs_refuse_the_call_budget_before_publication() {
        use faculties::mcp::Faculty;
        use faculties::out::Out;
        let f = Fixture::new();
        let actor = genid().id;
        let account_id = f.ops().add_account("Synthetic card", actor).unwrap();
        let path = f._dir.path().join("oversize.bin");
        // Sparse metadata-only fixture: the helper must refuse before reading.
        fs::File::create(&path).unwrap()
            .set_len(faculties::mcp::input::MAX_CALL_INPUT_BYTES + 1).unwrap();
        let adapter = mcp::Erp::with_storage(f.storage.clone());
        let before = fs::read(&f.path).unwrap();
        for (tool, args) in [
            ("erp_amex_import", serde_json::json!({"path":path, "name":"statement.csv",
                "account":hex(account_id), "currency":"EUR", "month":"2026-09", "actor":hex(actor)})),
            ("erp_invoice_add", serde_json::json!({"path":path, "name":"invoice.pdf",
                "date":"01/09/2026", "number":"SYN-BUDGET", "supplier":"Synthetic supplier",
                "total":"10,00", "currency":"EUR", "actor":hex(actor)})),
        ] {
            let mut outputs = 0;
            let error = adapter.call(tool, Bytes::from(serde_json::to_vec(&args).unwrap()),
                &mut Out::new(&mut |_| { outputs += 1; Ok(()) })).unwrap_err();
            assert!(error.downcast_ref::<faculties::mcp::InvalidArguments>().is_some(), "{error:#}");
            assert!(error.to_string().contains("input budget of one call"), "{error:#}");
            assert_eq!(outputs, 0);
            assert_eq!(fs::read(&f.path).unwrap(), before, "budget refusal publishes no original or activity");
        }
    }

    #[test]
    fn html_format_guard_preserves_bytes_and_rejects_mislabeled_data() {
        for valid in [
            "<html></html>",
            "<!DOCTYPE html><html></html>",
            "\u{feff}\r\n<!-- supplier comment -->\n<HTML lang=\"en\"></HTML>",
        ] {
            assert!(check_original_format(valid.as_bytes(), "text/html").is_ok());
        }
        for invalid in [
            "",
            "not html",
            "<htmlx>wrong</htmlx>",
            "<!doctype htmlx>",
            "<!-- unfinished",
            "%PDF-1.7",
            "<html>\0</html>",
        ] {
            assert!(check_original_format(invalid.as_bytes(), "text/html").is_err());
        }
    }
}
