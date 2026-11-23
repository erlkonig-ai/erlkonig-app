//! Import ERP source systems into a pile as a business model rather than a
//! table mirror.
//!
//! Revolver names its tables `l_<N>` and its columns `f_<K>`, and a field
//! number means something different in every module — `f_3` is the company
//! name in the address book, the document number in sales, and free-text
//! remarks in payments. Ingesting that shape verbatim would move the problem
//! rather than solve it, so this faculty writes invoices with customers,
//! dates and totals, and line items with quantities and prices.
//!
//! The source database is live. Ingest is therefore convergent, not one-shot:
//! run it again over a newer snapshot and it appends exactly what changed.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use crate::access::initialize_signer;
use triblespace::core::collection::records::CollectionHandle;
use crate::access::{parse_collection, initialize_collection};
use crate::revolver::{
    census_timed, count_statement_with_fields, decode_copy, ingest, parse_timestamp, read, read_timed,
    statement_with_fields, FieldBinding, IngestReport, Options, Source, SLICE,
};

#[derive(Parser)]
#[command(
    name = "erp",
    version = env!("CARGO_PKG_VERSION"),
    about = "Import ERP systems as queryable TribleSpace business models"
)]
struct Cli {
    /// Path to the pile file.
    #[arg(long, env = "PILE", global = true)]
    pile: Option<PathBuf>,
    /// Existing durable signing-key file. Ordinary operations never create it.
    #[arg(long, env = "TRIBLESPACE_KEY", global = true)]
    key: Option<PathBuf>,
    /// Exact existing ERP root descriptor. Ordinary reads/writes never create one.
    #[arg(long, env = "TRIBLESPACE_COLLECTION_ERP", global = true, value_parser = parse_collection)]
    collection: Option<CollectionHandle>,
    #[command(subcommand)]
    command: Option<Command>,
}

/// How to reach the database. Shared by every subcommand that reads it.
#[derive(Clone, Debug, clap::Args)]
struct SourceArgs {
    /// Reach `psql` on this host over ssh instead of running it locally.
    #[arg(long, value_name = "HOST")]
    ssh: Option<String>,
    /// Ingest captured `COPY` output — `<dir>/<table>.copy` — instead of
    /// reading a database. This faculty never writes such a directory; it
    /// reads one so a snapshot taken during a maintenance window, or a
    /// synthetic fixture, can be ingested by the same code path.
    #[arg(long, value_name = "DIR", conflicts_with = "ssh")]
    from_dir: Option<PathBuf>,
    /// Run the remote `psql` through `sudo -u <user>`.
    #[arg(long, value_name = "USER", default_value = "postgres")]
    sudo_user: String,
    /// Do not use sudo on the remote host.
    #[arg(long, conflicts_with = "sudo_user")]
    no_sudo: bool,
    /// Database name.
    #[arg(long, default_value = "revolver")]
    database: String,
    /// Local `psql` executable.
    #[arg(long, default_value = "psql")]
    psql: String,
}

impl SourceArgs {
    fn source(&self) -> Source {
        if let Some(directory) = &self.from_dir {
            return Source::Files {
                directory: directory.clone(),
            };
        }
        match &self.ssh {
            None => Source::Local {
                database: self.database.clone(),
                psql: self.psql.clone(),
            },
            Some(host) => Source::Ssh {
                host: host.clone(),
                database: self.database.clone(),
                sudo_user: (!self.no_sudo).then(|| self.sudo_user.clone()),
            },
        }
    }
}

/// Which rows to read. Shared so `import --show-sql`, `import` and `verify`
/// cannot drift apart about what they are talking about.
#[derive(Clone, Debug, clap::Args)]
struct SelectionArgs {
    /// Bind an optional installation field (levy_amount, levy_amount_c, levy_rate, task_contact, task_step).
    #[arg(long = "field", value_name = "FIELD=COLUMN")]
    fields: Vec<FieldBinding>,
    /// Restrict to these tables. Defaults to the whole slice.
    #[arg(long = "table", value_name = "TABLE")]
    tables: Vec<String>,
    /// Include trashed and deactivated records, which the vendor's own
    /// queries exclude.
    #[arg(long)]
    include_removed: bool,
    /// Restrict to one company (Mandant).
    #[arg(long, value_name = "N")]
    mandant: Option<i128>,
}

impl SelectionArgs {
    fn tables(&self) -> Vec<String> {
        if self.tables.is_empty() {
            SLICE.iter().map(|table| (*table).to_owned()).collect()
        } else {
            self.tables.clone()
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Import statements, record invoices and review classifications and matches.
    Bookkeeping(crate::bookkeeping::cli::Cli),
    /// Record and inspect attributed financial observations.
    Finance {
        #[command(subcommand)]
        command: crate::finance::cli::Command,
    },
    /// Work with an external ERP source system.
    Source {
        #[command(subcommand)]
        system: SourceSystem,
    },
}

#[derive(Subcommand)]
enum SourceSystem {
    /// Revolver Office/Server.
    Revolver {
        #[command(subcommand)]
        command: RevolverCommand,
    },
}

#[derive(Subcommand)]
enum RevolverCommand {
    /// Explicitly create a private ERP root and signer; print its exact descriptor.
    Init,
    /// Read a source snapshot and converge the pile onto it.
    Import {
        #[command(flatten)]
        source: SourceArgs,
        #[command(flatten)]
        selection: SelectionArgs,
        /// ISO-4217 code for amounts whose record carries no currency
        /// selector.
        #[arg(long, default_value = "EUR", value_name = "ISO")]
        currency: String,
        /// Map a Revolver currency selector onto an ISO-4217 code, as
        /// `64=EUR`. Repeatable. An unmapped selector stops the ingest rather
        /// than being guessed at.
        #[arg(long = "currency-code", value_name = "CODE=ISO")]
        currency_codes: Vec<String>,
        /// Facts per published commit.
        #[arg(long, default_value_t = 20_000)]
        batch: usize,
        /// Read, translate and validate everything; publish nothing.
        #[arg(long)]
        dry_run: bool,
        /// State when the source was read, as `YYYY-MM-DD HH:MM:SS`, instead
        /// of stamping now.
        ///
        /// Every snapshot one import publishes carries one instant — the
        /// import is one logical observation of the whole database, and the
        /// stamp is what orders successive observations. Reading a live
        /// database, now is that instant. Replaying a capture taken during a
        /// maintenance window, it is not: pass when the capture was taken, or
        /// a later replay of an older reading will overtake a newer one.
        #[arg(long, value_name = "TIMESTAMP")]
        as_of: Option<String>,
        /// Print the exact SQL this import would run, then exit without
        /// connecting to the source or opening a pile.
        #[arg(long, conflicts_with = "dry_run")]
        show_sql: bool,
    },
    /// Compare Revolver record counts with the current pile.
    Verify {
        #[command(flatten)]
        source: SourceArgs,
        #[command(flatten)]
        selection: SelectionArgs,
    },
    /// Report what the Revolver collection currently holds.
    Census {
        /// Also report how long materializing and resolving heads took.
        ///
        /// Head resolution is four whole-collection scans; whether that stays
        /// affordable as the collection grows is worth being able to answer
        /// with a measurement rather than an opinion.
        #[arg(long)]
        timings: bool,
    },
    /// Declare a corporate group, or list the declared ones.
    ///
    /// A group is a first-party entity that several legal entities point at,
    /// deliberately not a `same_as` between the entities: groups reorganise,
    /// and an equivalence that stops being true cannot be retracted from an
    /// append-only collection. Adding a member is monotone; so is a later
    /// reorganisation.
    Group {
        /// Display name for the group. Annotation, not identity — renaming a
        /// group is not a new group.
        #[arg(long, value_name = "TEXT")]
        name: Option<String>,
        /// A member's Revolver customer number (`l_256.f_62`). Repeatable.
        ///
        /// By number rather than by name on purpose -- matching on names is the
        /// heuristic this grouping exists to replace.
        ///
        /// This is the WHOLE membership, not an addition. Omitting a company
        /// that was a member removes it, which is how a divestiture is recorded.
        #[arg(long = "member", value_name = "NUMBER")]
        members: Vec<String>,
    },
    /// Find party records by company name, with the customer number a group
    /// declaration names them by.
    Parties {
        /// Case-insensitive substring of the company name.
        needle: String,
    },
}

fn parse_currency_codes(pairs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::from([("64".to_owned(), "EUR".to_owned())]);
    for pair in pairs {
        let (code, iso) = pair
            .split_once('=')
            .with_context(|| format!("--currency-code {pair:?} is not CODE=ISO"))?;
        if iso.len() != 3 || !iso.bytes().all(|byte| byte.is_ascii_uppercase()) {
            bail!("--currency-code {pair:?}: {iso:?} is not an ISO-4217 alphabetic code");
        }
        map.insert(code.to_owned(), iso.to_owned());
    }
    Ok(map)
}

fn cmd_init(pile: &Path, key: Option<&Path>) -> Result<()> {
    let created = !pile.exists();
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(pile)
        .with_context(|| format!("create pile {}", pile.display()))?;
    initialize_signer(pile, key)?;
    let handle = initialize_collection(pile, key)?;
    let hex: String = handle.raw.iter().map(|byte| format!("{byte:02x}")).collect();
    println!("{} {} · private ERP descriptor blake3:{hex}", if created { "created" } else { "using" }, pile.display());
    Ok(())
}

fn cmd_show_sql(selection: &SelectionArgs) -> Result<()> {
    for table in selection.tables() {
        print!(
            "{}",
            statement_with_fields(&table, selection.include_removed, selection.mandant, &selection.fields)?
        );
    }
    Ok(())
}

fn report_lines(report: &IngestReport) {
    for table in &report.tables {
        println!(
            "{:<22} {:>7} rows  {:>7} records  {:>7} changed{}",
            table.table,
            table.rows,
            table.records,
            table.changed,
            collapsed_note(table.collapsed())
        );
    }
    println!(
        "{:<22} {:>7} rows  {:>7} records  {:>7} changed  {} commit(s){}{}",
        "total",
        report.rows(),
        report.records(),
        report.changed(),
        report.commits,
        collapsed_note(report.collapsed()),
        if report.dry_run { " · dry run" } else { "" }
    );
}

/// Duplicated source identities are worth saying out loud every time, and worth
/// saying nothing at all when there are none.
fn collapsed_note(collapsed: usize) -> String {
    if collapsed == 0 {
        String::new()
    } else {
        format!("  ({collapsed} duplicate row(s) merged)")
    }
}

fn cmd_verify(
    pile: &Path,
    key: Option<&Path>,
    handle: CollectionHandle,
    source: &Source,
    selection: &SelectionArgs,
) -> Result<()> {
    let facts = read(pile, key, handle)?;
    let (census, _) = census_timed(&facts);
    println!("source: {}", source.describe());
    for table in selection.tables() {
        let body = source.run(&count_statement_with_fields(
            &table,
            selection.include_removed,
            selection.mandant,
            &selection.fields,
        )?)?;
        let counted = decode_copy(&body, 2, &table)?;
        let counted = counted.first().cloned().unwrap_or_default();
        let value = |index: usize| counted.get(index).cloned().flatten().unwrap_or_default();
        // Rows and records differ wherever `docid` is duplicated. The pile
        // holds records, so records is what the verdict compares — but a run
        // that quietly stopped reporting the row count would stop showing that
        // the duplication is there at all.
        let (rows, records) = (value(0), value(1));
        let held = census.table(&table);
        let verdict = if records == held.to_string() {
            "ok"
        } else {
            "MISMATCH"
        };
        println!(
            "{table:<22} source {rows:>8} rows {records:>8} records  pile {held:>8}  {verdict}"
        );
    }
    if census.forked > 0 {
        println!(
            "{} record(s) have more than one current snapshot; the next ingest reconciles them",
            census.forked
        );
    }
    Ok(())
}

fn cmd_census(pile: &Path, key: Option<&Path>, handle: CollectionHandle, timings: bool) -> Result<()> {
    let (facts, materialize) = read_timed(pile, key, handle)?;
    let (census, phases) = census_timed(&facts);
    if timings {
        println!("{:<22} {:>8.2?}", "materialize", materialize);
        println!("{:<22} {:>8.2?}", "resolve heads", phases.heads);
        println!("{:<22} {:>8.2?}", "count anchors", phases.counts);
    }
    for (table, held) in &census.by_table {
        println!("{table:<22} {held:>8}");
    }
    println!("{:<22} {:>8}", "records", census.records());
    println!("{:<22} {:>8}", "state snapshots", census.snapshots);
    println!("{:<22} {:>8}", "vocabulary codes", census.codes);
    println!("{:<22} {:>8}", "folded records", census.folded);
    println!("{:<22} {:>8}", "forked records", census.forked);
    println!("{:<22} {:>8}", "facts", facts.len());
    Ok(())
}

fn cmd_group(
    pile: &Path,
    key: Option<&Path>,
    handle: CollectionHandle,
    name: Option<String>,
    members: &[String],
) -> Result<()> {
    if members.is_empty() {
        let snapshot = crate::revolver::read_snapshot(pile, key, handle)?;
        let heads = crate::revolver::group_heads(&snapshot.facts);
        if heads.is_empty() {
            println!("no corporate groups declared");
            return Ok(());
        }
        for group in heads {
            let name = group
                .names
                .first()
                .and_then(|handle| {
                    crate::revolver::read_text(&snapshot.reader, *handle).ok()
                })
                .unwrap_or_else(|| "(unnamed)".to_owned());
            println!(
                "{} ({:X}) — {} members{}",
                name.trim(),
                group.id,
                group.members.len(),
                if group.names.len() > 1 {
                    format!("  ** {} competing names **", group.names.len())
                } else {
                    String::new()
                }
            );
        }
        return Ok(());
    }
    let report = crate::revolver::declare_group(pile, key, handle, name.as_deref(), members)?;
    match report.group {
        None => println!("unchanged, nothing published"),
        Some(id) => {
            println!(
                "group {id:X} · {} member(s) · {} facts{}",
                report.resolved.len(),
                report.facts,
                if report.superseded.is_empty() {
                    String::new()
                } else {
                    format!(
                        " · supersedes {}",
                        report
                            .superseded
                            .iter()
                            .map(|id| format!("{id:X}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            );
        }
    }
    Ok(())
}

fn cmd_parties(pile: &Path, key: Option<&Path>, handle: CollectionHandle, needle: &str) -> Result<()> {
    let snapshot = crate::revolver::read_snapshot(pile, key, handle)?;
    let rows = crate::revolver::parties_like(&snapshot.facts, &snapshot.reader, needle);
    if rows.is_empty() {
        println!("no party record matches {needle:?}");
        return Ok(());
    }
    for (anchor, number, name) in &rows {
        println!("{number:<12} {name}  ({anchor:X})");
    }
    println!("{} match(es)", rows.len());
    Ok(())
}

fn require_pile(pile: Option<&Path>) -> Result<&Path> {
    pile.context("this operation requires --pile PATH or the PILE environment variable")
}

fn require_collection(handle: Option<CollectionHandle>) -> Result<CollectionHandle> {
    handle.context("supply --collection or TRIBLESPACE_COLLECTION_ERP; ordinary operations never create a root")
}

/// Run the ERP command with an explicit argv including the program name.
pub fn run_from<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    let pile = cli.pile;
    let key = cli.key;
    let collection = cli.collection;
    match cli.command {
        Some(Command::Bookkeeping(command)) => crate::bookkeeping::cli::run(
            command, require_pile(pile.as_deref())?.to_path_buf(), key, require_collection(collection)?,
        ),
        Some(Command::Finance { command }) => crate::finance::cli::run(
            command, require_pile(pile.as_deref())?, key.as_deref(), require_collection(collection)?,
        ),
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
        Some(Command::Source {
            system: SourceSystem::Revolver { command },
        }) => match command {
            RevolverCommand::Init => cmd_init(require_pile(pile.as_deref())?, key.as_deref()),
            RevolverCommand::Census { timings } => {
                cmd_census(require_pile(pile.as_deref())?, key.as_deref(), require_collection(collection)?, timings)
            }
            RevolverCommand::Group { name, members } => cmd_group(
                require_pile(pile.as_deref())?,
                key.as_deref(),
                require_collection(collection)?,
                name,
                &members,
            ),
            RevolverCommand::Parties { needle } => {
                cmd_parties(require_pile(pile.as_deref())?, key.as_deref(), require_collection(collection)?, &needle)
            }
            RevolverCommand::Verify { source, selection } => cmd_verify(
                require_pile(pile.as_deref())?,
                key.as_deref(),
                require_collection(collection)?,
                &source.source(),
                &selection,
            ),
            RevolverCommand::Import {
                source,
                selection,
                currency,
                currency_codes,
                batch,
                dry_run,
                as_of,
                show_sql,
            } => {
                if show_sql {
                    return cmd_show_sql(&selection);
                }
                let observed = match as_of.as_deref() {
                    None => None,
                    Some(text) => Some(
                        parse_timestamp(text)
                            .with_context(|| format!("--as-of {text:?} is not a timestamp"))?
                            .with_context(|| {
                                format!("--as-of {text:?} is too early to be an import stamp")
                            })?,
                    ),
                };
                let options = Options {
                    tables: selection.tables(),
                    field_bindings: selection.fields,
                    include_removed: selection.include_removed,
                    mandant: selection.mandant,
                    currency,
                    currency_map: parse_currency_codes(&currency_codes)?,
                    batch,
                    dry_run,
                    observed,
                };
                let source = source.source();
                println!("source: {}", source.describe());
                let report = ingest(
                    require_pile(pile.as_deref())?,
                    key.as_deref(),
                    require_collection(collection)?,
                    &source,
                    &options,
                )?;
                report_lines(&report);
                Ok(())
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currency_codes_default_to_the_documented_euro_selector() {
        let map = parse_currency_codes(&[]).unwrap();
        assert_eq!(map.get("64").map(String::as_str), Some("EUR"));
    }

    #[test]
    fn currency_codes_reject_a_non_iso_alphabetic_code() {
        assert!(parse_currency_codes(&["7=chf".to_owned()]).is_err());
        assert!(parse_currency_codes(&["7=CHFX".to_owned()]).is_err());
        assert!(parse_currency_codes(&["7".to_owned()]).is_err());
        assert_eq!(
            parse_currency_codes(&["7=CHF".to_owned()])
                .unwrap()
                .get("7")
                .map(String::as_str),
            Some("CHF")
        );
    }

    #[test]
    fn selection_defaults_to_the_whole_slice() {
        let selection = SelectionArgs {
            fields: Vec::new(),
            tables: Vec::new(),
            include_removed: false,
            mandant: None,
        };
        assert_eq!(selection.tables(), SLICE.to_vec());
    }

    #[test]
    fn source_defaults_to_sudo_postgres_over_ssh_and_plain_psql_locally() {
        let over_ssh = SourceArgs {
            ssh: Some("host".to_owned()),
            from_dir: None,
            sudo_user: "postgres".to_owned(),
            no_sudo: false,
            database: "revolver".to_owned(),
            psql: "psql".to_owned(),
        };
        assert_eq!(
            over_ssh.source(),
            Source::Ssh {
                host: "host".to_owned(),
                database: "revolver".to_owned(),
                sudo_user: Some("postgres".to_owned()),
            }
        );
        let local = SourceArgs {
            ssh: None,
            ..over_ssh
        };
        assert_eq!(
            local.source(),
            Source::Local {
                database: "revolver".to_owned(),
                psql: "psql".to_owned(),
            }
        );
    }

    #[test]
    fn cli_verifies() {
        Cli::command().debug_assert();
    }

    #[test]
    fn source_first_import_path_parses() {
        let cli = Cli::try_parse_from([
            "erp",
            "source",
            "revolver",
            "import",
            "--pile",
            "business.pile",
            "--from-dir",
            "snapshot",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Source {
                system: SourceSystem::Revolver {
                    command: RevolverCommand::Import { .. }
                }
            })
        ));
        assert_eq!(cli.pile.as_deref(), Some(Path::new("business.pile")));
    }

    #[test]
    fn show_sql_is_an_import_view_and_needs_no_pile() {
        let cli = Cli::try_parse_from([
            "erp",
            "source",
            "revolver",
            "import",
            "--show-sql",
            "--table",
            "l_256",
        ])
        .unwrap();
        let Some(Command::Source {
            system:
                SourceSystem::Revolver {
                    command: RevolverCommand::Import {
                        show_sql, selection, ..
                    },
                },
        }) = cli.command
        else {
            panic!("--show-sql parses as an import view");
        };
        assert!(show_sql);
        // `needs no pile` is a property of the view, not of the environment.
        // Asserting `cli.pile.is_none()` asserted the latter — `--pile` reads
        // through `PILE`, so an ambient one answered and failed the test for
        // reasons that had nothing to do with `--show-sql`. Running the view
        // is the claim itself.
        cmd_show_sql(&selection).expect("the view writes SQL without a pile");
    }

    #[test]
    fn bare_invocation_reaches_the_usage_path() {
        let cli = Cli::try_parse_from(["erp"]).unwrap();
        assert!(cli.command.is_none());
    }
}
