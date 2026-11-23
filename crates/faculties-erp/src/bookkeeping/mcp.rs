//! Private native MCP adapter. Explicit host paths or exact Files references,
//! no encoded payload arguments. No subprocesses, credential
//! arguments, ambient persona or independent backend owner.

use super::{original_content, schema, AccountMapping, Bookkeeping, InvoiceOriginal, Migration};
use crate::amex::Month;
use anybytes::Bytes;
use anyhow::{bail, Result};
use faculties::files::operations::Files;
use faculties::mcp::input::InputBudget;
use faculties::mcp::{decode_arguments, invalid_arguments, Faculty, Tool};
use faculties::out::Out;
use faculties::storage::Storage;
use serde::Deserialize;
use triblespace::prelude::Id;

pub struct Erp {
    operations: Bookkeeping,
    files: Files,
}
impl Erp {
    /// Same owner and immutable explicit routes as the host's other faculties.
    /// Tool discovery does not open a store, read a key, or inspect environment.
    pub fn with_storage(storage: Storage) -> Self {
        Self {
            files: Files::with_storage(storage.clone()),
            operations: Bookkeeping::with_storage(storage),
        }
    }
}

const TOOLS: &[Tool] = &[
    Tool { name: "erp_account_add", description: "Create an opaque card/source account. Supply a display label and the actor ID. Account identity is independent of the label. Explicit actor is a persona entity id.", input_schema: r#"{"type":"object","properties":{"label":{"type":"string"},"actor":{"type":"string"}},"required":["label","actor"],"additionalProperties":false}"# },
    Tool { name: "erp_amex_import", description: "Read original CSV bytes from exactly one file_id or absolute host path; folders are refused. Import German Amex activity originals and provider records identified by opaque card account plus nonblank Betreff. This is a provider identity convention, not guaranteed economic equivalence. Conflicting payloads remain correlated observation alternatives, never summed or chosen. 11 columns require account; 13 columns require exact Karteninhaber/Konto # mappings (never last4 inference). EUR only. Blank references refuse the import before publication. No purpose inference or implicit legacy migration.", input_schema: r#"{"type":"object","properties":{"name":{"type":"string"},"account":{"type":"string"},"mappings":{"type":"array","minItems":1,"items":{"type":"object","properties":{"cardholder":{"type":"string"},"card_number":{"type":"string"},"account":{"type":"string"}},"required":["cardholder","card_number","account"],"additionalProperties":false}},"currency":{"type":"string","enum":["EUR"]},"month":{"type":"string","description":"YYYY-MM"},"actor":{"type":"string"},"path":{"type":"string","description":"Absolute path of the original regular file on the host the MCP server runs on, read with the server process's permissions. Pass only a source authorized by the user's task."},"file_id":{"type":"string","pattern":"^[0-9a-fA-F]{32}$","description":"Exact Files entity or single-file import ID in this workspace; not a content hash or directory."}},"required":["name","currency","month","actor"],"additionalProperties":false,"allOf":[{"oneOf":[{"required":["account"],"not":{"required":["mappings"]}},{"required":["mappings"],"not":{"required":["account"]}}]},{"oneOf":[{"required":["file_id"],"not":{"required":["path"]}},{"required":["path"],"not":{"required":["file_id"]}}]}]}"# },
    Tool { name: "erp_amex_migrate", description: "Explicitly project legacy Amex activity decisions onto provider records. Each selection asserts a specific legacy ID/account/original content/logical row correlation; no cross-product inference. Default dry run returns exact target/evidence/projection IDs without writes. apply=true atomically publishes the selected batch in ERP, preserving all original claims and Files. Projection actor is the migration actor, original decision authors and conflicting/superseded claims remain distinguishable. Account membership never classifies purpose.", input_schema: r#"{"type":"object","properties":{"selections":{"type":"array","items":{"type":"object","properties":{"legacy":{"type":"string"},"content":{"type":"string"},"row":{"type":"integer","minimum":1},"account":{"type":"string"}},"required":["legacy","content","row","account"],"additionalProperties":false}},"month":{"type":"string"},"actor":{"type":"string"},"apply":{"type":"boolean","default":false}},"required":["selections","month","actor"],"additionalProperties":false}"# },
    Tool { name: "erp_invoice_add", description: "Register supplier original evidence and an immutable attributed metadata assertion. Supply either path+name (original PDF bytes from an absolute path on the server host, read with its permissions and only as authorized by the task) OR file+content+media_type (exact existing Files original PDF or UTF-8 HTML, read without rewriting). Inspect the original before supplying metadata; PDF/HTML format does not establish a tax invoice, supplier authenticity or purpose. No printing or conversion. Date DD/MM/YYYY, total exact signed decimal comma with two fractional digits, explicit original EUR or USD. Supersedes names known metadata_claim ids of this same original; concurrent assertions remain visible. Does not extract or infer tax, purpose, FX or payment. Foreign totals are not EUR totals.", input_schema: r#"{"type":"object","properties":{"name":{"type":"string"},"file":{"type":"string","description":"Exact full 32-hex Files entity id in the routed Files collection"},"content":{"type":"string","description":"Exact full 64-hex RawBytes content handle from Files; optional blake3: prefix"},"media_type":{"type":"string","enum":["application/pdf","text/html"]},"date":{"type":"string"},"number":{"type":"string"},"supplier":{"type":"string"},"total":{"type":"string"},"currency":{"type":"string","enum":["EUR","USD"]},"actor":{"type":"string"},"supersedes":{"type":"array","items":{"type":"string"},"default":[]},"path":{"type":"string","description":"Absolute path of the original regular file on the host the MCP server runs on, read with the server process's permissions. Pass only a source authorized by the user's task."}},"required":["date","number","supplier","total","currency","actor"],"oneOf":[{"required":["path","name"],"not":{"anyOf":[{"required":["file"]},{"required":["content"]},{"required":["media_type"]}]}},{"required":["file","content","media_type"],"not":{"anyOf":[{"required":["path"]},{"required":["name"]}]}}],"additionalProperties":false}"# },
    Tool { name: "erp_review", description: "Read selected-month transactions once each, with correlated semantic observation alternatives and original source evidence; unmigrated legacy rows are marked legacy. When any observation falls in the month all its alternatives are shown, never selected or summed. Claim values/authors are sets; migration provenance and original decisions are separate. Late mapped legacy decisions remain visible without writes. Invoice candidates match an observed signed EUR amount, not a judgment of identity or purpose; equal USD is not a candidate. Empty purposes means unresolved. Review performs no writes.", input_schema: r#"{"type":"object","properties":{"month":{"type":"string","description":"YYYY-MM"}},"required":["month"],"additionalProperties":false}"# },
    Tool { name: "erp_classify", description: "Assert whole-company or whole-private purpose of an activity OR invoice, with explicit actor and reason. No split expense and no inference from payment. Opposing claims remain visible unless explicitly superseded; supersedes names only this subject's known purpose claims.", input_schema: r#"{"type":"object","properties":{"subject":{"type":"string"},"purpose":{"type":"string","enum":["company","private"]},"actor":{"type":"string"},"note":{"type":"string"},"supersedes":{"type":"array","items":{"type":"string"},"default":[]}},"required":["subject","purpose","actor","note"],"additionalProperties":false}"# },
    Tool { name: "erp_match", description: "Explicitly associate one card activity with an invoice, with actor and reason. This is documentary matching, not allocation, tax, expense purpose, or settlement. Conflicting matches stay visible unless their known claim ids are explicitly superseded.", input_schema: r#"{"type":"object","properties":{"activity":{"type":"string"},"invoice":{"type":"string"},"actor":{"type":"string"},"note":{"type":"string"},"supersedes":{"type":"array","items":{"type":"string"},"default":[]}},"required":["activity","invoice","actor","note"],"additionalProperties":false}"# },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Account {
    label: String,
    actor: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    #[serde(default, deserialize_with = "present_string")]
    file_id: Option<String>,
    #[serde(default, deserialize_with = "present_string")]
    path: Option<String>,
    name: String,
    #[serde(default, deserialize_with = "present_string")]
    account: Option<String>,
    #[serde(default, deserialize_with = "present_mappings")]
    mappings: Option<Vec<AccountMapping>>,
    currency: String,
    month: String,
    actor: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Migrate {
    selections: Vec<Migration>,
    month: String,
    actor: String,
    #[serde(default)]
    apply: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invoice {
    #[serde(default, deserialize_with = "present_string")]
    path: Option<String>,
    #[serde(default, deserialize_with = "present_string")]
    name: Option<String>,
    #[serde(default, deserialize_with = "present_string")]
    file: Option<String>,
    #[serde(default, deserialize_with = "present_string")]
    content: Option<String>,
    #[serde(default, deserialize_with = "present_string")]
    media_type: Option<String>,
    date: String,
    number: String,
    supplier: String,
    total: String,
    currency: String,
    actor: String,
    #[serde(default)]
    supersedes: Vec<String>,
}

// Missing source fields are allowed; an explicitly supplied null is not a
// string and must not turn a mixed/ambiguous input into a different variant.
fn present_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}
fn present_mappings<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Vec<AccountMapping>>, D::Error> {
    Vec::<AccountMapping>::deserialize(d).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    month: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Classify {
    subject: String,
    purpose: String,
    actor: String,
    note: String,
    #[serde(default)]
    supersedes: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Match {
    activity: String,
    invoice: String,
    actor: String,
    note: String,
    #[serde(default)]
    supersedes: Vec<String>,
}

fn id(value: &str) -> Result<Id> {
    Id::from_hex(value)
        .ok_or_else(|| invalid_arguments("id must be a full nonzero 32-hex entity id"))
}
fn ids(values: &[String]) -> Result<Vec<Id>> {
    values.iter().map(|v| id(v)).collect()
}
fn month(value: &str) -> Result<Month> {
    Month::parse(value).map_err(invalid_arguments)
}
fn emit(out: &mut Out<'_>, value: impl serde::Serialize) -> Result<()> {
    out.text(serde_json::to_string(&value)?)
}

impl Faculty for Erp {
    fn tools(&self) -> &[Tool] {
        TOOLS
    }
    fn call(&self, name: &str, arguments: Bytes, out: &mut Out<'_>) -> Result<()> {
        match name {
            "erp_account_add" => {
                let a: Account = decode_arguments(arguments)?;
                let account = self.operations.add_account(&a.label, id(&a.actor)?)?;
                emit(out, serde_json::json!({"account":format!("{account:X}")}))
            }
            "erp_amex_import" => {
                let a: Import = decode_arguments(arguments)?;
                if a.account.is_some() == a.mappings.is_some() {
                    return Err(invalid_arguments("supply account or mappings, not both"));
                }
                let account = a.account.as_deref().map(id).transpose()?;
                let actor = id(&a.actor)?;
                let selected = month(&a.month)?;
                let data = InputBudget::per_call().reference(&self.files, a.file_id.as_deref(), a.path.as_deref())?;
                emit(
                    out,
                    self.operations.import_amex_mapped(
                        data,
                        &a.name,
                        account,
                        a.mappings.as_deref().unwrap_or(&[]),
                        &a.currency,
                        selected,
                        actor,
                    )?,
                )
            }
            "erp_amex_migrate" => {
                let a: Migrate = decode_arguments(arguments)?;
                emit(
                    out,
                    self.operations.migrate_amex(
                        &a.selections,
                        month(&a.month)?,
                        id(&a.actor)?,
                        a.apply,
                    )?,
                )
            }
            "erp_invoice_add" => {
                let a: Invoice = decode_arguments(arguments)?;
                let original = match (&a.path, &a.name, &a.file, &a.content, &a.media_type) {
                    (Some(path), Some(name), None, None, None) => InvoiceOriginal::Pdf {
                        bytes: InputBudget::per_call().host_file(path)?,
                        name,
                    },
                    (None, None, Some(file), Some(content), Some(media_type)) => {
                        InvoiceOriginal::File {
                            file: id(file)?,
                            content: original_content(content).map_err(invalid_arguments)?,
                            media_type,
                        }
                    }
                    _ => {
                        return Err(invalid_arguments(
                            "supply exactly path+name or file+content+media_type",
                        ))
                    }
                };
                let actor = id(&a.actor)?;
                emit(
                    out,
                    self.operations.add_invoice_original(
                        original,
                        &a.date,
                        &a.number,
                        &a.supplier,
                        &a.total,
                        &a.currency,
                        actor,
                        &ids(&a.supersedes)?,
                    )?,
                )
            }
            "erp_review" => {
                let a: Review = decode_arguments(arguments)?;
                emit(out, self.operations.review(month(&a.month)?)?)
            }
            "erp_classify" => {
                let a: Classify = decode_arguments(arguments)?;
                let value = match a.purpose.as_str() {
                    "company" => schema::COMPANY,
                    "private" => schema::PRIVATE,
                    _ => return Err(invalid_arguments("purpose must be company or private")),
                };
                let claim = self.operations.classify(
                    id(&a.subject)?,
                    value,
                    id(&a.actor)?,
                    &a.note,
                    &ids(&a.supersedes)?,
                )?;
                emit(out, serde_json::json!({"claim":format!("{claim:X}")}))
            }
            "erp_match" => {
                let a: Match = decode_arguments(arguments)?;
                let claim = self.operations.match_invoice(
                    id(&a.activity)?,
                    id(&a.invoice)?,
                    id(&a.actor)?,
                    &a.note,
                    &ids(&a.supersedes)?,
                )?;
                emit(out, serde_json::json!({"claim":format!("{claim:X}")}))
            }
            _ => bail!("unknown ERP tool {name:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use triblespace::macros::{find, pattern};
    #[test]
    fn amex_routing_and_migration_arguments_are_explicit() {
        let base = serde_json::json!({"path":"/synthetic.csv", "name":"synthetic.csv", "currency":"EUR", "month":"2026-09", "actor":"unused"});
        let mut retired = base.clone();
        retired["data"] = serde_json::json!("opaque payload");
        assert!(serde_json::from_value::<Import>(retired).is_err());
        let mut single = base.clone();
        single["account"] = serde_json::json!("opaque");
        assert!(serde_json::from_value::<Import>(single.clone())
            .unwrap()
            .account
            .is_some());
        single["mappings"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Import>(single).is_err());
        let mut mapped = base;
        mapped["mappings"] = serde_json::json!([{"cardholder":"Synthetic", "card_number":"-0000", "account":"opaque"}]);
        assert_eq!(
            serde_json::from_value::<Import>(mapped.clone())
                .unwrap()
                .mappings
                .unwrap()
                .len(),
            1
        );
        mapped["mappings"][0]["last4"] = serde_json::json!("0000");
        assert!(serde_json::from_value::<Import>(mapped).is_err());
        let migration: Migrate = serde_json::from_value(
            serde_json::json!({"selections":[], "month":"2026-09", "actor":"unused"}),
        )
        .unwrap();
        assert!(!migration.apply);
        for tool in TOOLS {
            let schema: serde_json::Value = serde_json::from_str(tool.input_schema).unwrap();
            assert_eq!(schema["additionalProperties"], false);
            assert!(schema["properties"].get("data").is_none(), "no raw binary input: {}", tool.name);
        }
    }
    #[test]
    fn discovery_and_rejected_arguments_have_no_io() {
        let adapter = Erp::with_storage(
            Storage::shared_local("/not/a/pile".into(), Some("/not/a/key".into()))
                .with_collections(Default::default()),
        );
        assert_eq!(adapter.tools().len(), 7);
        let mut discard = |_| Ok(());
        let mut out = Out::new(&mut discard);
        let error = adapter
            .call(
                "erp_review",
                Bytes::from(br#"{"month":"2026-09"}"#.to_vec()),
                &mut out,
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("ERP collection role is unconfigured"));
        for args in [
            br#"{"month":"2026-09","unexpected":true}"#.as_slice(),
            br#"{"month":"2026-09","month":"2026-08"}"#.as_slice(),
            b"[]",
        ] {
            assert!(adapter
                .call("erp_review", Bytes::from(args.to_vec()), &mut out)
                .unwrap_err()
                .downcast_ref::<faculties::mcp::InvalidArguments>()
                .is_some());
        }
    }

    #[test]
    fn invoice_dispatch_validates_currency_and_amount_before_storage() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("synthetic.pdf");
        std::fs::write(&original, b"%PDF-1.7\nsynthetic adapter fixture").unwrap();
        let adapter = Erp::with_storage(
            Storage::shared_local("/not/a/pile".into(), Some("/not/a/key".into()))
                .with_collections(Default::default()),
        );
        let mut discard = |_| Ok(());
        let mut out = Out::new(&mut discard);
        for (currency, total, expected) in [
            ("EUR", "10,00", "ERP collection role is unconfigured"),
            ("USD", "10,00", "ERP collection role is unconfigured"),
            (
                "GBP",
                "10,00",
                "explicit invoice currency must be EUR or USD",
            ),
            ("USD", "10.00", "amount requires decimal comma"),
            ("USD", "10,0", "invalid exact amount"),
        ] {
            let arguments = serde_json::json!({
                "path": original,
                "name": "synthetic.pdf", "date": "01/09/2026", "number": "SYN-1",
                "supplier": "Synthetic supplier", "total": total, "currency": currency,
                "actor": format!("{:X}", triblespace::prelude::genid().id),
            });
            let error = adapter
                .call(
                    "erp_invoice_add",
                    Bytes::from(serde_json::to_vec(&arguments).unwrap()),
                    &mut out,
                )
                .unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "{currency} {total}: {error:#}"
            );
        }
        let amex: serde_json::Value = serde_json::from_str(
            adapter
                .tools()
                .iter()
                .find(|t| t.name == "erp_amex_import")
                .unwrap()
                .input_schema,
        )
        .unwrap();
        assert_eq!(
            amex["properties"]["currency"]["enum"],
            serde_json::json!(["EUR"])
        );
    }

    #[test]
    fn invoice_source_selection_is_explicit_and_rejected_before_storage() {
        let adapter = Erp::with_storage(
            Storage::shared_local("/not/a/pile".into(), Some("/not/a/key".into()))
                .with_collections(Default::default()),
        );
        let mut discard = |_| Ok(());
        let mut out = Out::new(&mut discard);
        let file = format!("{:X}", triblespace::prelude::genid().id);
        let fragment = faculties::files::stage(
            b"<html>Synthetic adapter original</html>".as_slice(),
            "original.html",
            "text/html",
        )
        .unwrap();
        let original_file = fragment.root().unwrap();
        let content = faculties::files::content_hash_hex(
            find!(
                content: faculties::files::ContentHandle,
                pattern!(fragment.facts(), [{ original_file @ faculties::schemas::files::file::content: ?content }])
            )
            .next()
            .expect("staged test original has content"),
        );
        let base = serde_json::json!({
            "date": "01/09/2026", "number": "SYN-HTML", "supplier": "Synthetic supplier",
            "total": "10,00", "currency": "USD", "actor": file,
        });
        for (source, valid) in [
            (
                serde_json::json!({"file":file,"content":content,"media_type":"text/html"}),
                true,
            ),
            (
                serde_json::json!({"file":file,"content":format!("blake3:{content}"),"media_type":"application/pdf"}),
                true,
            ),
            (serde_json::json!({"file":file}), false),
            (
                serde_json::json!({"content":content,"media_type":"text/html"}),
                false,
            ),
            (
                serde_json::json!({"file":file,"content":content,"media_type":"text/html","name":"original.html"}),
                false,
            ),
            (
                serde_json::json!({"file":file,"content":content,"media_type":"text/html","path":null}),
                false,
            ),
            (
                serde_json::json!({"file":file,"content":"abc","media_type":"text/html"}),
                false,
            ),
            (
                serde_json::json!({"file":"abc","content":content,"media_type":"text/html"}),
                false,
            ),
            (
                serde_json::json!({"path":"/original.pdf","name":"original.pdf","file":file}),
                false,
            ),
            (serde_json::json!({}), false),
        ] {
            let mut arguments = base.clone();
            arguments
                .as_object_mut()
                .unwrap()
                .extend(source.as_object().unwrap().clone());
            let error = adapter
                .call(
                    "erp_invoice_add",
                    Bytes::from(serde_json::to_vec(&arguments).unwrap()),
                    &mut out,
                )
                .unwrap_err();
            if valid {
                assert!(
                    error
                        .to_string()
                        .contains("ERP collection role is unconfigured"),
                    "{error:#}"
                );
            } else {
                assert!(
                    error
                        .downcast_ref::<faculties::mcp::InvalidArguments>()
                        .is_some(),
                    "{error:#}"
                );
            }
        }
    }
}
