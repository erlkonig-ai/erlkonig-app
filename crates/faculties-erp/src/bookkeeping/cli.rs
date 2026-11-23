//! Tailored CLI over the same native operations. Paths are a CLI concern only.

use super::{
    original_content, schema, AccountMapping, Bookkeeping, InvoiceOriginal, Migration,
    DEFAULT_SCOPE_ID,
};
use crate::amex::Month;
use anybytes::Bytes;
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use faculties::storage::Storage;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use triblespace::core::collection::CollectionHandle;
use triblespace::prelude::*;

#[derive(Args)]
pub struct Cli {
    /// Exact existing Files root descriptor (required by source imports).
    #[arg(long)]
    files_collection: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Create an opaque account; its display label does not determine identity.
    AccountAdd {
        label: String,
        #[arg(long)]
        actor: String,
    },
    /// Preserve original CSV and import selected-month provider records.
    AmexImport {
        path: PathBuf,
        #[arg(
            long,
            required_unless_present = "mappings",
            conflicts_with = "mappings"
        )]
        account: Option<String>,
        /// JSON array of exact cardholder/card_number/account mappings (13 columns).
        #[arg(long, required_unless_present = "account")]
        mappings: Option<String>,
        #[arg(long)]
        currency: String,
        #[arg(long)]
        month: String,
        #[arg(long)]
        actor: String,
    },
    /// Preflight legacy decision migration; --apply commits the complete batch.
    AmexMigrate {
        /// JSON array of legacy/content/row/account evidence selections.
        #[arg(long)]
        selections: String,
        #[arg(long)]
        month: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        apply: bool,
    },
    /// Register explicit metadata for original PDF bytes or an existing PDF/HTML in Files.
    InvoiceAdd {
        #[arg(required_unless_present = "file", conflicts_with_all = ["file", "content", "media_type"])]
        path: Option<PathBuf>,
        /// Full existing Files entity id; the original is read, never rewritten.
        #[arg(long, requires_all = ["content", "media_type"])]
        file: Option<String>,
        /// Full RawBytes content handle selected from this Files entity.
        #[arg(long, requires = "file")]
        content: Option<String>,
        #[arg(long, requires = "file", value_parser = ["application/pdf", "text/html"])]
        media_type: Option<String>,
        #[arg(long)]
        date: String,
        #[arg(long)]
        number: String,
        #[arg(long)]
        supplier: String,
        /// Exact signed decimal comma, with two fractional digits.
        #[arg(long)]
        total: String,
        /// Original document denomination; no conversion is performed.
        #[arg(long, value_parser = ["EUR", "USD"])]
        currency: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        supersedes: Vec<String>,
    },
    /// Read accounts, activity, invoices, claims and candidate matches without writes.
    Review {
        #[arg(long)]
        month: String,
    },
    /// Assert whole-company or whole-private purpose, with an explicit reason.
    Classify {
        subject: String,
        #[arg(long, value_parser = ["company", "private"])]
        purpose: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        note: String,
        #[arg(long)]
        supersedes: Vec<String>,
    },
    /// Associate activity and invoice. Does not allocate or classify an expense.
    Match {
        activity: String,
        invoice: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        note: String,
        #[arg(long)]
        supersedes: Vec<String>,
    },
}
fn id(value: &str) -> Result<Id> {
    Id::from_hex(value).context("expected full nonzero 32-hex entity id")
}
fn ids(values: &[String]) -> Result<Vec<Id>> {
    values.iter().map(|s| id(s)).collect()
}
fn handle(value: &str) -> Result<CollectionHandle> {
    let text = value.strip_prefix("blake3:").unwrap_or(value);
    ensure!(
        text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected exact 64-hex collection handle"
    );
    let mut bytes = [0; 32];
    for (i, target) in bytes.iter_mut().enumerate() {
        *target = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)?;
    }
    Ok(Inline::new(bytes))
}
fn original(path: &Path) -> Result<(Bytes, String)> {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .context("original must have a UTF-8 leaf name")?
        .to_owned();
    Ok((
        Bytes::from(std::fs::read(path).context("read original file")?),
        name,
    ))
}
pub fn run(cli: Cli, pile: PathBuf, key: Option<PathBuf>, collection: CollectionHandle) -> Result<()> {
    let mut routes = BTreeMap::from([(DEFAULT_SCOPE_ID, collection)]);
    if let Some(files) = cli.files_collection {
        routes.insert(faculties::schemas::files::DEFAULT_SCOPE_ID, handle(&files)?);
    }
    let storage = Storage::shared_local(pile, key).with_collections(routes);
    let ops = Bookkeeping::with_storage(storage.clone());
    let result = (|| {
        match cli.command {
            Command::AccountAdd { label, actor } => {
                let account = ops.add_account(&label, id(&actor)?)?;
                println!("{}", serde_json::json!({"account":format!("{account:X}")}));
            }
            Command::AmexImport {
                path,
                account,
                mappings,
                currency,
                month,
                actor,
            } => {
                let (bytes, name) = original(&path)?;
                let mappings: Vec<AccountMapping> = mappings
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()?
                    .unwrap_or_default();
                let result = ops.import_amex_mapped(
                    bytes,
                    &name,
                    account.as_deref().map(id).transpose()?,
                    &mappings,
                    &currency,
                    Month::parse(&month)?,
                    id(&actor)?,
                )?;
                println!("{}", serde_json::to_string(&result)?);
            }
            Command::AmexMigrate {
                selections,
                month,
                actor,
                apply,
            } => {
                let selections: Vec<Migration> = serde_json::from_str(&selections)?;
                println!(
                    "{}",
                    serde_json::to_string(&ops.migrate_amex(
                        &selections,
                        Month::parse(&month)?,
                        id(&actor)?,
                        apply
                    )?)?
                );
            }
            Command::InvoiceAdd {
                path,
                file,
                content,
                media_type,
                date,
                number,
                supplier,
                total,
                currency,
                actor,
                supersedes,
            } => {
                let source = path.as_deref().map(original).transpose()?;
                let original = match (&source, &file, &content, &media_type) {
                    (Some((bytes, name)), None, None, None) => InvoiceOriginal::Pdf {
                        bytes: bytes.clone(),
                        name,
                    },
                    (None, Some(file), Some(content), Some(media_type)) => InvoiceOriginal::File {
                        file: id(file)?,
                        content: original_content(content)?,
                        media_type,
                    },
                    _ => {
                        anyhow::bail!("supply exactly a PDF path or --file/--content/--media-type")
                    }
                };
                println!(
                    "{}",
                    serde_json::to_string(&ops.add_invoice_original(
                        original,
                        &date,
                        &number,
                        &supplier,
                        &total,
                        &currency,
                        id(&actor)?,
                        &ids(&supersedes)?
                    )?)?
                );
            }
            Command::Review { month } => println!(
                "{}",
                serde_json::to_string_pretty(&ops.review(Month::parse(&month)?)?)?
            ),
            Command::Classify {
                subject,
                purpose,
                actor,
                note,
                supersedes,
            } => {
                let purpose_id = if purpose == "company" {
                    schema::COMPANY
                } else {
                    schema::PRIVATE
                };
                let claim = ops.classify(
                    id(&subject)?,
                    purpose_id,
                    id(&actor)?,
                    &note,
                    &ids(&supersedes)?,
                )?;
                println!("{}", serde_json::json!({"claim":format!("{claim:X}")}));
            }
            Command::Match {
                activity,
                invoice,
                actor,
                note,
                supersedes,
            } => {
                let claim = ops.match_invoice(
                    id(&activity)?,
                    id(&invoice)?,
                    id(&actor)?,
                    &note,
                    &ids(&supersedes)?,
                )?;
                println!("{}", serde_json::json!({"claim":format!("{claim:X}")}));
            }
        }
        Ok(())
    })();
    storage.finish(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use faculties::mcp::Faculty;

    #[derive(Parser)]
    struct ParseOnly {
        #[command(flatten)]
        cli: Cli,
    }

    #[test]
    fn amex_cli_requires_explicit_account_mode_and_migration_is_dry_by_default() {
        let base = [
            "erp-test",
            "amex-import",
            "synthetic.csv",
            "--currency",
            "EUR",
            "--month",
            "2026-09",
            "--actor",
            "unused",
        ];
        assert!(ParseOnly::try_parse_from(base).is_err());
        assert!(ParseOnly::try_parse_from(base.into_iter().chain(["--account", "unused"])).is_ok());
        assert!(ParseOnly::try_parse_from(base.into_iter().chain(["--mappings", "[]"])).is_ok());
        assert!(ParseOnly::try_parse_from(base.into_iter().chain([
            "--account",
            "unused",
            "--mappings",
            "[]"
        ]))
        .is_err());
        let parsed = ParseOnly::try_parse_from([
            "erp-test",
            "amex-migrate",
            "--selections",
            "[]",
            "--month",
            "2026-09",
            "--actor",
            "unused",
        ])
        .unwrap();
        assert!(matches!(
            parsed.cli.command,
            Command::AmexMigrate { apply: false, .. }
        ));
    }

    #[test]
    fn explicit_invoice_currencies_match_the_actual_mcp_schema() {
        let adapter = super::super::mcp::Erp::with_storage(
            Storage::shared_local("/not/a/pile".into(), Some("/not/a/key".into()))
                .with_collections(Default::default()),
        );
        let tool = adapter
            .tools()
            .iter()
            .find(|t| t.name == "erp_invoice_add")
            .unwrap();
        let schema: serde_json::Value = serde_json::from_str(tool.input_schema).unwrap();
        let currencies = schema["properties"]["currency"]["enum"].as_array().unwrap();
        assert_eq!(
            currencies,
            &[serde_json::json!("EUR"), serde_json::json!("USD")]
        );
        for currency in ["EUR", "USD", "GBP", "eur", ""] {
            let args = [
                "erp-test",
                "invoice-add",
                "synthetic.pdf",
                "--date",
                "01/09/2026",
                "--number",
                "SYN-1",
                "--supplier",
                "Synthetic supplier",
                "--total",
                "10,00",
                "--currency",
                currency,
                "--actor",
                "unused-during-parsing",
            ];
            assert_eq!(
                ParseOnly::try_parse_from(args).is_ok(),
                currencies.contains(&serde_json::json!(currency)),
                "CLI/MCP parity for {currency:?}"
            );
        }
    }

    #[test]
    fn invoice_cli_accepts_exactly_one_complete_original_source() {
        let base = [
            "erp-test",
            "invoice-add",
            "--date",
            "01/09/2026",
            "--number",
            "SYN-1",
            "--supplier",
            "Synthetic supplier",
            "--total",
            "10,00",
            "--currency",
            "USD",
            "--actor",
            "unused",
        ];
        for (source, valid) in [
            (vec!["original.pdf"], true),
            (
                vec![
                    "--file",
                    "unused",
                    "--content",
                    "unused",
                    "--media-type",
                    "text/html",
                ],
                true,
            ),
            (
                vec![
                    "--file",
                    "unused",
                    "--content",
                    "unused",
                    "--media-type",
                    "application/pdf",
                ],
                true,
            ),
            (vec!["--file", "unused"], false),
            (
                vec!["--content", "unused", "--media-type", "text/html"],
                false,
            ),
            (
                vec![
                    "original.pdf",
                    "--file",
                    "unused",
                    "--content",
                    "unused",
                    "--media-type",
                    "text/html",
                ],
                false,
            ),
            (
                vec![
                    "--file",
                    "unused",
                    "--content",
                    "unused",
                    "--media-type",
                    "text/plain",
                ],
                false,
            ),
            (vec![], false),
        ] {
            assert_eq!(
                ParseOnly::try_parse_from(base.iter().copied().chain(source)).is_ok(),
                valid
            );
        }
    }
}
