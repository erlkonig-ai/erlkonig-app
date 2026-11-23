//! Import ordered PostgreSQL COPY records into a selected ERP collection.
//! Field positions are shared by live database reads and offline replay.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use hifitime::Epoch;
use num_rational::Ratio;
use rayon::prelude::*;
use triblespace::core::collection::records::CollectionHandle;
use triblespace::core::collection::{CollectionSnapshotExt, CollectionStoreExt};
use triblespace::core::repo::SnapshotSource;
use triblespace::core::inline::encodings::money::Euro;
use triblespace::core::inline::{Encodes, TryToInline};
use triblespace::core::metadata;
use triblespace::core::repo::pile::{Pile, PileSnapshot};
use triblespace::core::trible::{build_intrinsic_entity, IntrinsicEntityRow};
use triblespace::macros::{entity, find, pattern};
use triblespace::prelude::*;

use self::schema::{
    activity, allocation, article, booking, code, employee, group, line, opportunity,
    opportunity_step, party, payment, price, project, purchase, purchase_line, record, request,
    revolver, sales, step, supply, task, time, user, Euros, DOMAIN_ACTIVITY_CATEGORY,
    DOMAIN_ACTIVITY_STATUS, DOMAIN_ADDRESS_TYPE, DOMAIN_ARTICLE_CATEGORY, DOMAIN_ARTICLE_TAX_CLASS,
    DOMAIN_BILLING_MODE, DOMAIN_CURRENCY, DOMAIN_DELIVERY_METHOD, DOMAIN_DOCTYPE,
    DOMAIN_EMPLOYEE_MARITAL_STATUS, DOMAIN_EMPLOYEE_STATUS, DOMAIN_EMPLOYEE_TAX_CLASS,
    DOMAIN_NEXT_STEP_TYPE, DOMAIN_OPPORTUNITY_SOURCE,
    DOMAIN_OPPORTUNITY_STATUS, DOMAIN_OPPORTUNITY_STEP_CATEGORY, DOMAIN_PAYMENT_MODE,
    DOMAIN_POSITION_TYPE, DOMAIN_PRICE_CATEGORY, DOMAIN_PRICE_SELECTION, DOMAIN_PROJECT_DIVISION,
    DOMAIN_PROJECT_STATUS, DOMAIN_PURCHASE_DIVISION, DOMAIN_PURCHASE_TAX_CLASS,
    DOMAIN_RECORD_STATE, DOMAIN_REQUEST_DOCUMENT_TYPE, DOMAIN_SALES_TAX_CLASS,
    DOMAIN_STEP_TAX_CLASS, DOMAIN_STEP_UNIT, DOMAIN_TASK_CATEGORY, KIND_ACTIVITY, KIND_ARTICLE,
    KIND_ARTICLE_PRICE, KIND_ARTICLE_SUPPLY, KIND_CODE, KIND_CODE_DOMAIN, KIND_CORPORATE_GROUP,
    KIND_EMPLOYEE, KIND_HR_REQUEST, KIND_OPPORTUNITY, KIND_OPPORTUNITY_STEP, KIND_PARTY,
    KIND_PAYMENT, KIND_PAYMENT_ALLOCATION, KIND_PROJECT, KIND_PROJECT_STEP, KIND_PURCHASE_DOCUMENT,
    KIND_PURCHASE_LINE, KIND_SALES_DOCUMENT, KIND_SALES_LINE, KIND_SNAPSHOT, KIND_TASK,
    KIND_TIME_ENTRY, KIND_TIME_LINE, KIND_USER,
};
use crate::access::{load_signer, open_pile_strict, open_collection};

pub mod schema;

pub type TextHandle = Inline<inlineencodings::Handle<blobencodings::UTF8String>>;
pub type IntervalValue = Inline<inlineencodings::NsTAIInterval>;
pub type EuroValue = Inline<inlineencodings::Currency<Euro>>;

#[derive(Debug)]
pub struct Snapshot {
    pub facts: TribleSet,
    pub reader: PileSnapshot,
}

const UNSET_DATE_YEAR: i32 = 1900;

const EURO_CODE: &str = "EUR";

const SOURCE_EXPONENT: u32 = 3;

const SOURCE_RESIDUE: i128 = 10;

const UNROUNDED_COLUMNS: &[(&str, &str)] = &[("l_263_3", "f_8")];

pub fn ingest_collection_name() -> &'static str { "erp" }

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    Local { database: String, psql: String },
    Ssh {
        host: String,
        database: String,
        sudo_user: Option<String>,
    },
    Files { directory: PathBuf },
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Self::Local { .. } => "local psql".into(),
            Self::Ssh {
                host,
                sudo_user,
                ..
            } => match sudo_user {
                Some(_) => format!("ssh {host}, sudo psql"),
                None => format!("ssh {host}, psql"),
            },
            Self::Files { directory } => {
                format!("captured COPY output in {}", directory.display())
            }
        }
    }

    fn command(&self) -> Result<Command> {
        const PSQL_ARGS: [&str; 6] = ["-X", "-q", "-v", "ON_ERROR_STOP=1", "-f", "-"];
        Ok(match self {
            Self::Local { database, psql } => {
                let mut command = Command::new(psql);
                command.args(["-d", database]).args(PSQL_ARGS);
                command
            }
            Self::Ssh {
                host,
                database,
                sudo_user,
            } => {
                if host.is_empty() || host.starts_with('-') {
                    bail!("SSH destination must be a hostname or user@hostname");
                }
                let mut words = Vec::new();
                if let Some(user) = sudo_user {
                    words.extend(["sudo", "-u", user.as_str(), "--"]);
                }
                words.extend(["psql", "-d", database.as_str()]);
                words.extend(PSQL_ARGS);
                // OpenSSH passes a single command string to the remote login shell.
                let remote = words.into_iter()
                    .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut command = Command::new("ssh");
                command.args(["--", host, &remote]);
                command
            }
            Self::Files { directory } => bail!(
                "{} holds captured output; there is no database to run a statement against",
                directory.display()
            ),
        })
    }

    pub fn run(&self, statement: &str) -> Result<String> {
        let mut child = self
            .command()?
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("spawn psql via {}", self.describe()))?;
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("psql stdin was not captured"))?
            .write_all(statement.as_bytes())
            .context("write SQL to psql stdin")?;
        let output = child.wait_with_output().context("wait for psql")?;
        if !output.status.success() {
            bail!(
                "psql failed ({}) via {}: {}",
                output.status,
                self.describe(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).context("psql produced non-UTF-8 output")
    }

    pub fn fetch(&self, table: &str, statement: &str) -> Result<String> {
        match self {
            Self::Files { directory } => {
                let path = directory.join(format!("{table}.copy"));
                std::fs::read_to_string(&path)
                    .with_context(|| format!("read captured {}", path.display()))
            }
            _ => self.run(statement),
        }
    }
}

fn decode_field(field: &str) -> Option<String> {
    if field == "\\N" {
        return None;
    }
    if !field.contains('\\') {
        return Some(field.to_owned());
    }
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('v') => out.push('\u{b}'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Some(out)
}

pub fn decode_copy(body: &str, columns: usize, label: &str) -> Result<Vec<Vec<Option<String>>>> {
    let mut rows = Vec::new();
    for (index, raw) in body.split('\n').enumerate() {
        if raw.is_empty() || raw == "\\." {
            continue;
        }
        let row: Vec<Option<String>> = raw.split('\t').map(decode_field).collect();
        if row.len() != columns {
            bail!(
                "{label} line {}: expected {columns} columns, found {}",
                index + 1,
                row.len()
            );
        }
        rows.push(row);
    }
    Ok(rows)
}

struct Row<'a> {
    table: &'a str,
    key: String,
    columns: &'a [&'a str],
    values: &'a [Option<String>],
}

impl Row<'_> {
    fn raw(&self, column: &str) -> Result<Option<&str>> {
        let index = self
            .columns
            .iter()
            .position(|name| *name == column)
            .ok_or_else(|| anyhow!("{}: column {column} was not selected", self.table))?;
        Ok(self.values[index]
            .as_deref()
            .filter(|text| !text.is_empty()))
    }

    fn text(&self, column: &str) -> Result<Option<String>> {
        Ok(self
            .raw(column)?
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty()))
    }

    fn short(&self, column: &str) -> Result<Option<String>> {
        let Some(text) = self.text(column)? else {
            return Ok(None);
        };
        if text.len() > 32 {
            bail!(
                "{}.{column} {}: {} bytes exceeds the 32-byte inline budget",
                self.table,
                self.key,
                text.len()
            );
        }
        Ok(Some(text))
    }

    fn integer(&self, column: &str) -> Result<Option<i128>> {
        let Some(text) = self.raw(column)? else {
            return Ok(None);
        };
        text.trim()
            .parse::<i128>()
            .map(Some)
            .with_context(|| format!("{}.{column} {}: not an integer", self.table, self.key))
    }

    fn required_integer(&self, column: &str) -> Result<i128> {
        self.integer(column)?
            .ok_or_else(|| anyhow!("{}.{column} {}: missing", self.table, self.key))
    }

    fn nonzero(&self, column: &str) -> Result<Option<i128>> {
        Ok(self.integer(column)?.filter(|value| *value != 0))
    }

    fn reference(&self, column: &str) -> Result<Option<i128>> {
        self.nonzero(column)
    }

    fn rational(&self, column: &str) -> Result<Option<Ratio<i128>>> {
        let Some(text) = self.raw(column)? else {
            return Ok(None);
        };
        parse_decimal(text)
            .map(Some)
            .with_context(|| format!("{}.{column} {}: not an exact decimal", self.table, self.key))
    }

    fn proportion(&self, column: &str) -> Result<Option<Ratio<i128>>> {
        Ok(self
            .rational(column)?
            .map(|percent| percent / Ratio::from_integer(100)))
    }

    fn flag(&self, column: &str) -> Result<Option<bool>> {
        Ok(matches!(self.raw(column)?, Some("t") | Some("true")).then_some(true))
    }

    fn money(&self, column: &str, options: &Options) -> Result<Option<EuroValue>> {
        let Some(units) = self.integer(column)? else {
            return Ok(None);
        };
        if units == 0 {
            return Ok(None);
        }
        let selector = self.raw(&format!("{column}_c"))?.map(str::to_owned);
        options.require_euro(selector.as_deref(), self.table, column, &self.key)?;
        if units % SOURCE_RESIDUE != 0
            && !UNROUNDED_COLUMNS
                .iter()
                .any(|(table, unrounded)| *table == self.table && *unrounded == column)
        {
            bail!(
                "{}.{column} {}: stored value {units} is not a multiple of {SOURCE_RESIDUE}, so \
                 this column is not a euro figure at {SOURCE_EXPONENT} decimal places. The value \
                 itself is fine — an exact rational holds a sub-cent amount without rounding — so \
                 what this is reporting is a column read at the wrong scale, which would be off by \
                 a factor of ten and silent. If Revolver genuinely computes this figure rather \
                 than printing it, add it to UNROUNDED_COLUMNS with the count that says so.",
                self.table,
                self.key
            );
        }
        let amount = Euros::from_units(units, SOURCE_EXPONENT)
            .map_err(|error| anyhow!("{}.{column} {}: {error}", self.table, self.key))?;
        amount
            .try_to_inline()
            .map(Some)
            .map_err(|error| anyhow!("{}.{column} {}: {error:?}", self.table, self.key))
    }

    fn date(&self, column: &str) -> Result<Option<IntervalValue>> {
        let Some(text) = self.raw(column)? else {
            return Ok(None);
        };
        parse_timestamp(text)
            .with_context(|| format!("{}.{column} {}: unreadable date", self.table, self.key))
    }

    fn time_of_day(&self, column: &str) -> Result<Option<Inline<inlineencodings::NsDuration>>> {
        let Some(text) = self.raw(column)? else {
            return Ok(None);
        };
        let nanos = parse_time_of_day(text).with_context(|| {
            format!(
                "{}.{column} {}: unreadable time of day",
                self.table, self.key
            )
        })?;
        Ok((nanos != 0).then(|| inlineencodings::NsDuration::encode(nanos)))
    }
}

pub fn parse_time_of_day(text: &str) -> Result<i128> {
    let text = text.trim();
    let mut clock = text.split(':');
    let hour: i128 = clock
        .next()
        .ok_or_else(|| anyhow!("missing hour"))?
        .parse()
        .context("hour")?;
    let minute: i128 = clock.next().unwrap_or("0").parse().context("minute")?;
    let seconds = clock.next().unwrap_or("0");
    let (whole, fraction) = match seconds.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (seconds, ""),
    };
    let second: i128 = whole.parse().context("second")?;
    let mut nanos: i128 = 0;
    for index in 0..9 {
        let digit = fraction.as_bytes().get(index).copied().unwrap_or(b'0');
        if !digit.is_ascii_digit() {
            bail!("fractional seconds are not decimal");
        }
        nanos = nanos * 10 + i128::from(digit - b'0');
    }
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..=60).contains(&second) {
        bail!("{text:?} is not a time of day");
    }
    Ok(((hour * 60 + minute) * 60 + second) * 1_000_000_000 + nanos)
}

pub fn parse_timestamp(text: &str) -> Result<Option<IntervalValue>> {
    let text = text.trim();
    let (date, time) = match text.split_once(' ') {
        Some((date, time)) => (date, Some(time)),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let year: i32 = parts
        .next()
        .ok_or_else(|| anyhow!("missing year"))?
        .parse()
        .context("year")?;
    let month: u8 = parts
        .next()
        .ok_or_else(|| anyhow!("missing month"))?
        .parse()
        .context("month")?;
    let day: u8 = parts
        .next()
        .ok_or_else(|| anyhow!("missing day"))?
        .parse()
        .context("day")?;
    if year < UNSET_DATE_YEAR {
        return Ok(None);
    }
    let (hour, minute, second, nanos) = match time {
        None => (0, 0, 0, 0),
        Some(time) => {
            let time = time.split(['+', 'Z']).next().unwrap_or(time);
            let mut clock = time.split(':');
            let hour: u8 = clock.next().unwrap_or("0").parse().context("hour")?;
            let minute: u8 = clock.next().unwrap_or("0").parse().context("minute")?;
            let seconds = clock.next().unwrap_or("0");
            let (whole, fraction) = match seconds.split_once('.') {
                Some((whole, fraction)) => (whole, fraction),
                None => (seconds, ""),
            };
            let second: u8 = whole.parse().context("second")?;
            let mut nanos: u32 = 0;
            for index in 0..9 {
                let digit = fraction.as_bytes().get(index).copied().unwrap_or(b'0');
                if !digit.is_ascii_digit() {
                    bail!("fractional seconds are not decimal");
                }
                nanos = nanos * 10 + u32::from(digit - b'0');
            }
            (hour, minute, second, nanos)
        }
    };
    let epoch = Epoch::from_gregorian_utc(year, month, day, hour, minute, second, nanos);
    Ok(Some((epoch, epoch).try_to_inline().map_err(|error| {
        anyhow!("{text} is not a representable instant: {error:?}")
    })?))
}

pub fn parse_decimal(text: &str) -> Result<Ratio<i128>> {
    let text = text.trim();
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (
            mantissa,
            exponent.parse::<i32>().context("decimal exponent")?,
        ),
        None => (text, 0),
    };
    let (sign, digits) = match mantissa.strip_prefix('-') {
        Some(rest) => (-1i128, rest),
        None => (1i128, mantissa.strip_prefix('+').unwrap_or(mantissa)),
    };
    let (whole, fraction) = match digits.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (digits, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        bail!("{text:?} has no digits");
    }
    if !whole
        .bytes()
        .chain(fraction.bytes())
        .all(|b| b.is_ascii_digit())
    {
        bail!("{text:?} is not a decimal");
    }
    let numerator: i128 = format!("{whole}{fraction}")
        .parse()
        .with_context(|| format!("{text:?} does not fit i128"))?;
    let scale = i32::try_from(fraction.len()).context("fraction length")? - exponent;
    let value = Ratio::from_integer(sign * numerator);
    let power = Ratio::from_integer(
        10i128
            .checked_pow(scale.unsigned_abs())
            .ok_or_else(|| anyhow!("{text:?} needs more than i128 of precision"))?,
    );
    Ok(if scale >= 0 {
        value / power
    } else {
        value * power
    })
}

fn domain_fragment(name: &str) -> (Fragment, Id) {
    let mut blobs = Fragment::empty();
    let handle: TextHandle = blobs.put(name.to_owned());
    let mut fragment = entity! {
        metadata::tag: &KIND_CODE_DOMAIN,
        metadata::name: handle,
    };
    let id = fragment.root().expect("intrinsic domain has one root");
    fragment += blobs;
    (fragment, id)
}

fn code_fragment(domain: Id, value: &str, label: Option<&str>) -> Result<(Fragment, Id)> {
    if value.len() > 32 {
        bail!("code {value:?} exceeds the 32-byte inline budget");
    }
    let mut fragment = entity! {
        metadata::tag: &KIND_CODE,
        code::domain: domain,
        code::code: value,
    };
    let id = fragment.root().expect("intrinsic code has one root");
    if let Some(label) = label {
        let handle: TextHandle = fragment.put(label.to_owned());
        fragment += entity! { ExclusiveId::force_ref(&id) @ metadata::name: handle };
    }
    Ok((fragment, id))
}

fn label_for(domain: &str, value: &str) -> Option<&'static str> {
    let table: &[(&str, &str)] = match domain {
        DOMAIN_DOCTYPE => &[
            ("257", "Firma"),
            ("258", "Ansprechpartner*in"),
            ("259", "Person"),
            ("281", "Angebot"),
            ("283", "Auftragsbestätigung"),
            ("284", "Lieferschein"),
            ("285", "Rechnung"),
            ("286", "Gutschrift"),
            ("299", "Zahlungs-Eingang"),
            ("300", "Zahlungs-Ausgang"),
            ("301", "Mahnung"),
            ("337QKSHEYMXEQUSSPAP", "Rechnungsentwurf"),
            ("338QLUXSTJMFYSMLQQP", "Kostenvoranschlag"),
        ],
        DOMAIN_ADDRESS_TYPE => &[
            ("1", "Kunde"),
            ("2", "Lieferant"),
            ("3", "Kunde/Lieferant"),
            ("5", "Mitarbeiter"),
        ],
        DOMAIN_CURRENCY => &[("64", "EUR")],
        DOMAIN_RECORD_STATE => &[("4", "pending"), ("8", "archived")],
        DOMAIN_POSITION_TYPE => &[
            ("0", "Standard"),
            ("10", "Alternativ (optional)"),
            ("20", "Nur Text"),
        ],
        DOMAIN_SALES_TAX_CLASS => &[("1", "reduziert"), ("2", "voll")],
        _ => &[],
    };
    table
        .iter()
        .find(|(code, _)| *code == value)
        .map(|(_, label)| *label)
}

#[derive(Default)]
struct Vocabulary {
    known: HashSet<Id>,
    domains: HashMap<&'static str, Id>,
    codes: HashMap<(Id, String), Id>,
    fragment: Fragment,
}

impl Vocabulary {
    fn seeded(space: &TribleSet) -> Self {
        let mut known = ids_of_kind(space, KIND_CODE_DOMAIN)
            .into_iter()
            .collect::<HashSet<Id>>();
        known.extend(ids_of_kind(space, KIND_CODE));
        Self {
            known,
            ..Self::default()
        }
    }

    fn domain(&mut self, name: &'static str) -> Id {
        if let Some(id) = self.domains.get(name) {
            return *id;
        }
        let (fragment, id) = domain_fragment(name);
        if !self.known.contains(&id) {
            self.fragment += fragment;
        }
        self.domains.insert(name, id);
        id
    }

    fn code(&mut self, domain: &'static str, value: &str) -> Result<Id> {
        let domain_id = self.domain(domain);
        let key = (domain_id, value.to_owned());
        if let Some(id) = self.codes.get(&key) {
            return Ok(*id);
        }
        let (fragment, id) = code_fragment(domain_id, value, label_for(domain, value))?;
        if !self.known.contains(&id) {
            self.fragment += fragment;
        }
        self.codes.insert(key, id);
        Ok(id)
    }

    fn code_opt(&mut self, domain: &'static str, value: Option<&str>) -> Result<Option<Id>> {
        value.map(|value| self.code(domain, value)).transpose()
    }

    fn take(&mut self) -> Fragment {
        std::mem::take(&mut self.fragment)
    }
}

pub fn record_anchor(kind: Id, mandant: i128, docid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &kind,
        revolver::mandant: mandant,
        revolver::record: docid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn user_anchor(userid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_USER,
        user::userid: userid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn sales_line_anchor(document: Id, itemid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_SALES_LINE,
        line::document: document,
        revolver::position: itemid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn allocation_anchor(payment_anchor: Id, itemid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_PAYMENT_ALLOCATION,
        allocation::payment: payment_anchor,
        revolver::position: itemid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn project_step_anchor(project: Id, itemid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_PROJECT_STEP,
        step::project: project,
        revolver::position: itemid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn purchase_line_anchor(document: Id, itemid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_PURCHASE_LINE,
        purchase_line::document: document,
        revolver::position: itemid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn booking_anchor(entry: Id, itemid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_TIME_LINE,
        booking::entry: entry,
        revolver::position: itemid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn supply_anchor(article: Id, itemid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_ARTICLE_SUPPLY,
        supply::article: article,
        revolver::position: itemid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn article_price_anchor(article: Id, itemid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_ARTICLE_PRICE,
        price::article: article,
        revolver::position: itemid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

pub fn opportunity_step_anchor(opportunity: Id, itemid: i128) -> (Fragment, Id) {
    let fragment = entity! {
        metadata::tag: &KIND_OPPORTUNITY_STEP,
        opportunity_step::opportunity: opportunity,
        revolver::position: itemid,
    };
    let id = fragment.root().expect("intrinsic anchor has one root");
    (fragment, id)
}

struct Observation {
    anchor: Fragment,
    anchor_id: Id,
    content: Fragment,
    key: String,
}

impl Observation {
    fn new(row: &Row<'_>, anchor: Fragment, anchor_id: Id, content: Fragment) -> Self {
        Self {
            anchor,
            anchor_id,
            content,
            key: row.key.clone(),
        }
    }
}

pub struct HeadOrder<'a> {
    facts: &'a TribleSet,
    stated: StatedOrder<'a, TribleSet, inlineencodings::NsTAIInterval>,
    legacy: ObservationOrder<'a, TribleSet>,
}

impl<'a> HeadOrder<'a> {
    fn new(facts: &'a TribleSet) -> Self {
        Self {
            facts,
            stated: StatedOrder::new(facts, revolver::of.id(), metadata::created_at.id()),
            legacy: ObservationOrder::new(facts, metadata::supersedes.id()),
        }
    }

    fn stamped(&self, state: Id) -> bool {
        find!(
            at: IntervalValue,
            pattern!(self.facts, [{ state @ metadata::created_at: ?at }])
        )
        .next()
        .is_some()
    }

    fn overtaken_by_the_stamped_era(&self, state: Id) -> bool {
        find!(
            newer: Id,
            pattern!(self.facts, [
                { state @ revolver::of: _?anchor },
                { ?newer @ revolver::of: _?anchor, metadata::created_at: _?at },
            ])
        )
        .next()
        .is_some()
    }
}

impl RegisterOrder for HeadOrder<'_> {
    fn dominated(&self, state: Id) -> bool {
        if self.stamped(state) {
            self.stated.dominated(state)
        } else {
            self.legacy.dominated(state) || self.overtaken_by_the_stamped_era(state)
        }
    }
}

pub fn head_register(space: &TribleSet) -> HeadOrder<'_> {
    HeadOrder::new(space)
}

#[derive(Default)]
struct Heads {
    by_anchor: HashMap<Id, Vec<Id>>,
}

impl Heads {
    fn build(space: &TribleSet) -> Self {
        let mut snapshots_of: HashMap<Id, Vec<Id>> = HashMap::new();
        let mut stamp: HashMap<Id, [u8; 32]> = HashMap::new();
        let mut superseded: HashSet<Id> = HashSet::new();
        rayon::scope(|scope| {
            scope.spawn(|_| {
                let observed: Vec<(Id, Id)> = find!(
                    (snapshot: Id, anchor: Id),
                    pattern!(space, [
                        { ?snapshot @ metadata::tag: &KIND_SNAPSHOT, revolver::of: ?anchor }
                    ])
                )
                .into_par_iter()
                .collect();
                for (snapshot, anchor) in observed {
                    snapshots_of.entry(anchor).or_default().push(snapshot);
                }
            });
            scope.spawn(|_| {
                stamp = find!(
                    (snapshot: Id, at: IntervalValue),
                    pattern!(space, [
                        { ?snapshot @ metadata::tag: &KIND_SNAPSHOT, metadata::created_at: ?at }
                    ])
                )
                .into_par_iter()
                .map(|(snapshot, at)| (snapshot, at.raw))
                .collect();
            });
            scope.spawn(|_| {
                superseded = find!(
                    old: Id,
                    pattern!(space, [
                        { _?newer @ metadata::tag: &KIND_SNAPSHOT, metadata::supersedes: ?old }
                    ])
                )
                .into_par_iter()
                .collect();
            });
        });
        let by_anchor = snapshots_of
            .into_par_iter()
            .map(|(anchor, snapshots)| (anchor, maximal_snapshots(&snapshots, &stamp, &superseded)))
            .collect();
        Self { by_anchor }
    }

    fn heads(&self, anchor: Id) -> &[Id] {
        self.by_anchor.get(&anchor).map_or(&[], Vec::as_slice)
    }
}

#[derive(Default)]
struct Convergence {
    heads: Heads,
    content: HashMap<Id, Id>,
}

impl Convergence {
    fn build(space: &TribleSet) -> Self {
        let (heads, content) = rayon::join(
            || Heads::build(space),
            || {
                find!(
                    (snapshot: Id, digest: Id),
                    pattern!(space, [
                        { ?snapshot @ metadata::tag: &KIND_SNAPSHOT, revolver::content: ?digest }
                    ])
                )
                .into_par_iter()
                .collect()
            },
        );
        Self { heads, content }
    }

    fn already_current(&self, anchor: Id, content: Id) -> bool {
        match self.heads.heads(anchor) {
            [head] => self.content.get(head) == Some(&content),
            _ => false,
        }
    }

    fn advance(&mut self, anchor: Id, snapshot: Id, content: Id) {
        self.heads.by_anchor.insert(anchor, vec![snapshot]);
        self.content.insert(snapshot, content);
    }
}

pub fn heads(space: &TribleSet) -> HashMap<Id, Vec<Id>> {
    Heads::build(space).by_anchor
}

fn maximal_snapshots(
    snapshots: &[Id],
    stamp: &HashMap<Id, [u8; 32]>,
    superseded: &HashSet<Id>,
) -> Vec<Id> {
    let newest = snapshots
        .iter()
        .filter_map(|snapshot| stamp.get(snapshot))
        .max();
    let mut heads: Vec<Id> = match newest {
        Some(newest) => snapshots
            .iter()
            .copied()
            .filter(|snapshot| stamp.get(snapshot) == Some(newest))
            .collect(),
        None => snapshots
            .iter()
            .copied()
            .filter(|snapshot| !superseded.contains(snapshot))
            .collect(),
    };
    heads.sort_unstable();
    heads.dedup();
    heads
}

fn converge(
    observation: &Observation,
    convergence: &mut Convergence,
    observed: IntervalValue,
) -> Option<Fragment> {
    let content_id = observation
        .content
        .root()
        .expect("intrinsic content has one root");
    if convergence.already_current(observation.anchor_id, content_id) {
        return None;
    }
    let snapshot = entity! {
        metadata::tag: &KIND_SNAPSHOT,
        revolver::of: observation.anchor_id,
        revolver::content: content_id,
        metadata::created_at: observed,
    };
    let snapshot_id = snapshot.root().expect("intrinsic snapshot has one root");
    let mut fragment = Fragment::empty();
    fragment += observation.anchor.clone();
    fragment += snapshot;
    fragment += rebase(&observation.content, snapshot_id);
    convergence.advance(observation.anchor_id, snapshot_id, content_id);
    Some(fragment)
}

fn rebase(fragment: &Fragment, entity: Id) -> Fragment {
    let root = fragment.root().expect("rebase needs a rooted fragment");
    let mut facts = TribleSet::new();
    for trible in fragment.facts().iter() {
        if trible.e() != &root {
            continue;
        }
        let moved = Trible::force(
            &entity,
            trible.a(),
            trible.v::<triblespace::core::inline::encodings::UnknownInline>(),
        );
        facts.insert(&moved);
    }
    let mut rebased = Fragment::rooted(entity, facts);
    rebased += Fragment::from_facts_and_blobs(TribleSet::new(), fragment.blobs().clone());
    rebased
}

pub const SLICE: &[&str] = &[
    "userlist",
    "l_256",
    "l_262",
    "l_262_22",
    "l_262_23",
    "l_268",
    "l_259",
    "l_259_10",
    "l_260",
    "l_260_12",
    "l_261",
    "l_261_12",
    "l_263",
    "l_263_3",
    "l_264",
    "l_264_9",
    "l_269",
    "l_269_7",
    "l_258",
    "l_270hjmqctccyqlqckq",
    "l_220",
];

const L_256_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_3",
    "f_5",
    "f_6",
    "f_11",
    "f_12",
    "f_14",
    "f_15",
    "f_16",
    "f_17",
    "f_31",
    "f_39",
    "f_44",
    "f_62",
    "f_87",
];
const L_261_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_3",
    "f_7",
    "f_8",
    "f_8_c",
    "f_9",
    "f_9_c",
    "f_10",
    "f_10_c",
    "f_16",
    "f_17",
    "f_17_c",
    "f_18",
    "f_24",
    "f_25",
    "f_28",
    "f_39",
    "f_40",
    "f_44",
    "f_45",
];
const L_261_12_COLUMNS: &[&str] = &[
    "parent",
    "itemid",
    "deactivated",
    "subtotal",
    "itemlevel",
    "numtext",
    "f_1",
    "f_3",
    "f_5",
    "f_6",
    "f_8",
    "f_8_c",
    "f_9",
    "f_9_c",
    "f_10",
    "f_11",
    "f_12",
    "f_19",
    "f_19_c",
    "f_20",
    "f_20_c",
    "f_21",
    "f_21_c",
];
const L_264_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_2",
    "f_3",
    "f_4",
    "f_4_c",
    "f_5",
    "f_10",
    "f_11",
];
const L_264_9_COLUMNS: &[&str] = &[
    "parent",
    "itemid",
    "deactivated",
    "subtotal",
    "itemlevel",
    "f_1",
    "f_1_c",
    "f_2",
    "f_3",
    "f_5",
];
const L_259_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_3",
    "f_4",
    "f_5",
    "f_6",
    "f_7",
    "f_8",
    "f_9",
    "f_11",
    "f_11_c",
    "f_12",
    "f_14",
    "f_19",
    "f_19_c",
    "f_21",
    "f_21_c",
    "f_22",
    "f_23",
    "f_23_c",
    "f_26kqpmejpupqlthxt",
];
const L_259_10_COLUMNS: &[&str] = &[
    "parent",
    "itemid",
    "deactivated",
    "subtotal",
    "itemlevel",
    "numtext",
    "f_1",
    "f_3",
    "f_5",
    "f_6",
    "f_8",
    "f_9",
    "f_11",
    "f_12",
    "f_13",
    "f_14",
    "f_14_c",
    "f_16",
    "f_16_c",
    "f_18",
    "f_19",
    "f_20",
    "f_22",
    "f_23",
    "f_24",
    "f_25",
    "f_28",
    "f_29",
    "f_31",
    "f_31_c",
    "f_32",
    "f_33",
    "f_34",
    "f_36",
    "f_36_c",
    "f_37",
    "f_37_c",
    "f_40",
    "f_41",
];
const L_260_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_3",
    "f_7",
    "f_8",
    "f_8_c",
    "f_9",
    "f_9_c",
    "f_10",
    "f_10_c",
    "f_13",
    "f_16",
    "f_17",
    "f_17_c",
    "f_18",
    "f_20",
    "f_23",
    "f_24",
    "f_25",
    "f_28",
    "f_30",
    "f_30_c",
    "f_31",
    "f_31_c",
    "f_32",
    "f_32_c",
    "f_33",
    "f_34",
    "f_35",
    "f_35_c",
    "f_36",
    "f_36_c",
    "f_38",
    "f_45tjuahssckklqthm",
    "f_46tjuacyxufskesax",
    "f_47tjuachslcffatyy",
    "f_48tjuakpxlapcpuhe",
    "f_49tjuakhjxhltqpqh",
    "f_52tjsqpmeltlcmptl",
    "f_53tjsqptqheysfcxy",
    "f_55",
    "f_56",
    "f_45xlqkqjxucqqkpyj",
];
const L_260_12_COLUMNS: &[&str] = &[
    "parent",
    "itemid",
    "deactivated",
    "subtotal",
    "itemlevel",
    "numtext",
    "f_2",
    "f_3",
    "f_4",
    "f_5",
    "f_6",
    "f_7",
    "f_8",
    "f_8_c",
    "f_9",
    "f_9_c",
    "f_10",
    "f_11",
    "f_12",
    "f_14",
    "f_16",
    "f_16_c",
    "f_17",
    "f_17_c",
    "f_18",
    "f_18_c",
    "f_19",
    "levy_amount",
    "levy_amount_c",
    "levy_rate",
];
const L_263_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_4",
    "f_4_c",
    "f_5",
    "f_6huucyypxpmjmups",
    "f_7huucyfusmfajlcc",
    "f_8huucyaclfxlulhf",
];
const L_263_3_COLUMNS: &[&str] = &[
    "parent",
    "itemid",
    "deactivated",
    "subtotal",
    "itemlevel",
    "numtext",
    "f_1",
    "f_2",
    "f_3",
    "f_4",
    "f_5",
    "f_6",
    "f_7",
    "f_8",
    "f_8_c",
    "f_9",
    "f_9_c",
    "f_10",
    "f_11",
    "f_12huucaftuhxehcyk",
    "f_15cyeekmplymhqphu",
];
const L_262_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_3",
    "f_4",
    "f_5",
    "f_7",
    "f_13",
    "f_20",
    "f_27",
    "f_27_c",
    "f_28",
    "f_28_c",
    "f_29",
    "f_35",
    "f_35_c",
    "f_36",
    "f_37",
    "f_37_c",
    "f_38",
    "f_38_c",
];
const L_262_22_COLUMNS: &[&str] = &[
    "parent",
    "itemid",
    "deactivated",
    "subtotal",
    "itemlevel",
    "numtext",
    "f_1",
    "f_5",
    "f_7",
    "f_7_c",
    "f_9",
];
const L_262_23_COLUMNS: &[&str] = &[
    "parent",
    "itemid",
    "deactivated",
    "subtotal",
    "itemlevel",
    "numtext",
    "f_2",
    "f_3",
    "f_4",
    "f_5",
    "f_9",
    "f_11",
    "f_11_c",
];
const L_268_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_3",
    "f_4",
    "f_5",
    "f_6",
    "f_7",
    "f_9",
    "f_10",
    "f_11",
    "f_13",
    "f_14",
    "f_15",
    "f_16",
    "f_17",
    "f_18",
    "f_20",
    "f_21",
    "f_22",
    "f_23",
    "f_23_c",
    "f_26aaaaaaaa",
    "f_28aaaaaaaa",
    "f_29hjmqstfcxkfmsua",
    "f_30hjmqshqlclppumj",
    "f_31ctqqxtxktkhpujs",
];
const L_269_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_3",
    "f_4",
    "f_4_c",
    "f_5",
    "f_6",
    "f_8",
    "f_9",
    "f_10",
    "f_11",
    "f_12",
    "f_13",
    "f_13_c",
    "f_16",
];
const L_269_7_COLUMNS: &[&str] = &[
    "parent",
    "itemid",
    "deactivated",
    "subtotal",
    "itemlevel",
    "numtext",
    "f_1",
    "f_2",
    "f_3",
    "f_4",
    "f_5",
];
const L_258_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_3",
    "f_4",
    "f_5",
    "f_6",
    "f_7",
    "f_11",
    "f_12",
    "f_13",
    "f_18",
    "f_20",
    "f_25",
    "f_28",
    "f_31",
    "f_39tjmpxlkkcxeeqkq",
];
const L_270HJMQCTCCYQLQCKQ_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_3hjmqjxucmlaqmmx",
    "f_4hjmqjssceashcfs",
    "f_5hjmqjlhhphxexqp",
    "f_6hjmqxpmkpxuepsy",
    "f_7hjmeqtskutlfepp",
    "f_11hjmemsejmmjmlek",
    "f_12hjmetjplkyyfmmc",
    "f_13hjmekfacqjtjphc",
];
const L_220_COLUMNS: &[&str] = &[
    "docid",
    "company",
    "doctype",
    "credate",
    "moddate",
    "trashed",
    "deactivated",
    "state",
    "f_1",
    "f_2",
    "f_3",
    "f_4",
    "f_6",
    "f_8",
    "f_9",
    "f_10culeqhshhuacpcj",
    "f_11qfysltppjmyhhxqp",
    "task_contact",
    "task_step",
];

const USERLIST_COLUMNS: &[&str] = &["userid", "name", "initials", "first_name", "last_name"];

fn columns_of(table: &str) -> Result<&'static [&'static str]> {
    Ok(match table {
        "userlist" => USERLIST_COLUMNS,
        "l_256" => L_256_COLUMNS,
        "l_261" => L_261_COLUMNS,
        "l_261_12" => L_261_12_COLUMNS,
        "l_264" => L_264_COLUMNS,
        "l_264_9" => L_264_9_COLUMNS,
        "l_259" => L_259_COLUMNS,
        "l_259_10" => L_259_10_COLUMNS,
        "l_260" => L_260_COLUMNS,
        "l_260_12" => L_260_12_COLUMNS,
        "l_263" => L_263_COLUMNS,
        "l_263_3" => L_263_3_COLUMNS,
        "l_262" => L_262_COLUMNS,
        "l_262_22" => L_262_22_COLUMNS,
        "l_262_23" => L_262_23_COLUMNS,
        "l_268" => L_268_COLUMNS,
        "l_269" => L_269_COLUMNS,
        "l_269_7" => L_269_7_COLUMNS,
        "l_258" => L_258_COLUMNS,
        "l_270hjmqctccyqlqckq" => L_270HJMQCTCCYQLQCKQ_COLUMNS,
        "l_220" => L_220_COLUMNS,
        other => bail!(
            "{other} is not part of this ingest slice ({})",
            SLICE.join(", ")
        ),
    })
}

fn parent_of(table: &str) -> Option<&'static str> {
    match table {
        "l_261_12" => Some("l_261"),
        "l_264_9" => Some("l_264"),
        "l_259_10" => Some("l_259"),
        "l_260_12" => Some("l_260"),
        "l_262_22" => Some("l_262"),
        "l_262_23" => Some("l_262"),
        "l_263_3" => Some("l_263"),
        "l_269_7" => Some("l_269"),
        _ => None,
    }
}

fn identity_columns(table: &str) -> &'static [&'static str] {
    match (table, parent_of(table)) {
        ("userlist", _) => &["userid"],
        (_, None) => &["company", "docid"],
        (_, Some(_)) => &["parent", "itemid"],
    }
}

const OPTIONAL_COLUMNS: [&str; 5] = ["levy_amount", "levy_amount_c", "levy_rate", "task_contact", "task_step"];

/// A mapping from an optional logical field to an operator-supplied PostgreSQL column.
#[derive(Clone, Debug)]
pub struct FieldBinding {
    field: String,
    column: String,
}

impl std::str::FromStr for FieldBinding {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let (field, column) = text.split_once('=').context("expected FIELD=COLUMN")?;
        if !OPTIONAL_COLUMNS.contains(&field) {
            bail!("unknown optional field {field:?} (expected one of {})", OPTIONAL_COLUMNS.join(", "));
        }
        if !column.starts_with("f_") || column.len() < 3 || column.len() > 63
            || !column.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            bail!("expected a Revolver column name beginning with f_ and containing lowercase ASCII letters, digits or underscores");
        }
        Ok(Self { field: field.into(), column: column.into() })
    }
}

fn validate_bindings(bindings: &[FieldBinding]) -> Result<()> {
    let mut seen = HashSet::new();
    for binding in bindings {
        if !seen.insert(binding.field.as_str()) {
            bail!("multiple bindings for {}", binding.field);
        }
    }
    if seen.contains("levy_amount") != seen.contains("levy_amount_c") {
        bail!("bind levy_amount and levy_amount_c together so the source currency selector is observed");
    }
    Ok(())
}

fn require_observed_binding<S: triblespace::core::inline::InlineEncoding>(
    facts: &TribleSet,
    heads: &Heads,
    options: &Options,
    table: &str,
    name: &str,
    field: &triblespace::core::attribute::Attribute<S>,
) -> Result<()> {
    if !options.tables.iter().any(|selected| selected == table)
        || options.field_bindings.iter().any(|binding| binding.field == name)
    {
        return Ok(());
    }
    let observed = find!((anchor: Id, snapshot: Id), pattern!(facts, [
        { ?snapshot @ revolver::of: ?anchor, field: _?value }
    ]))
    .any(|(anchor, snapshot)| {
        if !heads.heads(anchor).contains(&snapshot) {
            return false;
        }
        match options.mandant {
            None => true,
            Some(tenant) if table == "l_260_12" => exists!(pattern!(facts, [
                { anchor @ purchase_line::document: _?document },
                { _?document @ revolver::mandant: tenant },
            ])),
            Some(tenant) => exists!(pattern!(facts, [{ anchor @ revolver::mandant: tenant }])),
        }
    });
    if observed {
        bail!("{table} already has current {name} values for this selection; supply --field {name}=COLUMN for live reads");
    }
    Ok(())
}

fn selection(table: &str, include_removed: bool, mandant: Option<i128>, bindings: &[FieldBinding]) -> Result<String> {
    validate_bindings(bindings)?;
    let columns = columns_of(table)?;
    let alias = match parent_of(table) {
        None => "t",
        Some(_) => "s",
    };
    let selected = columns
        .iter()
        .map(|&column| {
            if !OPTIONAL_COLUMNS.contains(&column) {
                return format!("{alias}.{column}");
            }
            match bindings.iter().find(|binding| binding.field == column) {
                Some(binding) => format!("{alias}.{} AS {column}", binding.column),
                None => format!("NULL AS {column}"),
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut filters = Vec::new();
    match parent_of(table) {
        None if table == "userlist" => filters.push("t.type = 'user'".to_owned()),
        None => {
            if !include_removed {
                filters.push("t.trashed = false AND t.deactivated = false".to_owned());
            }
            if let Some(mandant) = mandant {
                filters.push(format!("t.company = {mandant}"));
            }
        }
        Some(parent) => {
            let mut parent_filters = vec!["p.docid = s.parent".to_owned()];
            if !include_removed {
                parent_filters.push("p.trashed = false AND p.deactivated = false".to_owned());
            }
            if let Some(mandant) = mandant {
                parent_filters.push(format!("p.company = {mandant}"));
            }
            filters.push(format!(
                "EXISTS (SELECT 1 FROM {parent} p WHERE {})",
                parent_filters.join(" AND ")
            ));
        }
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filters.join(" AND "))
    };
    Ok(match parent_of(table) {
        None => format!("SELECT {selected} FROM {table} t{where_clause}"),
        Some(_) => format!("SELECT {selected} FROM {table} s{where_clause}"),
    })
}

pub fn statement(table: &str, include_removed: bool, mandant: Option<i128>) -> Result<String> {
    statement_with_fields(table, include_removed, mandant, &[])
}

pub fn statement_with_fields(table: &str, include_removed: bool, mandant: Option<i128>, bindings: &[FieldBinding]) -> Result<String> {
    let body = selection(table, include_removed, mandant, bindings)?;
    let order = match (table, parent_of(table)) {
        ("userlist", _) => "t.userid",
        (_, None) => "t.company, t.docid",
        (_, Some(_)) => "s.parent, s.itemid",
    };
    Ok(format!("COPY ({body} ORDER BY {order}) TO STDOUT;\n"))
}

pub fn count_statement(
    table: &str,
    include_removed: bool,
    mandant: Option<i128>,
) -> Result<String> {
    count_statement_with_fields(table, include_removed, mandant, &[])
}

pub fn count_statement_with_fields(
    table: &str, include_removed: bool, mandant: Option<i128>, bindings: &[FieldBinding],
) -> Result<String> {
    let body = selection(table, include_removed, mandant, bindings)?;
    let alias = match parent_of(table) {
        None => "t",
        Some(_) => "s",
    };
    let identity = identity_columns(table)
        .iter()
        .map(|column| format!("{alias}.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "COPY (SELECT count(*), count(DISTINCT ({identity})) FROM ({body}) {alias}) TO STDOUT;\n"
    ))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableOutcome {
    pub table: String,
    pub rows: usize,
    pub records: usize,
    pub changed: usize,
}

impl TableOutcome {
    pub fn collapsed(&self) -> usize {
        self.rows.saturating_sub(self.records)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IngestReport {
    pub tables: Vec<TableOutcome>,
    pub commits: usize,
    pub dry_run: bool,
}

impl IngestReport {
    pub fn rows(&self) -> usize {
        self.tables.iter().map(|table| table.rows).sum()
    }

    pub fn records(&self) -> usize {
        self.tables.iter().map(|table| table.records).sum()
    }

    pub fn changed(&self) -> usize {
        self.tables.iter().map(|table| table.changed).sum()
    }

    pub fn collapsed(&self) -> usize {
        self.tables.iter().map(TableOutcome::collapsed).sum()
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub field_bindings: Vec<FieldBinding>,
    pub tables: Vec<String>,
    pub include_removed: bool,
    pub mandant: Option<i128>,
    pub currency: String,
    pub currency_map: BTreeMap<String, String>,
    pub batch: usize,
    pub dry_run: bool,
    pub observed: Option<IntervalValue>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            tables: SLICE.iter().map(|table| (*table).to_owned()).collect(),
            field_bindings: Vec::new(),
            include_removed: false,
            mandant: None,
            currency: "EUR".to_owned(),
            currency_map: BTreeMap::from([("64".to_owned(), "EUR".to_owned())]),
            batch: 20_000,
            dry_run: false,
            observed: None,
        }
    }
}

impl Options {
    fn currency_for(&self, selector: Option<&str>, table: &str, key: &str) -> Result<&str> {
        match selector {
            None | Some("0") => Ok(&self.currency),
            Some(code) => self
                .currency_map
                .get(code)
                .map(String::as_str)
                .ok_or_else(|| {
                    anyhow!(
                        "{table} {key}: currency selector {code} has no ISO-4217 mapping. No \
                         first-party source in this product maps it — there is no currency table \
                         in the schema and no currency list in AppDef — so it cannot be guessed. \
                         Pass --currency-code {code}=<ISO> once someone establishes what it means."
                    )
                }),
        }
    }

    fn require_euro(
        &self,
        selector: Option<&str>,
        table: &str,
        column: &str,
        key: &str,
    ) -> Result<()> {
        let code = self.currency_for(selector, table, key)?;
        if code != EURO_CODE {
            bail!(
                "{table}.{column} {key}: this amount is denominated in {code}, and every monetary \
                 attribute in this collection is Currency<Euro>. Writing it anyway would book \
                 {code} as euros. Model the module's {code} amounts as their own attributes first."
            );
        }
        Ok(())
    }
}

#[derive(Default)]
struct Run {
    vocabulary: Vocabulary,
    anchors: HashMap<(&'static str, i128), Id>,
    mandant_of: HashMap<(&'static str, i128), i128>,
}

impl Run {
    fn anchor(&mut self, kind: Id, table: &'static str, mandant: i128, docid: i128) -> Id {
        *self
            .anchors
            .entry((table, docid))
            .or_insert_with(|| record_anchor(kind, mandant, docid).1)
    }

    fn remember(&mut self, table: &'static str, docid: i128, mandant: i128, anchor: Id) {
        self.anchors.insert((table, docid), anchor);
        self.mandant_of.insert((table, docid), mandant);
    }

    fn parent(
        &self,
        parent_table: &'static str,
        docid: i128,
        table: &str,
        key: &str,
    ) -> Result<(Id, i128)> {
        let anchor = self.anchors.get(&(parent_table, docid)).copied();
        let mandant = self.mandant_of.get(&(parent_table, docid)).copied();
        match (anchor, mandant) {
            (Some(anchor), Some(mandant)) => Ok((anchor, mandant)),
            _ => Err(anyhow!(
                "{table} {key}: its {parent_table} record {docid} was not ingested in this run"
            )),
        }
    }
}

struct Group {
    observation: Observation,
    rows: i128,
}

impl Group {
    fn new(observation: Observation) -> Self {
        Self {
            observation,
            rows: 1,
        }
    }

    fn absorb(&mut self, other: Observation, table: &str, key: &str) -> Result<()> {
        let created = record::created.id();
        if fields_of(&self.observation.content, Some(created))
            != fields_of(&other.content, Some(created))
        {
            bail!(
                "{table} {key}: two rows resolve to one identity and disagree about what the \
                 record says. `docid` is not unique in this module, and these two rows are not \
                 copies of one record — merging them would silently lose one, so the ingest \
                 stops instead. Resolve the duplicate in the source, or extend the identity."
            );
        }
        let mine = field_value(&self.observation.content, created);
        let theirs = field_value(&other.content, created);
        let take_other = match (mine, theirs) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(mine), Some(theirs)) => theirs < mine,
        };
        if take_other {
            self.observation = other;
        }
        self.rows += 1;
        Ok(())
    }

    fn finish(mut self) -> Observation {
        if self.rows > 1 {
            let value = inlineencodings::I256BE::encode(self.rows).raw;
            self.observation.content =
                with_extra_fact(&self.observation.content, record::source_rows.id(), value);
        }
        self.observation
    }
}

fn fields_of(content: &Fragment, without: Option<Id>) -> BTreeSet<(Id, [u8; 32])> {
    let root = content.root().expect("intrinsic content has one root");
    content
        .facts()
        .iter()
        .filter(|trible| trible.e() == &root)
        .filter(|trible| Some(*trible.a()) != without)
        .map(|trible| {
            let value: [u8; 32] = trible.v::<triblespace::core::inline::encodings::UnknownInline>().raw;
            (*trible.a(), value)
        })
        .collect()
}

fn field_value(content: &Fragment, attribute: Id) -> Option<[u8; 32]> {
    let root = content.root().expect("intrinsic content has one root");
    content
        .facts()
        .iter()
        .find(|trible| trible.e() == &root && trible.a() == &attribute)
        .map(|trible| trible.v::<triblespace::core::inline::encodings::UnknownInline>().raw)
}

fn with_extra_fact(content: &Fragment, attribute: Id, value: [u8; 32]) -> Fragment {
    let mut rows: Vec<IntrinsicEntityRow> = fields_of(content, None)
        .into_iter()
        .map(|(attribute, value)| IntrinsicEntityRow::new(attribute, value))
        .collect();
    rows.push(IntrinsicEntityRow::new(attribute, value));
    let (root, facts) = build_intrinsic_entity(rows);
    let mut fragment = Fragment::rooted(root, facts);
    fragment += Fragment::from_facts_and_blobs(TribleSet::new(), content.blobs().clone());
    fragment
}

fn import_stamp() -> Result<IntervalValue> {
    let now = Epoch::now().map_err(|error| anyhow!("read the system clock: {error}"))?;
    (now, now)
        .try_to_inline()
        .map_err(|error| anyhow!("the current instant is not representable: {error:?}"))
}

pub fn ingest(
    pile_path: &Path,
    key_path: Option<&Path>,
    handle: CollectionHandle,
    source: &Source,
    options: &Options,
) -> Result<IngestReport> {
    validate_bindings(&options.field_bindings)?;
    for table in &options.tables {
        columns_of(table)?;
    }
    let stamp = match options.observed {
        Some(observed) => observed,
        None => import_stamp()?,
    };
    let signer = load_signer(pile_path, key_path)?;
    let mut pile = open_pile_strict(pile_path)?;
    let reader = pile.snapshot().context("freeze ERP pile observation")?;
    let collection = open_collection(&reader, handle, &signer, true)?;
    let outcome = (|| {
        let current: TribleSet = reader.collection(collection)?.view()
            .context("materialize the Revolver collection")?;
        let mut convergence = Convergence::build(&current);
        if !matches!(source, Source::Files { .. }) {
            require_observed_binding(&current, &convergence.heads, options, "l_260_12", "levy_amount", &purchase_line::levy_amount)?;
            require_observed_binding(&current, &convergence.heads, options, "l_260_12", "levy_rate", &purchase_line::levy_rate)?;
            require_observed_binding(&current, &convergence.heads, options, "l_220", "task_contact", &task::contact)?;
            require_observed_binding(&current, &convergence.heads, options, "l_220", "task_step", &task::step_record)?;
        }
        let mut run = Run {
            vocabulary: Vocabulary::seeded(&current),
            ..Run::default()
        };
        let mut report = IngestReport {
            dry_run: options.dry_run,
            ..IngestReport::default()
        };
        let mut pending = Fragment::empty();
        let mut pending_facts = 0usize;

        for table in SLICE {
            if !options.tables.iter().any(|wanted| wanted == table) {
                continue;
            }
            let columns = columns_of(table)?;
            let body = source.fetch(
                table,
                &statement_with_fields(table, options.include_removed, options.mandant, &options.field_bindings)?,
            )?;
            let rows = decode_copy(&body, columns.len(), table)?;
            let mut outcome = TableOutcome {
                table: (*table).to_owned(),
                rows: rows.len(),
                records: 0,
                changed: 0,
            };
            let mut settled: HashSet<Id> = HashSet::with_capacity(rows.len());
            let mut group: Option<Group> = None;
            for index in 0..=rows.len() {
                let observed = match rows.get(index) {
                    Some(values) => Some(observe(table, columns, values, options, &mut run)?),
                    None => None,
                };
                let continues = match (&group, &observed) {
                    (Some(group), Some(next)) => group.observation.anchor_id == next.anchor_id,
                    _ => false,
                };
                if !continues {
                    if let Some(finished) = group.take() {
                        let observation = finished.finish();
                        if !settled.insert(observation.anchor_id) {
                            bail!(
                                "{table}: rows resolving to one identity were not adjacent, so \
                                 the source did not honour the ingest statement's ordering"
                            );
                        }
                        outcome.records += 1;
                        if let Some(fragment) = converge(&observation, &mut convergence, stamp) {
                            outcome.changed += 1;
                            pending_facts += fragment.facts().len();
                            pending += fragment;
                            if pending_facts >= options.batch && !options.dry_run {
                                pending += run.vocabulary.take();
                                debug_assert!(!pending.facts().is_empty());
                                pile
                                    .commit(collection, &signer, std::mem::take(&mut pending))
                                    .context("publish a Revolver ingest batch")?;
                                report.commits += 1;
                                pending_facts = 0;
                            }
                        }
                    }
                }
                match observed {
                    None => break,
                    Some(observation) => match &mut group {
                        Some(group) => {
                            let key = observation.key.clone();
                            group.absorb(observation, table, &key)?;
                        }
                        None => group = Some(Group::new(observation)),
                    },
                }
            }
            report.tables.push(outcome);
        }

        pending += run.vocabulary.take();
        if !options.dry_run && !pending.facts().is_empty() {
            pile
                .commit(collection, &signer, pending)
                .context("publish the final Revolver ingest batch")?;
            report.commits += 1;
        }
        Ok(report)
    })();
    finish(pile, outcome)
}

fn finish<T>(pile: Pile, result: Result<T>) -> Result<T> {
    match (result, pile.close()) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(anyhow!("close Revolver pile: {error}")),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(close)) => {
            Err(error.context(format!("closing the Revolver pile also failed: {close}")))
        }
    }
}

fn observe(
    table: &'static str,
    columns: &'static [&'static str],
    values: &[Option<String>],
    options: &Options,
    run: &mut Run,
) -> Result<Observation> {
    let key = match parent_of(table) {
        None => values.first().cloned().flatten().unwrap_or_default(),
        Some(_) => format!(
            "{}/{}",
            values.first().cloned().flatten().unwrap_or_default(),
            values.get(1).cloned().flatten().unwrap_or_default()
        ),
    };
    let row = Row {
        table,
        key,
        columns,
        values,
    };
    match table {
        "userlist" => observe_user(&row),
        "l_256" => observe_party(&row, run),
        "l_261" => observe_sales_document(&row, options, run),
        "l_261_12" => observe_sales_line(&row, options, run),
        "l_264" => observe_payment(&row, options, run),
        "l_264_9" => observe_allocation(&row, options, run),
        "l_259" => observe_project(&row, options, run),
        "l_259_10" => observe_project_step(&row, options, run),
        "l_260" => observe_purchase_document(&row, options, run),
        "l_260_12" => observe_purchase_line(&row, options, run),
        "l_263" => observe_time_entry(&row, options, run),
        "l_263_3" => observe_booking(&row, options, run),
        "l_262" => observe_article(&row, options, run),
        "l_262_22" => observe_supply(&row, options, run),
        "l_262_23" => observe_article_price(&row, options, run),
        "l_268" => observe_employee(&row, options, run),
        "l_269" => observe_opportunity(&row, options, run),
        "l_269_7" => observe_opportunity_step(&row, run),
        "l_258" => observe_activity(&row, run),
        "l_270hjmqctccyqlqckq" => observe_request(&row, run),
        "l_220" => observe_task(&row, run),
        other => bail!("no ingest rule for {other}"),
    }
}

fn observe_party(row: &Row<'_>, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_PARTY, mandant, docid);
    run.remember("l_256", docid, mandant, anchor_id);

    let employer = row
        .reference("f_11")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let master = row
        .reference("f_87")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let role = run
        .vocabulary
        .code_opt(DOMAIN_ADDRESS_TYPE, row.raw("f_12")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_1")?.map(|text| blobs.put(text));
    let company_name = row.text("f_3")?.map(|text| blobs.put(text));
    let last_name = row.text("f_5")?.map(|text| blobs.put(text));
    let first_name = row.text("f_6")?.map(|text| blobs.put(text));
    let email = row.text("f_31")?.map(|text| blobs.put(text));
    let street = row.text("f_14")?.map(|text| blobs.put(text));
    let city = row.text("f_17")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        party::company_name?: company_name,
        party::last_name?: last_name,
        party::first_name?: first_name,
        party::email?: email,
        party::street?: street,
        party::city?: city,
        party::postal_code?: row.short("f_16")?,
        party::country?: row.short("f_15")?,
        party::vat_id?: row.short("f_44")?,
        party::number?: row.short("f_62")?,
        party::employer?: employer,
        party::master_address?: master,
        party::account_owner?: row.reference("f_39")?,
        party::role?: role,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_sales_document(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_SALES_DOCUMENT, mandant, docid);
    run.remember("l_261", docid, mandant, anchor_id);

    let selector = row.raw("f_7")?.map(|code| code.to_owned());

    let customer = row
        .reference("f_1")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let attention = row
        .reference("f_16")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let project = row
        .reference("f_28")?
        .map(|target| run.anchor(KIND_PROJECT, "l_259", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let currency_code = run
        .vocabulary
        .code_opt(DOMAIN_CURRENCY, selector.as_deref())?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;

    let mut blobs = Fragment::empty();
    let title = row.text("f_44")?.map(|text| blobs.put(text));
    let reference = row.text("f_18")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: title,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        sales::number?: row.short("f_3")?,
        sales::date?: row.date("f_2")?,
        sales::due_date?: row.date("f_25")?,
        sales::delivery_date?: row.date("f_45")?,
        sales::customer?: customer,
        sales::attention?: attention,
        sales::currency?: currency_code,
        sales::customer_reference?: reference,
        sales::net_total?: row.money("f_8", options)?,
        sales::tax_total?: row.money("f_9", options)?,
        sales::gross_total?: row.money("f_10", options)?,
        sales::outstanding?: row.money("f_17", options)?,
        sales::tax_rate_full?: row.proportion("f_39")?,
        sales::tax_rate_reduced?: row.proportion("f_40")?,
        sales::project?: project,
        sales::query_to?: row.reference("f_24")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_sales_line(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let parent = row.required_integer("parent")?;
    let itemid = row.required_integer("itemid")?;
    let (document, _) = run.parent("l_261", parent, "l_261_12", &row.key)?;
    let (anchor, anchor_id) = sales_line_anchor(document, itemid);

    let position_type = run
        .vocabulary
        .code_opt(DOMAIN_POSITION_TYPE, row.raw("f_1")?)?;
    let tax_class = run
        .vocabulary
        .code_opt(DOMAIN_SALES_TAX_CLASS, row.raw("f_11")?)?;

    let mut blobs = Fragment::empty();
    let article = row.text("f_3")?.map(|text| blobs.put(text));
    let description = row.text("f_12")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: article,
        line::description?: description,
        line::position_type?: position_type,
        line::tax_class?: tax_class,
        line::line_number?: row.short("numtext")?,
        line::unit?: row.short("f_6")?,
        line::quantity?: row.rational("f_5")?,
        line::discount?: row.proportion("f_10")?,
        line::unit_price?: row.money("f_8", options)?,
        line::total?: row.money("f_9", options)?,
        line::net_full?: row.money("f_19", options)?,
        line::net_reduced?: row.money("f_20", options)?,
        line::net_free?: row.money("f_21", options)?,
        record::deactivated?: row.flag("deactivated")?,
        record::subtotal?: row.flag("subtotal")?,
        record::item_level?: row.nonzero("itemlevel")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_payment(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_PAYMENT, mandant, docid);
    run.remember("l_264", docid, mandant, anchor_id);

    let counterparty = row
        .reference("f_10")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let mode = run
        .vocabulary
        .code_opt(DOMAIN_PAYMENT_MODE, row.raw("f_5")?)?;

    let mut blobs = Fragment::empty();
    let memo = row.text("f_3")?.map(|text| blobs.put(text));
    let receipt = row.text("f_11")?.map(|text| blobs.put(text));
    let mut content = entity! {
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        payment::date?: row.date("f_2")?,
        payment::amount?: row.money("f_4", options)?,
        payment::memo?: memo,
        payment::receipt_number?: receipt,
        payment::counterparty?: counterparty,
        payment::mode?: mode,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_allocation(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let parent = row.required_integer("parent")?;
    let itemid = row.required_integer("itemid")?;
    let (payment_anchor, mandant) = run.parent("l_264", parent, "l_264_9", &row.key)?;
    let (anchor, anchor_id) = allocation_anchor(payment_anchor, itemid);

    let sales_document = row
        .reference("f_2")?
        .map(|target| run.anchor(KIND_SALES_DOCUMENT, "l_261", mandant, target));
    let purchase_document = row
        .reference("f_5")?
        .map(|target| run.anchor(KIND_PURCHASE_DOCUMENT, "l_260", mandant, target));

    let mut blobs = Fragment::empty();
    let note = row.text("f_3")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::description?: note,
        allocation::amount?: row.money("f_1", options)?,
        allocation::sales_document?: sales_document,
        allocation::purchase_document?: purchase_document,
        record::deactivated?: row.flag("deactivated")?,
        record::subtotal?: row.flag("subtotal")?,
        record::item_level?: row.nonzero("itemlevel")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_project(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_PROJECT, mandant, docid);
    run.remember("l_259", docid, mandant, anchor_id);

    let parent_project = row
        .reference("f_2")?
        .map(|target| run.anchor(KIND_PROJECT, "l_259", mandant, target));
    let customer = row
        .reference("f_3")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let contact = row
        .reference("f_8")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let status = run
        .vocabulary
        .code_opt(DOMAIN_PROJECT_STATUS, row.raw("f_4")?)?;
    let division = run
        .vocabulary
        .code_opt(DOMAIN_PROJECT_DIVISION, row.raw("f_26kqpmejpupqlthxt")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_1")?.map(|text| blobs.put(text));
    let description = row.text("f_7")?.map(|text| blobs.put(text));
    let reference = row.text("f_14")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        project::parent_project?: parent_project,
        project::customer?: customer,
        project::contact?: contact,
        project::responsible?: row.reference("f_5")?,
        project::number?: row.short("f_9")?,
        project::date?: row.date("f_12")?,
        project::delivery_date?: row.date("f_6")?,
        project::budget?: row.money("f_11", options)?,
        project::status?: status,
        project::description?: description,
        project::customer_reference?: reference,
        project::purchase_total?: row.money("f_19", options)?,
        project::margin_total?: row.money("f_21", options)?,
        project::margin_rate?: row.proportion("f_22")?,
        project::sales_total?: row.money("f_23", options)?,
        project::division?: division,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_project_step(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let parent = row.required_integer("parent")?;
    let itemid = row.required_integer("itemid")?;
    let (project, mandant) = run.parent("l_259", parent, "l_259_10", &row.key)?;
    let (anchor, anchor_id) = project_step_anchor(project, itemid);

    let service = row
        .reference("f_11")?
        .map(|target| run.anchor(KIND_ARTICLE, "l_262", mandant, target));
    let price_tier = row
        .reference("f_22")?
        .map(|target| run.anchor(KIND_ARTICLE, "l_262", mandant, target));
    let supplier = row
        .reference("f_18")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let billed_in = row
        .reference("f_29")?
        .map(|target| run.anchor(KIND_SALES_DOCUMENT, "l_261", mandant, target));
    let unit = run
        .vocabulary
        .code_opt(DOMAIN_STEP_UNIT, row.raw("f_23")?)?;
    let tax_class = run
        .vocabulary
        .code_opt(DOMAIN_STEP_TAX_CLASS, row.raw("f_25")?)?;
    let billing_mode = run
        .vocabulary
        .code_opt(DOMAIN_BILLING_MODE, row.raw("f_28")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_1")?.map(|text| blobs.put(text));
    let notes = row.text("f_8")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::deactivated?: row.flag("deactivated")?,
        record::subtotal?: row.flag("subtotal")?,
        record::item_level?: row.nonzero("itemlevel")?,
        step::project: project,
        step::line_number?: row.short("numtext")?,
        step::employee?: row.reference("f_3")?,
        step::start_date?: row.date("f_5")?,
        step::end_date?: row.date("f_6")?,
        step::notes?: notes,
        step::done?: row.flag("f_9")?,
        step::service?: service,
        step::quantity_planned?: row.rational("f_12")?,
        step::quantity_actual?: row.rational("f_13")?,
        step::purchase_total?: row.money("f_14", options)?,
        step::purchase_unit_price?: row.money("f_16", options)?,
        step::supplier?: supplier,
        step::start_time?: row.time_of_day("f_19")?,
        step::end_time?: row.time_of_day("f_20")?,
        step::price_tier?: price_tier,
        step::unit?: unit,
        step::discount?: row.proportion("f_24")?,
        step::tax_class?: tax_class,
        step::billing_mode?: billing_mode,
        step::billed_in?: billed_in,
        step::margin?: row.money("f_31", options)?,
        step::margin_rate?: row.proportion("f_32")?,
        step::outsourced?: row.flag("f_33")?,
        step::billed?: row.flag("f_34")?,
        step::sales_unit_price?: row.money("f_36", options)?,
        step::sales_total?: row.money("f_37", options)?,
        step::quantity_remaining?: row.rational("f_40")?,
        step::remaining_rate?: row.proportion("f_41")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_purchase_document(
    row: &Row<'_>,
    options: &Options,
    run: &mut Run,
) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_PURCHASE_DOCUMENT, mandant, docid);
    run.remember("l_260", docid, mandant, anchor_id);

    let supplier = row
        .reference("f_1")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let attention = row
        .reference("f_16")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let currency = run.vocabulary.code_opt(DOMAIN_CURRENCY, row.raw("f_7")?)?;
    let delivery_method = run
        .vocabulary
        .code_opt(DOMAIN_DELIVERY_METHOD, row.raw("f_20")?)?;
    let division = run
        .vocabulary
        .code_opt(DOMAIN_PURCHASE_DIVISION, row.raw("f_45xlqkqjxucqqkpyj")?)?;

    let mut blobs = Fragment::empty();
    let address = row.text("f_13")?.map(|text| blobs.put(text));
    let external = row.text("f_18")?.map(|text| blobs.put(text));
    let name = row.text("f_3")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        purchase::supplier?: supplier,
        purchase::attention?: attention,
        purchase::date?: row.date("f_2")?,
        purchase::number?: row.short("f_3")?,
        purchase::net_total?: row.money("f_8", options)?,
        purchase::tax_total?: row.money("f_9", options)?,
        purchase::gross_total?: row.money("f_10", options)?,
        purchase::address?: address,
        purchase::outstanding?: row.money("f_17", options)?,
        purchase::external_number?: external,
        purchase::delivery_method?: delivery_method,
        purchase::due_date?: row.date("f_25")?,
        purchase::cash_purchase?: row.flag("f_28")?,
        purchase::net_full?: row.money("f_30", options)?,
        purchase::net_reduced?: row.money("f_31", options)?,
        purchase::net_free?: row.money("f_32", options)?,
        purchase::tax_rate_full?: row.proportion("f_33")?,
        purchase::tax_rate_reduced?: row.proportion("f_34")?,
        purchase::tax_full?: row.money("f_35", options)?,
        purchase::tax_reduced?: row.money("f_36", options)?,
        purchase::is_delivery_note?: row.flag("f_38")?,
        purchase::payment_terms_days?: row.rational("f_45tjuahssckklqthm")?,
        purchase::discount_1_days?: row.rational("f_46tjuacyxufskesax")?,
        purchase::discount_2_days?: row.rational("f_47tjuachslcffatyy")?,
        purchase::discount_1_rate?: row.proportion("f_48tjuakpxlapcpuhe")?,
        purchase::discount_2_rate?: row.proportion("f_49tjuakhjxhltqpqh")?,
        purchase::discount_1_due?: row.date("f_52tjsqpmeltlcmptl")?,
        purchase::discount_2_due?: row.date("f_53tjsqptqheysfcxy")?,
        purchase::currency?: currency,
        purchase::foreign_currency?: row.flag("f_55")?,
        purchase::entered_in_foreign?: row.flag("f_56")?,
        purchase::division?: division,
        purchase::query_to?: row.reference("f_24")?,
        purchase::print_query?: row.flag("f_23")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_purchase_line(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let parent = row.required_integer("parent")?;
    let itemid = row.required_integer("itemid")?;
    let (document, mandant) = run.parent("l_260", parent, "l_260_12", &row.key)?;
    let (anchor, anchor_id) = purchase_line_anchor(document, itemid);

    let article = row
        .reference("f_2")?
        .map(|target| run.anchor(KIND_ARTICLE, "l_262", mandant, target));
    let project = row
        .reference("f_4")?
        .map(|target| run.anchor(KIND_PROJECT, "l_259", mandant, target));
    let price_selection = run
        .vocabulary
        .code_opt(DOMAIN_PRICE_SELECTION, row.raw("f_7")?)?;
    let tax_class = run
        .vocabulary
        .code_opt(DOMAIN_PURCHASE_TAX_CLASS, row.raw("f_11")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_3")?.map(|text| blobs.put(text));
    let description = row.text("f_12")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::deactivated?: row.flag("deactivated")?,
        record::subtotal?: row.flag("subtotal")?,
        record::item_level?: row.nonzero("itemlevel")?,
        purchase_line::document: document,
        purchase_line::line_number?: row.short("numtext")?,
        purchase_line::article?: article,
        purchase_line::project?: project,
        purchase_line::quantity?: row.rational("f_5")?,
        purchase_line::unit?: row.short("f_6")?,
        purchase_line::price_selection?: price_selection,
        purchase_line::unit_price?: row.money("f_8", options)?,
        purchase_line::total?: row.money("f_9", options)?,
        purchase_line::discount?: row.proportion("f_10")?,
        purchase_line::tax_class?: tax_class,
        purchase_line::description?: description,
        purchase_line::quantity_delivered?: row.rational("f_14")?,
        purchase_line::net_full?: row.money("f_16", options)?,
        purchase_line::net_reduced?: row.money("f_17", options)?,
        purchase_line::net_free?: row.money("f_18", options)?,
        purchase_line::step_record?: row.reference("f_19")?,
        purchase_line::levy_amount?: row.money("levy_amount", options)?,
        purchase_line::levy_rate?: row.proportion("levy_rate")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_time_entry(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_TIME_ENTRY, mandant, docid);
    run.remember("l_263", docid, mandant, anchor_id);

    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let content = entity! {
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        time::employee?: row.reference("f_1")?,
        time::date?: row.date("f_2")?,
        time::hourly_rate?: row.money("f_4", options)?,
        time::hours?: row.rational("f_5")?,
        time::work_start?: row.time_of_day("f_6huucyypxpmjmups")?,
        time::work_end?: row.time_of_day("f_7huucyfusmfajlcc")?,
        time::attendance?: row.rational("f_8huucyaclfxlulhf")?,
    };
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_booking(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let parent = row.required_integer("parent")?;
    let itemid = row.required_integer("itemid")?;
    let (entry, mandant) = run.parent("l_263", parent, "l_263_3", &row.key)?;
    let (anchor, anchor_id) = booking_anchor(entry, itemid);

    let project = row
        .reference("f_3")?
        .map(|target| run.anchor(KIND_PROJECT, "l_259", mandant, target));
    let activity = row
        .reference("f_4")?
        .map(|target| run.anchor(KIND_ARTICLE, "l_262", mandant, target));
    let customer = row
        .reference("f_12huucaftuhxehcyk")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));

    let mut blobs = Fragment::empty();
    let note = row.text("f_5")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::description?: note,
        record::deactivated?: row.flag("deactivated")?,
        record::subtotal?: row.flag("subtotal")?,
        record::item_level?: row.nonzero("itemlevel")?,
        booking::entry: entry,
        booking::line_number?: row.short("numtext")?,
        booking::start?: row.time_of_day("f_1")?,
        booking::end?: row.time_of_day("f_2")?,
        booking::project?: project,
        booking::activity?: activity,
        booking::quantity?: row.rational("f_6")?,
        booking::billable?: row.flag("f_7")?,
        booking::cost?: row.money("f_8", options)?,
        booking::unit_price?: row.money("f_9", options)?,
        booking::step_record?: row.reference("f_10")?,
        booking::duration?: row.rational("f_11")?,
        booking::customer?: customer,
        booking::extra_effort?: row.flag("f_15cyeekmplymhqphu")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_article(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_ARTICLE, mandant, docid);
    run.remember("l_262", docid, mandant, anchor_id);

    let group = row
        .reference("f_4")?
        .map(|target| run.anchor(KIND_ARTICLE, "l_262", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let category = run
        .vocabulary
        .code_opt(DOMAIN_ARTICLE_CATEGORY, row.raw("f_3")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_1")?.map(|text| blobs.put(text));
    let description = row.text("f_7")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        article::code?: row.short("f_2")?,
        article::category?: category,
        article::group?: group,
        article::unit?: row.short("f_5")?,
        article::description?: description,
        article::price_list_a?: row.flag("f_13")?,
        article::purchase_price?: row.money("f_27", options)?,
        article::sales_price?: row.money("f_28", options)?,
        article::packaging_unit?: row.rational("f_29")?,
        article::margin?: row.money("f_35", options)?,
        article::margin_rate?: row.proportion("f_36")?,
        article::purchase_price_gross?: row.money("f_37", options)?,
        article::sales_price_gross?: row.money("f_38", options)?,
        article::stock?: row.rational("f_20")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_supply(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let parent = row.required_integer("parent")?;
    let itemid = row.required_integer("itemid")?;
    let (article, mandant) = run.parent("l_262", parent, "l_262_22", &row.key)?;
    let (anchor, anchor_id) = supply_anchor(article, itemid);

    let supplier = row
        .reference("f_1")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let tax_class = run
        .vocabulary
        .code_opt(DOMAIN_ARTICLE_TAX_CLASS, row.raw("f_9")?)?;
    let content = entity! {
        record::deactivated?: row.flag("deactivated")?,
        record::subtotal?: row.flag("subtotal")?,
        record::item_level?: row.nonzero("itemlevel")?,
        supply::article: article,
        supply::supplier?: supplier,
        supply::packaging_unit?: row.rational("f_5")?,
        supply::unit_price?: row.money("f_7", options)?,
        supply::tax_class?: tax_class,
    };
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_article_price(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let parent = row.required_integer("parent")?;
    let itemid = row.required_integer("itemid")?;
    let (article, mandant) = run.parent("l_262", parent, "l_262_23", &row.key)?;
    let (anchor, anchor_id) = article_price_anchor(article, itemid);

    let customer = row
        .reference("f_4")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let category = run
        .vocabulary
        .code_opt(DOMAIN_PRICE_CATEGORY, row.raw("f_3")?)?;
    let tax_class = run
        .vocabulary
        .code_opt(DOMAIN_ARTICLE_TAX_CLASS, row.raw("f_9")?)?;
    let content = entity! {
        record::deactivated?: row.flag("deactivated")?,
        record::subtotal?: row.flag("subtotal")?,
        record::item_level?: row.nonzero("itemlevel")?,
        price::article: article,
        price::packaging_unit?: row.rational("f_2")?,
        price::category?: category,
        price::customer?: customer,
        price::label?: row.short("f_5")?,
        price::tax_class?: tax_class,
        price::net_price?: row.money("f_11", options)?,
    };
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_user(row: &Row<'_>) -> Result<Observation> {
    let userid = row.required_integer("userid")?;
    let (anchor, anchor_id) = user_anchor(userid);

    let mut blobs = Fragment::empty();
    let first_name = row.text("first_name")?.map(|text| blobs.put(text));
    let last_name = row.text("last_name")?.map(|text| blobs.put(text));
    let name = row.text("name")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        user::first_name?: first_name,
        user::last_name?: last_name,
        user::initials?: row.short("initials")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn reference_label(text: &str) -> &str {
    match text.find('\u{10}') {
        Some(marker) => text[..marker].trim_end().trim_end_matches('(').trim_end(),
        None => text,
    }
}

fn observe_employee(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_EMPLOYEE, mandant, docid);
    run.remember("l_268", docid, mandant, anchor_id);

    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let status = run
        .vocabulary
        .code_opt(DOMAIN_EMPLOYEE_STATUS, row.raw("f_13")?)?;
    let marital_status = run
        .vocabulary
        .code_opt(DOMAIN_EMPLOYEE_MARITAL_STATUS, row.raw("f_6")?)?;
    let tax_class = run
        .vocabulary
        .code_opt(DOMAIN_EMPLOYEE_TAX_CLASS, row.raw("f_7")?)?;

    let mut blobs = Fragment::empty();
    let first_name = row.text("f_20")?.map(|text| blobs.put(text));
    let last_name = row.text("f_21")?.map(|text| blobs.put(text));
    let title = row
        .text("f_22")?
        .map(|text| reference_label(&text).to_owned())
        .filter(|label| !label.is_empty())
        .map(|label| blobs.put(label));
    let street = row.text("f_2")?.map(|text| blobs.put(text));
    let role = row.text("f_29hjmqstfcxkfmsua")?.map(|text| blobs.put(text));
    let department = row.text("f_30hjmqshqlclppumj")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: title,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        employee::user?: row.reference("f_1")?,
        employee::first_name?: first_name,
        employee::last_name?: last_name,
        employee::salutation?: row.short("f_18")?,
        employee::status?: status,
        employee::hired?: row.date("f_15")?,
        employee::left?: row.date("f_16")?,
        employee::role?: role,
        employee::department?: department,
        employee::weekly_hours?: row.rational("f_31ctqqxtxktkhpujs")?,
        employee::annual_leave?: row.rational("f_26aaaaaaaa")?,
        employee::leave_entitlement?: row.rational("f_28aaaaaaaa")?,
        employee::street?: street,
        employee::postal_code?: row.short("f_4")?,
        employee::city?: row.short("f_5")?,
        employee::country?: row.short("f_3")?,
        employee::phone?: row.short("f_9")?,
        employee::mobile?: row.short("f_10")?,
        employee::religion?: row.short("f_11")?,
        employee::health_insurer?: row.short("f_17")?,
        employee::social_security_number?: row.short("f_14")?,
        employee::marital_status?: marital_status,
        employee::tax_class?: tax_class,
        employee::hourly_rate?: row.money("f_23", options)?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_opportunity(row: &Row<'_>, options: &Options, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_OPPORTUNITY, mandant, docid);
    run.remember("l_269", docid, mandant, anchor_id);

    let company = row
        .reference("f_1")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let contact = row
        .reference("f_2")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let status = run
        .vocabulary
        .code_opt(DOMAIN_OPPORTUNITY_STATUS, row.raw("f_8")?)?;
    let source = run
        .vocabulary
        .code_opt(DOMAIN_OPPORTUNITY_SOURCE, row.raw("f_12")?)?;
    let next_step_type = run
        .vocabulary
        .code_opt(DOMAIN_NEXT_STEP_TYPE, row.raw("f_16")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_3")?.map(|text| blobs.put(text));
    let description = row.text("f_6")?.map(|text| blobs.put(text));
    let next_step = row.text("f_9")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        opportunity::company?: company,
        opportunity::contact?: contact,
        opportunity::volume?: row.money("f_4", options)?,
        opportunity::probability?: row.proportion("f_5")?,
        opportunity::description?: description,
        opportunity::status?: status,
        opportunity::next_step?: next_step,
        opportunity::next_step_date?: row.date("f_10")?,
        opportunity::expected_close?: row.date("f_11")?,
        opportunity::source?: source,
        opportunity::weighted_volume?: row.money("f_13", options)?,
        opportunity::next_step_type?: next_step_type,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_opportunity_step(row: &Row<'_>, run: &mut Run) -> Result<Observation> {
    let parent = row.required_integer("parent")?;
    let itemid = row.required_integer("itemid")?;
    let (opportunity, _) = run.parent("l_269", parent, "l_269_7", &row.key)?;
    let (anchor, anchor_id) = opportunity_step_anchor(opportunity, itemid);

    let category = run
        .vocabulary
        .code_opt(DOMAIN_OPPORTUNITY_STEP_CATEGORY, row.raw("f_3")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_1")?.map(|text| blobs.put(text));
    let notes = row.text("f_2")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::deactivated?: row.flag("deactivated")?,
        record::subtotal?: row.flag("subtotal")?,
        record::item_level?: row.nonzero("itemlevel")?,
        opportunity_step::opportunity: opportunity,
        opportunity_step::line_number?: row.short("numtext")?,
        opportunity_step::notes?: notes,
        opportunity_step::category?: category,
        opportunity_step::date?: row.date("f_4")?,
        opportunity_step::done?: row.flag("f_5")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_activity(row: &Row<'_>, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_ACTIVITY, mandant, docid);
    run.remember("l_258", docid, mandant, anchor_id);

    let party = row
        .reference("f_2")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let company = row
        .reference("f_25")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let project = row
        .reference("f_3")?
        .map(|target| run.anchor(KIND_PROJECT, "l_259", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let category = run
        .vocabulary
        .code_opt(DOMAIN_ACTIVITY_CATEGORY, row.raw("f_5")?)?;
    let status = run
        .vocabulary
        .code_opt(DOMAIN_ACTIVITY_STATUS, row.raw("f_31")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_4")?.map(|text| blobs.put(text));
    let text = row.text("f_18")?.map(|text| blobs.put(text));
    let address = row.text("f_20")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        activity::party?: party,
        activity::project?: project,
        activity::category?: category,
        activity::date_from?: row.date("f_1")?,
        activity::date_to?: row.date("f_39tjmpxlkkcxeeqkq")?,
        activity::time_from?: row.time_of_day("f_6")?,
        activity::time_to?: row.time_of_day("f_7")?,
        activity::text?: text,
        activity::address?: address,
        activity::query_to?: row.reference("f_13")?,
        activity::company?: company,
        activity::status?: status,
        activity::include_project?: row.flag("f_11")?,
        activity::include_external_reference?: row.flag("f_12")?,
        activity::print_query?: row.flag("f_28")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_request(row: &Row<'_>, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_HR_REQUEST, mandant, docid);
    run.remember("l_270hjmqctccyqlqckq", docid, mandant, anchor_id);

    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let document_type = run.vocabulary.code_opt(
        DOMAIN_REQUEST_DOCUMENT_TYPE,
        row.raw("f_11hjmemsejmmjmlek")?,
    )?;

    let mut blobs = Fragment::empty();
    let notes = row.text("f_6hjmqxpmkpxuepsy")?.map(|text| blobs.put(text));
    let mut content = entity! {
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        request::first_day?: row.date("f_3hjmqjxucmlaqmmx")?,
        request::last_day?: row.date("f_4hjmqjssceashcfs")?,
        request::days?: row.rational("f_5hjmqjlhhphxexqp")?,
        request::notes?: notes,
        request::unpaid?: row.flag("f_7hjmeqtskutlfepp")?,
        request::document_type?: document_type,
        request::applied?: row.date("f_12hjmetjplkyyfmmc")?,
        request::employee?: row.reference("f_13hjmekfacqjtjphc")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

fn observe_task(row: &Row<'_>, run: &mut Run) -> Result<Observation> {
    let mandant = row.required_integer("company")?;
    let docid = row.required_integer("docid")?;
    let (anchor, anchor_id) = record_anchor(KIND_TASK, mandant, docid);
    run.remember("l_220", docid, mandant, anchor_id);

    let project = row
        .reference("f_2")?
        .map(|target| run.anchor(KIND_PROJECT, "l_259", mandant, target));
    let parent_task = row
        .reference("f_3")?
        .map(|target| run.anchor(KIND_TASK, "l_220", mandant, target));
    let customer = row
        .reference("f_11qfysltppjmyhhxqp")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let contact = row
        .reference("task_contact")?
        .map(|target| run.anchor(KIND_PARTY, "l_256", mandant, target));
    let doctype = run
        .vocabulary
        .code_opt(DOMAIN_DOCTYPE, row.raw("doctype")?)?;
    let state = run
        .vocabulary
        .code_opt(DOMAIN_RECORD_STATE, row.raw("state")?)?;
    let category = run
        .vocabulary
        .code_opt(DOMAIN_TASK_CATEGORY, row.raw("f_10culeqhshhuacpcj")?)?;

    let mut blobs = Fragment::empty();
    let name = row.text("f_1")?.map(|text| blobs.put(text));
    let link = row.text("f_4")?.map(|text| blobs.put(text));
    let note = row.text("f_9")?.map(|text| blobs.put(text));
    let mut content = entity! {
        metadata::name?: name,
        record::doctype?: doctype,
        record::created?: row.date("credate")?,
        record::modified?: row.date("moddate")?,
        record::trashed?: row.flag("trashed")?,
        record::deactivated?: row.flag("deactivated")?,
        record::state?: state,
        task::project?: project,
        task::parent_task?: parent_task,
        task::link?: link,
        task::due_date?: row.date("f_6")?,
        task::followup_date?: row.date("f_8")?,
        task::note?: note,
        task::category?: category,
        task::customer?: customer,
        task::contact?: contact,
        task::step_record?: row.reference("task_step")?,
    };
    content += blobs;
    Ok(Observation::new(row, anchor, anchor_id, content))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Census {
    pub by_table: BTreeMap<&'static str, usize>,
    pub codes: usize,
    pub snapshots: usize,
    pub forked: usize,
    pub folded: usize,
}

impl Census {
    pub fn records(&self) -> usize {
        self.by_table.values().sum()
    }

    pub fn table(&self, table: &str) -> usize {
        self.by_table.get(table).copied().unwrap_or_default()
    }
}

pub fn kind_of(table: &str) -> Result<Id> {
    Ok(match table {
        "userlist" => KIND_USER,
        "l_256" => KIND_PARTY,
        "l_258" => KIND_ACTIVITY,
        "l_259" => KIND_PROJECT,
        "l_259_10" => KIND_PROJECT_STEP,
        "l_260" => KIND_PURCHASE_DOCUMENT,
        "l_260_12" => KIND_PURCHASE_LINE,
        "l_261" => KIND_SALES_DOCUMENT,
        "l_261_12" => KIND_SALES_LINE,
        "l_262" => KIND_ARTICLE,
        "l_262_22" => KIND_ARTICLE_SUPPLY,
        "l_262_23" => KIND_ARTICLE_PRICE,
        "l_263" => KIND_TIME_ENTRY,
        "l_263_3" => KIND_TIME_LINE,
        "l_264" => KIND_PAYMENT,
        "l_264_9" => KIND_PAYMENT_ALLOCATION,
        "l_268" => KIND_EMPLOYEE,
        "l_269" => KIND_OPPORTUNITY,
        "l_269_7" => KIND_OPPORTUNITY_STEP,
        "l_270hjmqctccyqlqckq" => KIND_HR_REQUEST,
        "l_220" => KIND_TASK,
        other => bail!("{other} is not part of this ingest slice"),
    })
}

pub fn ids_of_kind(space: &TribleSet, kind: Id) -> BTreeSet<Id> {
    find!(id: Id, pattern!(space, [{ ?id @ metadata::tag: &kind }])).collect()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    pub materialize: std::time::Duration,
    pub heads: std::time::Duration,
    pub counts: std::time::Duration,
}

pub fn census_timed(space: &TribleSet) -> (Census, Timings) {
    let mut timings = Timings::default();
    let started = std::time::Instant::now();
    let heads = Heads::build(space);
    timings.heads = started.elapsed();
    let started = std::time::Instant::now();
    let census = census_from(space, &heads);
    timings.counts = started.elapsed();
    (census, timings)
}

pub fn census(space: &TribleSet) -> Census {
    let heads = Heads::build(space);
    census_from(space, &heads)
}

fn census_from(space: &TribleSet, heads: &Heads) -> Census {
    let by_table = SLICE
        .iter()
        .map(|table| {
            let kind = kind_of(table).expect("every slice table declares its kind");
            (*table, ids_of_kind(space, kind).len())
        })
        .collect();
    let multiple: HashSet<Id> = find!(
        snapshot: Id,
        pattern!(space, [{ ?snapshot @ record::source_rows: _?rows }])
    )
    .collect();
    let folded = heads
        .by_anchor
        .values()
        .filter(|heads| match heads.as_slice() {
            [head] => multiple.contains(head),
            _ => false,
        })
        .count();
    Census {
        by_table,
        codes: ids_of_kind(space, KIND_CODE).len(),
        snapshots: ids_of_kind(space, KIND_SNAPSHOT).len(),
        forked: heads
            .by_anchor
            .values()
            .filter(|heads| heads.len() > 1)
            .count(),
        folded,
    }
}

pub fn head_of(space: &TribleSet, anchor: Id) -> Option<Id> {
    let order = head_register(space);
    let candidates: Vec<Id> = find!(
        snapshot: Id,
        pattern!(space, [{ ?snapshot @ metadata::tag: &KIND_SNAPSHOT, revolver::of: &anchor }])
    )
    .collect();
    sole(&order, candidates).sole()
}

fn read_snapshot_timed(
    pile_path: &Path,
    key_path: Option<&Path>,
    handle: CollectionHandle,
) -> Result<(Snapshot, std::time::Duration)> {
    let signer = load_signer(pile_path, key_path)?;
    let mut pile = open_pile_strict(pile_path)?;
    let reader = pile.snapshot().context("freeze ERP pile observation")?;
    let collection = open_collection(&reader, handle, &signer, false)?;
    let started = std::time::Instant::now();
    let result = reader.collection(collection)
        .context("observe the Revolver collection")
        .and_then(|view| view.view::<TribleSet>().context("materialize the Revolver collection"))
        .map(|facts| (Snapshot { facts, reader }, started.elapsed()));
    finish(pile, result)
}

pub fn read_snapshot(pile_path: &Path, key_path: Option<&Path>, handle: CollectionHandle) -> Result<Snapshot> {
    read_snapshot_timed(pile_path, key_path, handle).map(|(snapshot, _)| snapshot)
}

pub fn read_timed(
    pile_path: &Path,
    key_path: Option<&Path>,
    handle: CollectionHandle,
) -> Result<(TribleSet, std::time::Duration)> {
    read_snapshot_timed(pile_path, key_path, handle).map(|(snapshot, elapsed)| (snapshot.facts, elapsed))
}

#[derive(Debug)]
pub struct GroupReport {
    pub group: Option<Id>,
    pub superseded: Vec<Id>,
    pub resolved: Vec<(String, Id)>,
    pub facts: usize,
}

#[derive(Clone, Debug)]
pub struct CorporateGroup {
    pub id: Id,
    pub members: Vec<Id>,
    pub names: Vec<TextHandle>,
    pub supersedes: Vec<Id>,
}

pub fn group_fragment(members: &[Id], supersedes: &[Id]) -> (Fragment, Id) {
    let mut members: Vec<Id> = members.to_vec();
    members.sort_unstable();
    members.dedup();
    let mut supersedes: Vec<Id> = supersedes.to_vec();
    supersedes.sort_unstable();
    supersedes.dedup();
    let fragment = entity! {
        metadata::tag: &KIND_CORPORATE_GROUP,
        group::member*: members.iter(),
        metadata::supersedes*: supersedes.iter(),
    };
    let id = fragment.root().expect("intrinsic group has one root");
    (fragment, id)
}

pub fn all_groups(space: &TribleSet) -> HashMap<Id, CorporateGroup> {
    let mut groups: HashMap<Id, CorporateGroup> = find!(
        id: Id,
        pattern!(space, [{ ?id @ metadata::tag: &KIND_CORPORATE_GROUP }])
    )
    .map(|id| {
        (
            id,
            CorporateGroup {
                id,
                members: Vec::new(),
                names: Vec::new(),
                supersedes: Vec::new(),
            },
        )
    })
    .collect();
    for (id, member) in find!(
        (id: Id, member: Id),
        pattern!(space, [
            { ?id @ metadata::tag: &KIND_CORPORATE_GROUP, group::member: ?member }
        ])
    ) {
        if let Some(group) = groups.get_mut(&id) {
            group.members.push(member);
        }
    }
    for (id, old) in find!(
        (id: Id, old: Id),
        pattern!(space, [
            { ?id @ metadata::tag: &KIND_CORPORATE_GROUP, metadata::supersedes: ?old }
        ])
    ) {
        if let Some(group) = groups.get_mut(&id) {
            group.supersedes.push(old);
        }
    }
    for (id, name) in find!(
        (id: Id, name: TextHandle),
        pattern!(space, [
            { ?id @ metadata::tag: &KIND_CORPORATE_GROUP, metadata::name: ?name }
        ])
    ) {
        if let Some(group) = groups.get_mut(&id) {
            group.names.push(name);
        }
    }
    for group in groups.values_mut() {
        group.members.sort_unstable();
        group.members.dedup();
        group.supersedes.sort_unstable();
        group.supersedes.dedup();
        group.names.sort_unstable();
        group.names.dedup();
    }
    groups
}

pub fn group_heads(space: &TribleSet) -> Vec<CorporateGroup> {
    let mut groups = all_groups(space);
    let mut heads: Vec<CorporateGroup> = latest(
        space,
        metadata::supersedes.id(),
        groups.keys().copied().collect::<Vec<_>>(),
    )
    .into_iter()
    .filter_map(|id| groups.remove(&id))
    .collect();
    heads.sort_by_key(|group| group.id);
    heads
}

fn parties_numbered(space: &TribleSet, number: &str) -> Vec<Id> {
    find!(
        anchor: Id,
        pattern!(space, [
            { ?anchor @ metadata::tag: &KIND_PARTY },
            { _?snapshot @ metadata::tag: &KIND_SNAPSHOT, revolver::of: ?anchor,
              party::number: number }
        ])
    )
    .collect()
}

pub fn declare_group(
    pile_path: &Path,
    key_path: Option<&Path>,
    handle: CollectionHandle,
    name: Option<&str>,
    numbers: &[String],
) -> Result<GroupReport> {
    if numbers.is_empty() {
        bail!("a corporate group needs at least one member");
    }
    let signer = load_signer(pile_path, key_path)?;
    let mut pile = open_pile_strict(pile_path)?;
    let reader = pile.snapshot().context("freeze ERP pile observation")?;
    let collection = open_collection(&reader, handle, &signer, true)?;
    let outcome = (|| {
        let space: TribleSet = reader.collection(collection)?.view()
            .context("materialize the Revolver collection")?;

        let mut resolved = Vec::new();
        let mut unknown = Vec::new();
        let mut ambiguous = Vec::new();
        for number in numbers {
            let matches = parties_numbered(&space, number);
            match matches.len() {
                0 => unknown.push(number.clone()),
                1 => resolved.push((number.clone(), matches[0])),
                many => ambiguous.push((number.clone(), many)),
            }
        }
        if !unknown.is_empty() || !ambiguous.is_empty() {
            bail!(
                "refusing the declaration: unresolved customer number(s) {unknown:?}{}",
                if ambiguous.is_empty() {
                    String::new()
                } else {
                    format!("; {ambiguous:?} matched several party records")
                }
            );
        }

        let mut members: Vec<Id> = resolved.iter().map(|(_, id)| *id).collect();
        members.sort_unstable();
        members.dedup();

        let heads = group_heads(&space);
        let touched: Vec<&CorporateGroup> = heads
            .iter()
            .filter(|group| group.members.iter().any(|member| members.contains(member)))
            .collect();

        let unchanged = touched
            .iter()
            .find(|group| group.members == members)
            .filter(|_| touched.len() == 1)
            .map(|group| group.id);
        if let Some(group) = unchanged {
            let Some(name) = name else {
                return Ok(GroupReport {
                    group: None,
                    superseded: Vec::new(),
                    resolved,
                    facts: 0,
                });
            };
            let held: Vec<String> = touched[0]
                .names
                .iter()
                .filter_map(|handle| read_text(&reader, *handle).ok())
                .collect();
            if held.iter().any(|name_held| name_held == name) {
                return Ok(GroupReport {
                    group: None,
                    superseded: Vec::new(),
                    resolved,
                    facts: 0,
                });
            }
            let mut fragment = Fragment::empty();
            let handle: TextHandle = fragment.put(name.to_owned());
            fragment += entity! { ExclusiveId::force_ref(&group) @ metadata::name: handle };
            let facts = fragment.facts().len();
            pile
                .commit(collection, &signer, fragment)
                .context("annotate the corporate group with a name")?;
            return Ok(GroupReport {
                group: Some(group),
                superseded: Vec::new(),
                resolved,
                facts,
            });
        }

        let supersedes: Vec<Id> = touched.iter().map(|group| group.id).collect();
        let (core, id) = group_fragment(&members, &supersedes);
        let mut fragment = core;
        if let Some(name) = name {
            let handle: TextHandle = fragment.put(name.to_owned());
            fragment += entity! { ExclusiveId::force_ref(&id) @ metadata::name: handle };
        }
        let facts = fragment.facts().len();
        pile
            .commit(collection, &signer, fragment)
            .context("publish the corporate group")?;
        Ok(GroupReport {
            group: Some(id),
            superseded: supersedes,
            resolved,
            facts,
        })
    })();
    finish(pile, outcome)
}

pub fn read_text(
    reader: &triblespace::core::repo::pile::PileSnapshot,
    handle: TextHandle,
) -> Result<String> {
    use anybytes::View;
    use triblespace::core::repo::BlobStoreGet;

    let value: View<str> = reader.get(handle)?;
    Ok(value.to_string())
}

pub fn parties_like(
    space: &TribleSet,
    reader: &triblespace::core::repo::pile::PileSnapshot,
    needle: &str,
) -> Vec<(Id, String, String)> {
    let needle = needle.to_lowercase();
    let mut out: Vec<(Id, String, String)> = find!(
        (anchor: Id, number: String, handle: TextHandle),
        pattern!(space, [
            { ?anchor @ metadata::tag: &KIND_PARTY },
            { _?snapshot @ metadata::tag: &KIND_SNAPSHOT, revolver::of: ?anchor,
              party::number: ?number, party::company_name: ?handle }
        ])
    )
    .filter_map(|(anchor, number, handle)| {
        let name = read_text(reader, handle).ok()?;
        let name = name.trim().to_owned();
        name.to_lowercase()
            .contains(&needle)
            .then_some((anchor, number, name))
    })
    .collect();
    out.sort_by(|left, right| left.2.cmp(&right.2).then(left.1.cmp(&right.1)));
    out.dedup();
    out
}

pub fn read(pile_path: &Path, key_path: Option<&Path>, handle: CollectionHandle) -> Result<TribleSet> {
    read_snapshot(pile_path, key_path, handle).map(|snapshot| snapshot.facts)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::access::initialize_signer;
    use triblespace::core::inline::TryFromInline;

    use super::*;

    fn columns(table: &str) -> usize {
        columns_of(table).unwrap().len()
    }

    fn copy_line(fields: &[&str]) -> String {
        format!("{}\n", fields.join("\t"))
    }

    fn row_for(table: &str, values: &[(&str, &str)]) -> String {
        let fields: Vec<&str> = columns_of(table)
            .unwrap()
            .iter()
            .map(|column| {
                values
                    .iter()
                    .find(|(name, _)| name == column)
                    .map_or("\\N", |(_, value)| *value)
            })
            .collect();
        copy_line(&fields)
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        pile: PathBuf,
        key: PathBuf,
        snapshot: PathBuf,
        collection: CollectionHandle,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let pile = directory.path().join("revolver.pile");
            let key = directory.path().join("revolver.key");
            let snapshot = directory.path().join("snapshot");
            fs::File::create(&pile).unwrap();
            fs::create_dir_all(&snapshot).unwrap();
            initialize_signer(&pile, Some(&key)).unwrap();
            let collection = crate::access::initialize_collection(&pile, Some(&key)).unwrap();
            for table in SLICE {
                fs::write(snapshot.join(format!("{table}.copy")), "").unwrap();
            }
            Self {
                _directory: directory,
                pile,
                key,
                snapshot,
                collection,
            }
        }

        fn table(&self, table: &str, body: &str) -> &Self {
            fs::write(self.snapshot.join(format!("{table}.copy")), body).unwrap();
            self
        }

        fn source(&self) -> Source {
            Source::Files {
                directory: self.snapshot.clone(),
            }
        }

        fn ingest(&self, options: &Options) -> IngestReport {
            ingest(&self.pile, Some(&self.key), self.collection, &self.source(), options).unwrap()
        }

        fn facts(&self) -> TribleSet {
            read(&self.pile, Some(&self.key), self.collection).unwrap()
        }
    }

    fn snapshot_v1(fixture: &Fixture, gross: &str) {
        fixture
            .table(
                "l_256",
                &[
                    row_for(
                        "l_256",
                        &[
                            ("docid", "1"),
                            ("company", "1"),
                            ("doctype", "257"),
                            ("credate", "2020-01-02 09:00:00"),
                            ("moddate", "2020-01-02 09:00:00"),
                            ("trashed", "f"),
                            ("deactivated", "f"),
                            ("f_1", "Acme GmbH"),
                            ("f_3", "Acme GmbH"),
                            ("f_11", "0"),
                            ("f_12", "1"),
                            ("f_14", "Example Lane 1"),
                            ("f_15", "DE"),
                            ("f_16", "00000"),
                            ("f_17", "Exampleville"),
                            ("f_31", "kontakt@example.invalid"),
                            ("f_44", "TEST-TAX-01"),
                            ("f_62", "A00001"),
                            ("f_87", "0"),
                        ],
                    ),
                    row_for(
                        "l_256",
                        &[
                            ("docid", "2"),
                            ("company", "1"),
                            ("doctype", "258"),
                            ("credate", "2020-01-03 09:00:00"),
                            ("moddate", "2020-01-03 09:00:00"),
                            ("trashed", "f"),
                            ("deactivated", "f"),
                            ("f_1", "Muster, Erika"),
                            ("f_3", "Acme GmbH"),
                            ("f_5", "Muster"),
                            ("f_6", "Erika"),
                            ("f_11", "1"),
                            ("f_12", "1"),
                            ("f_31", "erika@example.invalid"),
                            ("f_87", "1"),
                        ],
                    ),
                ]
                .concat(),
            )
            .table(
                "l_261",
                &row_for(
                    "l_261",
                    &[
                        ("docid", "10"),
                        ("company", "1"),
                        ("doctype", "285"),
                        ("credate", "2024-03-01 08:00:00"),
                        ("moddate", "2024-03-01 08:00:00"),
                        ("trashed", "f"),
                        ("deactivated", "f"),
                        ("state", "4"),
                        ("f_1", "1"),
                        ("f_2", "2024-03-01"),
                        ("f_3", "RE24001"),
                        ("f_7", "64"),
                        ("f_8", "100000"),
                        ("f_8_c", "64"),
                        ("f_9", "19000"),
                        ("f_9_c", "64"),
                        ("f_10", gross),
                        ("f_10_c", "64"),
                        ("f_16", "2"),
                        ("f_17", "0"),
                        ("f_17_c", "64"),
                        ("f_18", "PO-77"),
                        ("f_25", "2024-03-31"),
                        ("f_28", "0"),
                        ("f_39", "19"),
                        ("f_40", "7"),
                        ("f_44", "Beratung März"),
                        ("f_45", "0002-11-30"),
                    ],
                ),
            )
            .table(
                "l_261_12",
                &[
                    row_for(
                        "l_261_12",
                        &[
                            ("parent", "10"),
                            ("itemid", "1"),
                            ("deactivated", "f"),
                            ("numtext", "1"),
                            ("f_1", "0"),
                            ("f_3", "Konzept"),
                            ("f_5", "2"),
                            ("f_6", "Std."),
                            ("f_8", "40000"),
                            ("f_8_c", "64"),
                            ("f_9", "80000"),
                            ("f_9_c", "64"),
                            ("f_10", "0"),
                            ("f_11", "2"),
                            ("f_12", "Konzeptarbeit"),
                            ("f_19", "80000"),
                            ("f_19_c", "64"),
                        ],
                    ),
                    row_for(
                        "l_261_12",
                        &[
                            ("parent", "10"),
                            ("itemid", "2"),
                            ("deactivated", "f"),
                            ("numtext", "2"),
                            ("f_1", "0"),
                            ("f_3", "Reinzeichnung"),
                            ("f_5", "1"),
                            ("f_6", "Std."),
                            ("f_8", "20000"),
                            ("f_8_c", "64"),
                            ("f_9", "20000"),
                            ("f_9_c", "64"),
                            ("f_10", "0"),
                            ("f_11", "2"),
                            ("f_19", "20000"),
                            ("f_19_c", "64"),
                        ],
                    ),
                ]
                .concat(),
            )
            .table(
                "l_264",
                &payment_row("20", "2024-04-02 10:00:00", gross, "Überweisung"),
            )
            .table(
                "l_264_9",
                &row_for(
                    "l_264_9",
                    &[
                        ("parent", "20"),
                        ("itemid", "1"),
                        ("deactivated", "f"),
                        ("f_1", gross),
                        ("f_1_c", "64"),
                        ("f_2", "10"),
                        ("f_5", "0"),
                    ],
                ),
            );
    }

    fn payment_row(docid: &str, created: &str, amount: &str, memo: &str) -> String {
        row_for(
            "l_264",
            &[
                ("docid", docid),
                ("company", "1"),
                ("doctype", "299"),
                ("credate", created),
                ("moddate", created),
                ("trashed", "f"),
                ("deactivated", "f"),
                ("f_2", "2024-04-02"),
                ("f_3", memo),
                ("f_4", amount),
                ("f_4_c", "64"),
                ("f_5", "1"),
                ("f_10", "1"),
                ("f_11", "BEL-1"),
            ],
        )
    }

    fn at(second: u32) -> IntervalValue {
        parse_timestamp(&format!("2026-01-01 00:00:{second:02}"))
            .unwrap()
            .unwrap()
    }

    fn options() -> Options {
        options_at(1)
    }

    fn options_at(second: u32) -> Options {
        Options {
            batch: 1,
            observed: Some(at(second)),
            ..Options::default()
        }
    }

    fn snapshot_modules(fixture: &Fixture, actual_quantity: &str) {
        fixture
            .table(
                "l_262",
                &row_for(
                    "l_262",
                    &[
                        ("docid", "30"),
                        ("company", "1"),
                        ("doctype", "291"),
                        ("credate", "2024-01-02 09:00:00"),
                        ("moddate", "2024-01-02 09:00:00"),
                        ("trashed", "f"),
                        ("deactivated", "f"),
                        ("f_1", "Konzeption"),
                        ("f_2", "LST-1"),
                        ("f_5", "Std."),
                        ("f_28", "95000"),
                        ("f_28_c", "64"),
                    ],
                ),
            )
            .table(
                "l_259",
                &row_for(
                    "l_259",
                    &[
                        ("docid", "40"),
                        ("company", "1"),
                        ("doctype", "266"),
                        ("credate", "2024-02-01 09:00:00"),
                        ("moddate", "2024-02-01 09:00:00"),
                        ("trashed", "f"),
                        ("deactivated", "f"),
                        ("f_1", "Relaunch"),
                        ("f_3", "1"),
                        ("f_8", "2"),
                        ("f_5", "7"),
                        ("f_9", "P-2024-1"),
                        ("f_12", "2024-02-01"),
                        ("f_11", "500000"),
                        ("f_11_c", "64"),
                        ("f_4", "5"),
                        ("f_22", "33.5"),
                        ("f_23", "480000"),
                        ("f_23_c", "64"),
                    ],
                ),
            )
            .table(
                "l_259_10",
                &row_for(
                    "l_259_10",
                    &[
                        ("parent", "40"),
                        ("itemid", "1"),
                        ("deactivated", "f"),
                        ("numtext", "1"),
                        ("f_1", "Konzept"),
                        ("f_3", "7"),
                        ("f_5", "2024-02-05"),
                        ("f_6", "2024-02-09"),
                        ("f_9", "t"),
                        ("f_11", "30"),
                        ("f_12", "8"),
                        ("f_13", actual_quantity),
                        ("f_19", "09:00:00"),
                        ("f_20", "17:30:00"),
                        ("f_24", "12.5"),
                        ("f_29", "10"),
                        ("f_36", "95000"),
                        ("f_36_c", "64"),
                        ("f_37", "760000"),
                        ("f_37_c", "64"),
                    ],
                ),
            )
            .table("l_260", &purchase_row("50", "2024-03-05 08:00:00"))
            .table(
                "l_260_12",
                &row_for(
                    "l_260_12",
                    &[
                        ("parent", "50"),
                        ("itemid", "1"),
                        ("deactivated", "f"),
                        ("numtext", "1"),
                        ("f_2", "30"),
                        ("f_3", "Fremdleistung"),
                        ("f_4", "40"),
                        ("f_5", "1"),
                        ("f_6", "Stk."),
                        ("f_8", "100000"),
                        ("f_8_c", "64"),
                        ("f_9", "100000"),
                        ("f_9_c", "64"),
                        ("f_11", "2"),
                    ],
                ),
            )
            .table(
                "l_263",
                &row_for(
                    "l_263",
                    &[
                        ("docid", "60"),
                        ("company", "1"),
                        ("doctype", "293"),
                        ("credate", "2024-02-05 18:00:00"),
                        ("moddate", "2024-02-05 18:00:00"),
                        ("trashed", "f"),
                        ("deactivated", "f"),
                        ("f_1", "7"),
                        ("f_2", "2024-02-05"),
                        ("f_4", "95000"),
                        ("f_4_c", "64"),
                        ("f_5", "8.5"),
                        ("f_6huucyypxpmjmups", "08:30:00"),
                        ("f_7huucyfusmfajlcc", "17:00:00"),
                        ("f_8huucyaclfxlulhf", "8.5"),
                    ],
                ),
            )
            .table(
                "l_263_3",
                &row_for(
                    "l_263_3",
                    &[
                        ("parent", "60"),
                        ("itemid", "1"),
                        ("deactivated", "f"),
                        ("numtext", "1"),
                        ("f_1", "09:00:00"),
                        ("f_2", "12:00:00"),
                        ("f_3", "40"),
                        ("f_4", "30"),
                        ("f_5", "Konzeptrunde"),
                        ("f_6", "3"),
                        ("f_7", "t"),
                        ("f_8", "285000"),
                        ("f_8_c", "64"),
                        ("f_9", "95000"),
                        ("f_9_c", "64"),
                        ("f_11", "3"),
                        ("f_12huucaftuhxehcyk", "1"),
                    ],
                ),
            );
    }

    fn purchase_row(docid: &str, created: &str) -> String {
        row_for(
            "l_260",
            &[
                ("docid", docid),
                ("company", "1"),
                ("doctype", "275"),
                ("credate", created),
                ("moddate", "2024-03-06 08:00:00"),
                ("trashed", "f"),
                ("deactivated", "f"),
                ("f_1", "1"),
                ("f_2", "2024-03-05"),
                ("f_3", "EK-24-1"),
                ("f_7", "64"),
                ("f_8", "100000"),
                ("f_8_c", "64"),
                ("f_9", "19000"),
                ("f_9_c", "64"),
                ("f_10", "119000"),
                ("f_10_c", "64"),
                ("f_18", "EXT-0001"),
                ("f_25", "2024-04-05"),
                ("f_33", "19"),
                ("f_34", "7"),
                ("f_55", "f"),
                ("f_56", "f"),
                ("f_45tjuahssckklqthm", "30"),
                ("f_48tjuakpxlapcpuhe", "2"),
            ],
        )
    }

    #[test]
    fn a_time_of_day_is_a_duration_since_midnight_not_an_instant() {
        assert_eq!(parse_time_of_day("00:00:00").unwrap(), 0);
        assert_eq!(
            parse_time_of_day("09:30:00").unwrap(),
            9 * 3_600_000_000_000 + 30 * 60_000_000_000
        );
        assert_eq!(parse_time_of_day("00:00:00.5").unwrap(), 500_000_000);
        assert!(parse_time_of_day("25:00:00").is_err());
        assert!(parse_time_of_day("noon").is_err());

        let row = Row {
            table: "l_263",
            key: "60".to_owned(),
            columns: &["f_1", "f_2"],
            values: &[Some("00:00:00".to_owned()), Some("09:15:00".to_owned())],
        };
        assert_eq!(row.time_of_day("f_1").unwrap(), None);
        assert!(row.time_of_day("f_2").unwrap().is_some());
    }

    #[test]
    fn the_directory_names_a_user_whose_personnel_file_does_not() {
        let fixture = Fixture::new();
        fixture
            .table(
                "userlist",
                &row_for(
                    "userlist",
                    &[
                        ("userid", "2832"),
                        ("name", "Muster, Erika"),
                        ("initials", "EM"),
                        ("first_name", "Erika"),
                        ("last_name", "Muster"),
                    ],
                ),
            )
            .table(
                "l_268",
                &row_for(
                    "l_268",
                    &[
                        ("docid", "70"),
                        ("company", "1"),
                        ("doctype", "292"),
                        ("credate", "2021-04-20 09:00:00"),
                        ("moddate", "2021-04-20 09:00:00"),
                        ("trashed", "f"),
                        ("deactivated", "f"),
                        ("f_1", "2832"),
                        ("f_22", "Muster, Erika (\u{10}\u{10}256;1;1\u{11})"),
                        ("f_29hjmqstfcxkfmsua", "Berater"),
                    ],
                ),
            );

        let report = fixture.ingest(&options());
        assert_eq!(report.rows(), 2);
        let facts = fixture.facts();
        let census = census(&facts);
        assert_eq!(census.table("userlist"), 1, "the directory row is a record");
        assert_eq!(census.table("l_268"), 1);

        let expected = user_anchor(2832).1;
        let anchors = ids_of_kind(&facts, KIND_USER);
        assert_eq!(
            anchors.iter().copied().collect::<Vec<_>>(),
            vec![expected],
            "the anchor must be a pure function of the user id"
        );

        let named: Vec<Id> = find!(
            state: Id,
            pattern!(&facts, [
                { ?state @ metadata::tag: &KIND_SNAPSHOT, revolver::of: expected,
                  user::last_name: _?surname }
            ])
        )
        .collect();
        assert_eq!(named.len(), 1, "the directory state carries a surname");

        let titled = find!(
            state: Id,
            pattern!(&facts, [
                { ?state @ metadata::tag: &KIND_SNAPSHOT, metadata::name: _?title,
                  employee::user: _?who }
            ])
        )
        .count();
        assert_eq!(titled, 1, "a blank personnel file is still named by f_22");

        let again = fixture.ingest(&options_at(2));
        assert_eq!(again.changed(), 0, "the directory converges like everything else");
    }

    #[test]
    fn the_added_modules_ingest_and_then_converge() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        snapshot_modules(&fixture, "8");

        let first = fixture.ingest(&options());
        assert_eq!(first.rows(), 14, "seven core rows and seven module rows");
        assert_eq!(
            first.records(),
            14,
            "no duplicated identity in this fixture"
        );
        assert_eq!(first.changed(), 14);
        let before = fixture.facts();
        let census_before = census(&before);
        for (table, expected) in [
            ("l_262", 1),
            ("l_259", 1),
            ("l_259_10", 1),
            ("l_260", 1),
            ("l_260_12", 1),
            ("l_263", 1),
            ("l_263_3", 1),
        ] {
            assert_eq!(census_before.table(table), expected, "{table}");
        }

        let second = fixture.ingest(&options_at(2));
        assert_eq!(second.rows(), 14);
        assert_eq!(second.changed(), 0);
        assert_eq!(second.commits, 0);
        assert_eq!(fixture.facts().len(), before.len(), "not one new fact");

        snapshot_modules(&fixture, "9");
        let third = fixture.ingest(&options_at(3));
        assert_eq!(third.changed(), 1, "only the step changed");
        let after = fixture.facts();
        let census_after = census(&after);
        assert_eq!(census_after.records(), census_before.records());
        assert_eq!(census_after.snapshots, census_before.snapshots + 1);
        assert_eq!(census_after.forked, 0);
    }

    #[test]
    fn the_two_held_record_ids_are_edges_now_that_their_modules_are_modelled() {
        let fixture = Fixture::new();
        snapshot_modules(&fixture, "8");
        fixture
            .table(
                "l_261",
                &row_for(
                    "l_261",
                    &[
                        ("docid", "10"),
                        ("company", "1"),
                        ("doctype", "285"),
                        ("credate", "2024-03-01 08:00:00"),
                        ("moddate", "2024-03-01 08:00:00"),
                        ("trashed", "f"),
                        ("deactivated", "f"),
                        ("f_1", "1"),
                        ("f_2", "2024-03-01"),
                        ("f_3", "RE24001"),
                        ("f_7", "64"),
                        ("f_10", "119000"),
                        ("f_28", "40"),
                    ],
                ),
            )
            .table(
                "l_264",
                &row_for(
                    "l_264",
                    &[
                        ("docid", "20"),
                        ("company", "1"),
                        ("doctype", "300"),
                        ("credate", "2024-04-02 10:00:00"),
                        ("moddate", "2024-04-02 10:00:00"),
                        ("trashed", "f"),
                        ("deactivated", "f"),
                        ("f_2", "2024-04-02"),
                        ("f_4", "119000"),
                        ("f_10", "1"),
                    ],
                ),
            )
            .table(
                "l_264_9",
                &row_for(
                    "l_264_9",
                    &[
                        ("parent", "20"),
                        ("itemid", "1"),
                        ("deactivated", "f"),
                        ("f_1", "119000"),
                        ("f_5", "50"),
                    ],
                ),
            );
        fixture.ingest(&options());
        let facts = fixture.facts();

        let (_, project) = record_anchor(KIND_PROJECT, 1, 40);
        let (_, purchase) = record_anchor(KIND_PURCHASE_DOCUMENT, 1, 50);
        let (_, invoice) = record_anchor(KIND_SALES_DOCUMENT, 1, 10);
        let (_, settlement) = record_anchor(KIND_PAYMENT, 1, 20);

        let invoice_head = head_of(&facts, invoice).unwrap();
        let projects: Vec<Id> = find!(
            id: Id,
            pattern!(&facts, [{ invoice_head @ sales::project: ?id }])
        )
        .collect();
        assert_eq!(projects, vec![project], "f_28 reaches the project itself");

        let (_, allocation_id) = allocation_anchor(settlement, 1);
        let allocation_head = head_of(&facts, allocation_id).unwrap();
        let purchases: Vec<Id> = find!(
            id: Id,
            pattern!(&facts, [{ allocation_head @ allocation::purchase_document: ?id }])
        )
        .collect();
        assert_eq!(purchases, vec![purchase], "f_5 reaches the purchase itself");

        let (_, line) = purchase_line_anchor(purchase, 1);
        let line_head = head_of(&facts, line).unwrap();
        let line_projects: Vec<Id> = find!(
            id: Id,
            pattern!(&facts, [{ line_head @ purchase_line::project: ?id }])
        )
        .collect();
        assert_eq!(line_projects, vec![project]);
    }

    #[test]
    fn copies_of_one_record_collapse_and_say_that_they_did() {
        let fixture = Fixture::new();
        let early = purchase_row("50", "2024-03-05 08:00:00");
        let late = purchase_row("50", "2025-09-01 08:00:00");
        fixture.table("l_260", &format!("{early}{late}"));

        let report = fixture.ingest(&options());
        assert_eq!(report.rows(), 2);
        assert_eq!(report.records(), 1, "two rows, one record");
        assert_eq!(report.collapsed(), 1);

        let facts = fixture.facts();
        let census = census(&facts);
        assert_eq!(census.table("l_260"), 1);
        assert_eq!(census.folded, 1, "the pile itself says a record was folded");
        let (_, document) = record_anchor(KIND_PURCHASE_DOCUMENT, 1, 50);
        let head = head_of(&facts, document).expect("one current head");

        let counts: Vec<i128> = find!(
            count: i128,
            pattern!(&facts, [{ head @ record::source_rows: ?count }])
        )
        .collect();
        assert_eq!(
            counts,
            vec![2],
            "the record says how many rows it came from"
        );

        assert_eq!(
            recomputed_content(&facts, head),
            content_digest(&facts, head)
        );

        let created: Vec<IntervalValue> = find!(
            value: IntervalValue,
            pattern!(&facts, [{ head @ record::created: ?value }])
        )
        .collect();
        assert_eq!(created.len(), 1);
        assert_eq!(
            created[0],
            parse_timestamp("2024-03-05 08:00:00").unwrap().unwrap()
        );

        let again = fixture.ingest(&options_at(2));
        assert_eq!(again.changed(), 0);
        assert_eq!(again.commits, 0);
    }

    #[test]
    fn identical_copies_collapse_without_a_creation_date_to_choose_between() {
        let fixture = Fixture::new();
        let row = purchase_row("50", "2024-03-05 08:00:00");
        fixture.table("l_260", &format!("{row}{row}{row}"));
        let report = fixture.ingest(&options());
        assert_eq!(report.rows(), 3);
        assert_eq!(report.records(), 1);
        let facts = fixture.facts();
        let (_, document) = record_anchor(KIND_PURCHASE_DOCUMENT, 1, 50);
        let head = head_of(&facts, document).unwrap();
        let counts: Vec<i128> = find!(
            count: i128,
            pattern!(&facts, [{ head @ record::source_rows: ?count }])
        )
        .collect();
        assert_eq!(counts, vec![3]);
    }

    #[test]
    fn two_different_records_under_one_identity_stop_the_ingest() {
        let fixture = Fixture::new();
        let mine = purchase_row("50", "2024-03-05 08:00:00");
        let other = mine.replace("EK-24-1", "EK-24-2");
        assert_ne!(mine, other);
        fixture.table("l_260", &format!("{mine}{other}"));
        let error = ingest(
            &fixture.pile,
            Some(&fixture.key),
            fixture.collection,
            &fixture.source(),
            &options(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("l_260"), "{error}");
        assert!(error.contains("50"), "the offending row is named: {error}");
        assert!(error.contains("identity"), "{error}");
    }

    #[test]
    fn every_observer_reads_only_columns_its_statement_selects() {
        let options = Options::default();
        for table in SLICE {
            let columns = columns_of(table).unwrap();
            let values: Vec<Option<String>> = columns
                .iter()
                .map(|column| match *column {
                    other if identity_columns(table).contains(&other) => Some("1".to_owned()),
                    other if columns.contains(&format!("{other}_c").as_str()) => {
                        Some("1000".to_owned())
                    }
                    _ => None,
                })
                .collect();
            let mut run = Run::default();
            for parent in SLICE {
                let anchor = record_anchor(kind_of(parent).unwrap(), 1, 1).1;
                run.remember(parent, 1, 1, anchor);
            }
            observe(table, columns, &values, &options, &mut run)
                .unwrap_or_else(|error| panic!("{table}: {error}"));
        }
    }

    #[test]
    fn every_slice_table_has_a_kind_a_parent_rule_and_an_ingest_rule() {
        for table in SLICE {
            assert!(kind_of(table).is_ok(), "{table} declares no anchor kind");
            let columns = columns_of(table).unwrap();
            assert!(!columns.is_empty(), "{table}");
            if let Some(parent) = parent_of(table) {
                let parent_at = SLICE.iter().position(|other| *other == parent);
                let table_at = SLICE.iter().position(|other| other == table);
                assert!(
                    parent_at < table_at,
                    "{table} is read before its parent {parent}"
                );
            }
        }
        let kinds: BTreeSet<Id> = SLICE.iter().map(|table| kind_of(table).unwrap()).collect();
        assert_eq!(kinds.len(), SLICE.len());
    }

    #[test]
    fn copy_text_format_separates_null_from_the_empty_string() {
        let rows = decode_copy("a\t\\N\t\tb\\tc\\nd\n", 4, "fixture").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].as_deref(), Some("a"));
        assert_eq!(rows[0][1], None, "\\N is NULL");
        assert_eq!(
            rows[0][2].as_deref(),
            Some(""),
            "an empty field is not NULL"
        );
        assert_eq!(
            rows[0][3].as_deref(),
            Some("b\tc\nd"),
            "escapes decode back to the delimiter and newline they stand for"
        );
    }

    #[test]
    fn copy_text_format_rejects_a_row_of_the_wrong_width() {
        assert!(decode_copy("a\tb\n", 3, "fixture").is_err());
    }

    #[test]
    fn the_unset_date_sentinel_is_absence_not_the_year_two() {
        assert_eq!(parse_timestamp("0002-11-30").unwrap(), None);
        assert!(parse_timestamp("2024-03-01").unwrap().is_some());
        assert!(parse_timestamp("2024-03-01 08:30:00.5").unwrap().is_some());
    }

    #[test]
    fn a_curr_column_becomes_the_exact_amount_it_denotes() {
        let row = Row {
            table: "l_261",
            key: "10".to_owned(),
            columns: &["f_10", "f_10_c"],
            values: &[Some("11900".to_owned()), Some("64".to_owned())],
        };
        let options = Options::default();
        let encoded = row.money("f_10", &options).unwrap().unwrap();
        let amount = Euros::try_from_inline(&encoded).unwrap();
        assert_eq!(amount.ratio(), Ratio::new(119, 10));
        assert_eq!(amount.to_string(), "11.90 EUR");
    }

    #[test]
    fn an_amount_whose_column_is_at_the_wrong_scale_stops_the_ingest() {
        let options = Options::default();
        let row = Row {
            table: "l_261",
            key: "10".to_owned(),
            columns: &["f_10", "f_10_c"],
            values: &[Some("11901".to_owned()), Some("64".to_owned())],
        };
        let error = row.money("f_10", &options).unwrap_err().to_string();
        assert!(error.contains("l_261"), "{error}");
        assert!(error.contains("f_10"), "{error}");
        assert!(
            error.contains("10"),
            "the offending record is named: {error}"
        );

        let computed = Row {
            table: "l_263_3",
            key: "43/197".to_owned(),
            columns: &["f_8", "f_8_c"],
            values: &[Some("11833".to_owned()), Some("64".to_owned())],
        };
        let amount =
            Euros::try_from_inline(&computed.money("f_8", &options).unwrap().unwrap()).unwrap();
        assert_eq!(amount.ratio(), Ratio::new(11_833, 1_000));
        assert_eq!(amount.to_string(), "11.833 EUR");
    }

    #[test]
    fn an_amount_in_another_currency_is_refused_rather_than_booked_as_euros() {
        let options = Options {
            currency_map: BTreeMap::from([
                ("64".to_owned(), "EUR".to_owned()),
                ("71".to_owned(), "CHF".to_owned()),
            ]),
            ..Options::default()
        };
        let row = Row {
            table: "l_260",
            key: "50".to_owned(),
            columns: &["f_7", "f_8", "f_8_c"],
            values: &[
                Some("71".to_owned()),
                Some("100000".to_owned()),
                Some("64".to_owned()),
            ],
        };
        assert!(row.money("f_8", &options).unwrap().is_some());

        let foreign = Row {
            table: "l_260",
            key: "50".to_owned(),
            columns: &["f_8", "f_8_c"],
            values: &[Some("100000".to_owned()), Some("71".to_owned())],
        };
        let error = foreign.money("f_8", &options).unwrap_err().to_string();
        assert!(error.contains("CHF"), "{error}");
        assert!(error.contains("Currency<Euro>"), "{error}");

        let unknown = Row {
            table: "l_260",
            key: "50".to_owned(),
            columns: &["f_8", "f_8_c"],
            values: &[Some("100000".to_owned()), Some("32".to_owned())],
        };
        let error = unknown.money("f_8", &options).unwrap_err().to_string();
        assert!(error.contains("no ISO-4217 mapping"), "{error}");
    }

    #[test]
    fn a_rate_is_an_exact_proportion_never_a_rounded_double() {
        let row = Row {
            table: "l_261",
            key: "10".to_owned(),
            columns: &["f_39", "f_10", "f_5"],
            values: &[
                Some("19".to_owned()),
                Some("12.5".to_owned()),
                Some("0.1".to_owned()),
            ],
        };
        assert_eq!(row.proportion("f_39").unwrap(), Some(Ratio::new(19, 100)));
        assert_eq!(row.proportion("f_10").unwrap(), Some(Ratio::new(1, 8)));
        assert_eq!(row.rational("f_5").unwrap(), Some(Ratio::new(1, 10)));
        assert_ne!(
            *Ratio::new(1i128, 10).numer() as f64 / *Ratio::new(1i128, 10).denom() as f64,
            0.1_f64.mul_add(3.0, 0.0) / 3.0,
            "0.1 has no exact binary form, which is the whole point"
        );
    }

    #[test]
    fn decimal_text_parses_exactly_including_scientific_notation() {
        assert_eq!(parse_decimal("7").unwrap(), Ratio::from_integer(7));
        assert_eq!(parse_decimal("-2.50").unwrap(), Ratio::new(-5, 2));
        assert_eq!(parse_decimal("1e-7").unwrap(), Ratio::new(1, 10_000_000));
        assert_eq!(parse_decimal("1.5e3").unwrap(), Ratio::from_integer(1500));
        assert_eq!(parse_decimal(".5").unwrap(), Ratio::new(1, 2));
        assert!(parse_decimal("abc").is_err());
        assert!(parse_decimal("").is_err());
    }

    #[test]
    fn an_over_long_short_value_is_refused_rather_than_truncated() {
        let row = Row {
            table: "l_256",
            key: "1".to_owned(),
            columns: &["f_62"],
            values: &[Some("x".repeat(33))],
        };
        assert!(row.short("f_62").is_err());
    }

    #[test]
    fn an_unmapped_currency_selector_stops_the_ingest() {
        let options = Options::default();
        assert_eq!(
            options.currency_for(Some("64"), "l_261", "10").unwrap(),
            "EUR"
        );
        assert_eq!(options.currency_for(None, "l_261", "10").unwrap(), "EUR");
        assert_eq!(
            options.currency_for(Some("0"), "l_261", "10").unwrap(),
            "EUR"
        );
        assert!(options.currency_for(Some("7"), "l_261", "10").is_err());
    }

    #[test]
    fn ssh_constructs_one_quoted_remote_command_without_connecting() {
        let source = Source::Ssh {
            host: "sample.invalid".into(),
            database: "operator's ledger".into(),
            sudo_user: Some("report reader".into()),
        };
        let command = source.command().unwrap();
        let arguments = command.get_args().collect::<Vec<_>>();
        assert_eq!(arguments, [
            "--", "sample.invalid",
            r"'sudo' '-u' 'report reader' '--' 'psql' '-d' 'operator'\''s ledger' '-X' '-q' '-v' 'ON_ERROR_STOP=1' '-f' '-'",
        ]);
        assert!(!source.describe().contains("ledger"));
        let empty_host = Source::Ssh { host: String::new(), database: "sample".into(), sudo_user: None };
        assert!(empty_host.command().is_err());
        let option_like_host = Source::Ssh { host: "-sample".into(), database: "sample".into(), sudo_user: None };
        assert!(option_like_host.command().is_err());
    }

    #[test]
    fn live_binding_preflight_distinguishes_current_fields_from_other_tenants_and_history() {
        let (mut source, anchor) = record_anchor(KIND_TASK, 7, 1);
        source += entity! {
            metadata::tag: &KIND_SNAPSHOT, revolver::of: anchor,
            metadata::created_at: at(1), task::step_record: 2i128,
        };
        let check = |source: &Fragment, tenant| {
            let options = Options { mandant: Some(tenant), ..Options::default() };
            require_observed_binding(source.facts(), &Heads::build(source.facts()), &options,
                "l_220", "task_step", &task::step_record)
        };
        assert!(check(&source, 7).is_err());
        assert!(check(&source, 8).is_ok());
        source += entity! {
            metadata::tag: &KIND_SNAPSHOT, revolver::of: anchor,
            metadata::created_at: at(2),
        };
        assert!(check(&source, 7).is_ok());
    }

    #[test]
    fn optional_fields_preserve_capture_positions_and_accept_explicit_identifiers() {
        let table = "l_260_12";
        let empty = statement(table, false, None).unwrap();
        let bindings = [
            "levy_amount=f_example_fee".parse().unwrap(),
            "levy_amount_c=f_example_unit".parse().unwrap(),
            "levy_rate=f_example_rate".parse().unwrap(),
        ];
        let sql = statement_with_fields(table, false, None, &bindings).unwrap();
        let mut expected = empty;
        for binding in bindings.iter().rev() {
            expected = expected.replace(&format!("NULL AS {}", binding.field), &format!("s.{} AS {}", binding.column, binding.field));
        }
        assert_eq!(sql, expected);
        assert!(statement_with_fields(table, false, None, &bindings[..1]).is_err());
        assert!(statement_with_fields(table, false, None, &bindings[1..2]).is_err());
        let task = statement_with_fields("l_220", false, None, &[
            "task_contact=f_example_contact".parse().unwrap(),
            "task_step=f_example_step".parse().unwrap(),
        ]).unwrap();
        assert!(task.contains("t.f_example_contact AS task_contact, t.f_example_step AS task_step"));
        assert!("other=f_example".parse::<FieldBinding>().is_err());
        assert!("levy_rate=f example".parse::<FieldBinding>().is_err());
        assert!(statement_with_fields(table, false, None, &[bindings[0].clone(), bindings[0].clone()]).is_err());
    }

    #[test]
    fn statements_apply_the_vendors_own_row_filters() {
        let sales = statement("l_261", false, Some(1)).unwrap();
        assert!(
            sales.contains("t.trashed = false AND t.deactivated = false"),
            "{sales}"
        );
        assert!(sales.contains("t.company = 1"), "{sales}");
        assert!(
            sales.ends_with("ORDER BY t.company, t.docid) TO STDOUT;\n"),
            "{sales}"
        );
        let lines = statement("l_261_12", false, None).unwrap();
        assert!(
            lines.contains("EXISTS (SELECT 1 FROM l_261 p WHERE p.docid = s.parent"),
            "{lines}"
        );
        assert!(lines.contains("p.trashed = false"), "{lines}");
        assert!(lines.contains("ORDER BY s.parent, s.itemid"), "{lines}");
        assert!(!statement("l_261", true, None).unwrap().contains("WHERE"));
    }

    #[test]
    fn a_reference_field_keeps_its_label_and_drops_its_target() {
        assert_eq!(
            reference_label("Muster, Erika (\u{10}\u{10}256;1;1\u{11})"),
            "Muster, Erika"
        );
        assert_eq!(
            reference_label("Muster, Erika, Beratung (\u{10}\u{10}256;1;1\u{11})"),
            "Muster, Erika, Beratung"
        );
        assert_eq!(reference_label("Exampleville (Nord)"), "Exampleville (Nord)");
        assert_eq!(reference_label(""), "");
    }

    #[test]
    fn the_directory_reads_accounts_and_not_permission_groups() {
        let directory = statement("userlist", false, Some(1)).unwrap();
        assert!(directory.contains("t.type = 'user'"), "{directory}");
        assert!(!directory.contains("t.company"), "{directory}");
        let everything = statement("userlist", true, None).unwrap();
        assert!(everything.contains("t.type = 'user'"), "{everything}");
    }

    #[test]
    fn a_sub_table_filters_on_its_parent_without_joining_to_it() {
        for table in SLICE.iter().filter(|table| parent_of(table).is_some()) {
            let reading = statement(table, false, None).unwrap();
            assert!(
                !reading.contains(" JOIN "),
                "{table} must not fan out on a duplicated parent: {reading}"
            );
            assert!(
                reading.contains("EXISTS (SELECT 1 FROM"),
                "{table}: {reading}"
            );
        }
    }

    #[test]
    fn a_count_statement_counts_rows_and_records_separately() {
        let counting = count_statement("l_261_12", false, Some(1)).unwrap();
        assert!(
            counting.starts_with("COPY (SELECT count(*), count(DISTINCT (s.parent, s.itemid))"),
            "{counting}"
        );
        assert!(
            counting.contains("EXISTS (SELECT 1 FROM l_261 p"),
            "{counting}"
        );
        assert!(counting.contains("p.company = 1"), "{counting}");
        let main = count_statement("l_260", false, None).unwrap();
        assert!(
            main.contains("count(DISTINCT (t.company, t.docid))"),
            "{main}"
        );
    }

    #[test]
    fn identity_is_the_identifying_attributes_and_nothing_else() {
        let (_, first) = record_anchor(KIND_SALES_DOCUMENT, 1, 10);
        let (_, again) = record_anchor(KIND_SALES_DOCUMENT, 1, 10);
        assert_eq!(first, again);
        assert_ne!(first, record_anchor(KIND_PAYMENT, 1, 10).1);
        assert_ne!(first, record_anchor(KIND_SALES_DOCUMENT, 2, 10).1);
        assert_ne!(first, record_anchor(KIND_SALES_DOCUMENT, 1, 11).1);
        let other = record_anchor(KIND_SALES_DOCUMENT, 1, 11).1;
        assert_eq!(sales_line_anchor(first, 1).1, sales_line_anchor(first, 1).1);
        assert_ne!(sales_line_anchor(first, 1).1, sales_line_anchor(first, 2).1);
        assert_ne!(sales_line_anchor(first, 1).1, sales_line_anchor(other, 1).1);
    }

    #[test]
    fn ingesting_the_same_snapshot_twice_adds_nothing() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");

        let first = fixture.ingest(&options());
        assert_eq!(
            first.rows(),
            7,
            "two parties, one invoice, two lines, one payment, one allocation"
        );
        assert_eq!(first.changed(), 7, "a first ingest is all delta");
        let after_first = fixture.facts();
        let census_first = census(&after_first);
        assert_eq!(census_first.table("l_256"), 2);
        assert_eq!(census_first.table("l_261"), 1);
        assert_eq!(census_first.table("l_261_12"), 2);
        assert_eq!(census_first.table("l_264"), 1);
        assert_eq!(census_first.table("l_264_9"), 1);
        assert_eq!(census_first.snapshots, 7, "one snapshot per record");
        assert_eq!(census_first.forked, 0);

        let second = fixture.ingest(&options_at(2));
        assert_eq!(second.rows(), 7, "the same rows are read again");
        assert_eq!(second.changed(), 0, "and none of them is a change");
        assert_eq!(second.commits, 0, "so nothing is published");
        let after_second = fixture.facts();
        assert_eq!(census(&after_second), census_first, "no entity growth");
        assert_eq!(
            after_second.len(),
            after_first.len(),
            "not one new fact either"
        );
    }

    #[test]
    fn ingesting_a_changed_snapshot_adds_only_the_delta() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.ingest(&options());
        let before = fixture.facts();
        let census_before = census(&before);

        snapshot_v1(&fixture, "238000");
        let third = fixture.ingest(&options_at(2));
        assert_eq!(third.rows(), 7);
        assert_eq!(
            third.changed(),
            3,
            "the invoice, the payment and its allocation changed; the parties and lines did not"
        );

        let after = fixture.facts();
        let census_after = census(&after);
        assert_eq!(
            census_after.records(),
            census_before.records(),
            "a corrected invoice is the same invoice"
        );
        assert_eq!(
            census_after.snapshots,
            census_before.snapshots + 3,
            "exactly one new observation per changed record"
        );
        assert_eq!(
            census_after.forked, 0,
            "each change is stamped later than its predecessor"
        );

        let (_, invoice) = record_anchor(KIND_SALES_DOCUMENT, 1, 10);
        let head = head_of(&after, invoice).expect("one current head");
        let totals: Vec<Euros> = find!(
            value: EuroValue,
            pattern!(&after, [{ head @ sales::gross_total: ?value }])
        )
        .map(|value| Euros::try_from_inline(&value).unwrap())
        .collect();
        assert_eq!(totals.len(), 1, "the head is unambiguous");
        assert_eq!(
            totals[0].ratio(),
            Ratio::new(238, 1),
            "the value, with no scale to unwind"
        );
        assert_eq!(totals[0].to_string(), "238.00 EUR");
    }

    fn legacy_snapshot(anchor: Id, predecessors: &[Id]) -> (Fragment, Id) {
        let fragment = entity! {
            metadata::tag: &KIND_SNAPSHOT,
            revolver::of: anchor,
            metadata::supersedes*: predecessors.iter(),
        };
        let id = fragment.root().expect("intrinsic snapshot has one root");
        (fragment, id)
    }

    fn stamped_snapshot(anchor: Id, observed: IntervalValue) -> (Fragment, Id) {
        let fragment = entity! {
            metadata::tag: &KIND_SNAPSHOT,
            revolver::of: anchor,
            metadata::created_at: observed,
        };
        let id = fragment.root().expect("intrinsic snapshot has one root");
        (fragment, id)
    }

    #[test]
    fn a_later_import_wins_by_its_stamp_and_names_no_predecessor() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.ingest(&options_at(1));
        let (_, invoice) = record_anchor(KIND_SALES_DOCUMENT, 1, 10);
        let first = head_of(&fixture.facts(), invoice).expect("one current head");

        snapshot_v1(&fixture, "238000");
        fixture.ingest(&options_at(2));
        let facts = fixture.facts();
        let second = head_of(&facts, invoice).expect("one current head");
        assert_ne!(first, second, "a corrected invoice is a new state");

        let edges: Vec<Id> = find!(
            old: Id,
            pattern!(&facts, [{ second @ metadata::supersedes: ?old }])
        )
        .collect();
        assert!(edges.is_empty(), "a stated order writes no supersedes edge");

        let stamp: Vec<IntervalValue> = find!(
            observed: IntervalValue,
            pattern!(&facts, [{ second @ metadata::created_at: ?observed }])
        )
        .collect();
        assert_eq!(stamp, vec![at(2)]);
        let earlier: Vec<IntervalValue> = find!(
            observed: IntervalValue,
            pattern!(&facts, [{ first @ metadata::created_at: ?observed }])
        )
        .collect();
        assert_eq!(earlier, vec![at(1)]);
    }

    #[test]
    fn one_import_is_one_instant_not_a_clock_reading_per_record() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.ingest(&options_at(1));
        let facts = fixture.facts();
        let mut stamps: Vec<[u8; 32]> = find!(
            observed: IntervalValue,
            pattern!(&facts, [
                { _?snapshot @ metadata::tag: &KIND_SNAPSHOT, metadata::created_at: ?observed }
            ])
        )
        .map(|observed| observed.raw)
        .collect();
        assert_eq!(stamps.len(), 7, "seven records were stamped");
        stamps.sort_unstable();
        stamps.dedup();
        assert_eq!(
            stamps,
            vec![at(1).raw],
            "and one reading of the source stamped all of them"
        );
    }

    #[test]
    fn snapshots_from_before_the_register_still_resolve() {
        let anchor = record_anchor(KIND_SALES_DOCUMENT, 1, 900).1;
        let untouched = record_anchor(KIND_SALES_DOCUMENT, 1, 901).1;
        let mut space = TribleSet::new();
        let (first, first_id) = legacy_snapshot(anchor, &[]);
        let (second, second_id) = legacy_snapshot(anchor, &[first_id]);
        let (older, older_id) = legacy_snapshot(untouched, &[]);
        let (newer, newer_id) = legacy_snapshot(untouched, &[older_id]);
        space += first;
        space += second;
        space += older;
        space += newer;
        assert_eq!(
            head_of(&space, anchor),
            Some(second_id),
            "within its own era the DAG still decides"
        );
        assert_eq!(head_of(&space, untouched), Some(newer_id));

        let (stamped, stamped_id) = stamped_snapshot(anchor, at(1));
        space += stamped;
        assert_eq!(head_of(&space, anchor), Some(stamped_id));
        assert_eq!(
            head_of(&space, untouched),
            Some(newer_id),
            "the other record's era is not disturbed by a stamp on this one"
        );
    }

    #[test]
    fn the_bulk_index_agrees_with_the_query_layer_order() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.ingest(&options_at(1));
        snapshot_v1(&fixture, "238000");
        fixture.ingest(&options_at(2));
        let mut space = fixture.facts();

        let legacy = record_anchor(KIND_SALES_DOCUMENT, 1, 900).1;
        let overtaken = record_anchor(KIND_SALES_DOCUMENT, 1, 901).1;
        let (first, first_id) = legacy_snapshot(legacy, &[]);
        let (second, _) = legacy_snapshot(legacy, &[first_id]);
        let (older, _) = legacy_snapshot(overtaken, &[]);
        let (stamped, _) = stamped_snapshot(overtaken, at(3));
        space += first;
        space += second;
        space += older;
        space += stamped;

        let heads = Heads::build(&space);
        let order = head_register(&space);
        let mut anchors: Vec<Id> = find!(
            anchor: Id,
            pattern!(&space, [
                { _?snapshot @ metadata::tag: &KIND_SNAPSHOT, revolver::of: ?anchor }
            ])
        )
        .collect();
        anchors.sort_unstable();
        anchors.dedup();
        assert_eq!(anchors.len(), 9, "seven ingested records and two hand-written");
        for anchor in anchors {
            let candidates: Vec<Id> = find!(
                snapshot: Id,
                pattern!(&space, [
                    { ?snapshot @ metadata::tag: &KIND_SNAPSHOT, revolver::of: &anchor }
                ])
            )
            .collect();
            let by_query: BTreeSet<Id> = resolve(&order, candidates);
            let by_index: BTreeSet<Id> = heads.heads(anchor).iter().copied().collect();
            assert_eq!(by_index, by_query, "anchor {anchor:X}");
        }
    }

    #[test]
    fn a_dry_run_reports_the_delta_without_publishing_it() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        let report = fixture.ingest(&Options {
            dry_run: true,
            ..options()
        });
        assert_eq!(report.changed(), 7);
        assert_eq!(report.commits, 0);
        assert_eq!(census(&fixture.facts()).records(), 0, "nothing was written");
    }

    fn sales_line_row(itemid: &str, total: &str, extra: &[(&str, &str)]) -> String {
        let mut values = vec![
            ("parent", "10"),
            ("itemid", itemid),
            ("deactivated", "f"),
            ("subtotal", "f"),
            ("itemlevel", "0"),
            ("numtext", itemid),
            ("f_1", "0"),
            ("f_3", "Position"),
            ("f_9", total),
            ("f_9_c", "64"),
            ("f_11", "2"),
        ];
        for (column, value) in extra {
            match values.iter_mut().find(|(name, _)| name == column) {
                Some(slot) => slot.1 = value,
                None => values.push((column, value)),
            }
        }
        row_for("l_261_12", &values)
    }

    #[test]
    fn a_line_sum_reproduces_the_header_total_only_once_subtotals_are_flagged() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.table(
            "l_261_12",
            &[
                sales_line_row("1", "80000", &[]),
                sales_line_row("2", "20000", &[]),
                sales_line_row("3", "100000", &[("subtotal", "t"), ("f_3", "Zwischensumme")]),
                sales_line_row("4", "50000", &[("deactivated", "t")]),
            ]
            .concat(),
        );
        fixture.ingest(&options());
        let facts = fixture.facts();

        let (_, invoice) = record_anchor(KIND_SALES_DOCUMENT, 1, 10);
        let invoice_head = head_of(&facts, invoice).unwrap();
        let header: Vec<EuroValue> = find!(
            value: EuroValue,
            pattern!(&facts, [{ invoice_head @ sales::net_total: ?value }])
        )
        .collect();
        let header = Euros::try_from_inline(&header[0]).unwrap();

        let sum = |keep: &dyn Fn(Id) -> bool| {
            ids_of_kind(&facts, KIND_SALES_LINE)
                .into_iter()
                .filter_map(|anchor| head_of(&facts, anchor))
                .filter(|head| keep(*head))
                .flat_map(|head| {
                    find!(
                        value: EuroValue,
                        pattern!(&facts, [{ head @ line::total: ?value }])
                    )
                })
                .map(|value| Euros::try_from_inline(&value).unwrap().ratio())
                .sum::<num_rational::Ratio<i128>>()
        };
        assert_eq!(sum(&|_| true), num_rational::Ratio::from_integer(250));
        assert_eq!(
            sum(&|head| {
                !exists!(pattern!(&facts, [{ head @ record::subtotal: true }]))
                    && !exists!(pattern!(&facts, [{ head @ record::deactivated: true }]))
            }),
            header.ratio(),
        );
        assert_eq!(header.ratio(), num_rational::Ratio::from_integer(100));
    }

    #[test]
    fn links_resolve_to_the_anchors_the_other_tables_write() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.ingest(&options());
        let facts = fixture.facts();

        let (_, company) = record_anchor(KIND_PARTY, 1, 1);
        let (_, contact) = record_anchor(KIND_PARTY, 1, 2);
        let (_, invoice) = record_anchor(KIND_SALES_DOCUMENT, 1, 10);
        let (_, settlement) = record_anchor(KIND_PAYMENT, 1, 20);
        let invoice_head = head_of(&facts, invoice).unwrap();

        let customers: Vec<Id> = find!(
            id: Id,
            pattern!(&facts, [{ invoice_head @ sales::customer: ?id }])
        )
        .collect();
        let attention: Vec<Id> = find!(
            id: Id,
            pattern!(&facts, [{ invoice_head @ sales::attention: ?id }])
        )
        .collect();
        assert_eq!(customers, vec![company]);
        assert_eq!(attention, vec![contact]);

        let contact_head = head_of(&facts, contact).unwrap();
        let employers: Vec<Id> = find!(
            id: Id,
            pattern!(&facts, [{ contact_head @ party::employer: ?id }])
        )
        .collect();
        assert_eq!(employers, vec![company]);

        let (_, allocation_anchor_id) = allocation_anchor(settlement, 1);
        let allocation_head = head_of(&facts, allocation_anchor_id).unwrap();
        let settled: Vec<Id> = find!(
            id: Id,
            pattern!(&facts, [{ allocation_head @ allocation::sales_document: ?id }])
        )
        .collect();
        assert_eq!(settled, vec![invoice]);
        let payment_head = head_of(&facts, settlement).unwrap();
        let counterparties: Vec<Id> = find!(
            id: Id,
            pattern!(&facts, [{ payment_head @ payment::counterparty: ?id }])
        )
        .collect();
        assert_eq!(
            counterparties,
            vec![company],
            "f_10 is who paid, not what was paid"
        );
    }

    #[test]
    fn a_code_keeps_its_raw_value_even_with_no_documented_label() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.ingest(&options());
        let facts = fixture.facts();

        let doctypes = domain_fragment(DOMAIN_DOCTYPE).1;
        let invoice_kind = code_fragment(doctypes, "285", None).unwrap().1;
        let names: Vec<TextHandle> = find!(
            handle: TextHandle,
            pattern!(&facts, [{ invoice_kind @ metadata::name: ?handle }])
        )
        .collect();
        assert_eq!(names.len(), 1, "a documented code carries its label");

        let types = domain_fragment(DOMAIN_ADDRESS_TYPE).1;
        let customer = code_fragment(types, "1", None).unwrap().1;
        assert!(exists!(pattern!(&facts, [{ customer @ code::code: "1" }])));
        let unknown = code_fragment(types, "8", None).unwrap().1;
        assert_ne!(customer, unknown);
    }

    #[test]
    fn two_rows_with_one_identity_stop_the_ingest_rather_than_merging() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        let one = payment_row("20", "2024-04-02 10:00:00", "119000", "eins");
        let other = payment_row("20", "2024-04-03 10:00:00", "119000", "zwei");
        fixture.table("l_264", &format!("{one}{other}"));
        let error = ingest(
            &fixture.pile,
            Some(&fixture.key),
            fixture.collection,
            &fixture.source(),
            &options(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("l_264"), "{error}");
        assert!(error.contains("identity"), "{error}");
    }

    fn company_row(docid: &str, number: &str, name: &str) -> String {
        row_for(
            "l_256",
            &[
                ("docid", docid),
                ("company", "1"),
                ("doctype", "257"),
                ("credate", "2020-01-02 09:00:00"),
                ("moddate", "2020-01-02 09:00:00"),
                ("trashed", "f"),
                ("deactivated", "f"),
                ("f_1", name),
                ("f_3", name),
                ("f_11", "0"),
                ("f_12", "1"),
                ("f_62", number),
                ("f_87", "0"),
            ],
        )
    }

    fn group_fixture() -> Fixture {
        let fixture = Fixture::new();
        fixture.table(
            "l_256",
            &format!(
                "{}{}",
                company_row("1", "A00001", "Acme Deutschland GmbH"),
                company_row("2", "A00002", "Acme Europe GmbH")
            ),
        );
        ingest(
            &fixture.pile,
            Some(&fixture.key),
            fixture.collection,
            &fixture.source(),
            &options(),
        )
        .unwrap();
        fixture
    }

    fn declare(fixture: &Fixture, name: Option<&str>, numbers: &[&str]) -> GroupReport {
        let numbers: Vec<String> = numbers.iter().map(|number| (*number).to_owned()).collect();
        declare_group(&fixture.pile, Some(&fixture.key), fixture.collection, name, &numbers).unwrap()
    }

    fn heads_of(fixture: &Fixture) -> Vec<CorporateGroup> {
        let space = read(&fixture.pile, Some(&fixture.key), fixture.collection).unwrap();
        group_heads(&space)
    }

    #[test]
    fn snapshot_reader_resolves_attachments_after_the_mutable_pile_is_closed() {
        let fixture = group_fixture();
        let snapshot = read_snapshot(&fixture.pile, Some(&fixture.key), fixture.collection).unwrap();
        let handles: Vec<TextHandle> = find!(
            handle: TextHandle,
            pattern!(&snapshot.facts, [{ _?snapshot @ party::company_name: ?handle }])
        )
        .collect();
        let mut names: Vec<String> = handles
            .into_iter()
            .map(|handle| read_text(&snapshot.reader, handle).unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["Acme Deutschland GmbH", "Acme Europe GmbH"]);
    }

    #[test]
    fn redeclaring_an_unchanged_group_publishes_nothing() {
        let fixture = group_fixture();
        assert!(declare(&fixture, Some("Acme"), &["A00001", "A00002"])
            .group
            .is_some());
        let again = declare(&fixture, Some("Acme"), &["A00001", "A00002"]);
        assert_eq!(again.group, None, "an unchanged re-declaration published");
        assert_eq!(again.facts, 0);
        assert_eq!(heads_of(&fixture).len(), 1);
    }

    #[test]
    fn a_divestiture_supersedes_rather_than_retracts() {
        let fixture = group_fixture();
        declare(&fixture, Some("Acme"), &["A00001", "A00002"]);
        assert_eq!(heads_of(&fixture)[0].members.len(), 2);

        let after = declare(&fixture, None, &["A00001"]);
        assert!(after.group.is_some(), "dropping a member is a change");
        assert_eq!(after.superseded.len(), 1, "it replaces the group it edits");
        let heads = heads_of(&fixture);
        assert_eq!(heads.len(), 1, "the predecessor is no longer a head");
        assert_eq!(heads[0].members.len(), 1, "the divested company left");

        let space = read(&fixture.pile, Some(&fixture.key), fixture.collection).unwrap();
        assert_eq!(
            all_groups(&space).len(),
            2,
            "history keeps the former group"
        );
    }

    #[test]
    fn identity_is_the_claim_not_a_label() {
        let fixture = group_fixture();
        let first = declare(&fixture, Some("Acme Group"), &["A00001", "A00002"]);
        let second = declare(&fixture, Some("ACME Konzern"), &["A00002", "A00001"]);
        assert_eq!(
            second.group, first.group,
            "the same claim is the same entity"
        );
        assert!(
            second.superseded.is_empty(),
            "a rename must not fork the lineage"
        );
        let heads = heads_of(&fixture);
        assert_eq!(heads.len(), 1, "a rename did not fork the group");
        assert_eq!(heads[0].names.len(), 2, "both names annotate the one group");
    }

    #[test]
    fn naming_companies_from_two_groups_merges_them() {
        let fixture = group_fixture();
        declare(&fixture, Some("One"), &["A00001"]);
        declare(&fixture, Some("Two"), &["A00002"]);
        assert_eq!(heads_of(&fixture).len(), 2);

        let merged = declare(&fixture, Some("Both"), &["A00001", "A00002"]);
        assert_eq!(merged.superseded.len(), 2, "it replaces both predecessors");
        let heads = heads_of(&fixture);
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].members.len(), 2);
    }

    #[test]
    fn an_unresolvable_member_refuses_the_whole_declaration() {
        let fixture = group_fixture();
        let numbers = vec!["A00001".to_owned(), "A09999".to_owned()];
        let error = declare_group(&fixture.pile, Some(&fixture.key), fixture.collection, Some("Acme"), &numbers)
            .unwrap_err()
            .to_string();
        assert!(error.contains("A09999"), "{error}");
        assert!(
            heads_of(&fixture).is_empty(),
            "a refused declaration still published a group"
        );
    }

    #[test]
    fn every_slice_table_declares_its_columns_and_its_parent() {
        for table in SLICE {
            assert!(columns(table) > 0);
            let columns = columns_of(table).unwrap();
            let identity = identity_columns(table);
            for column in identity {
                assert!(
                    columns.contains(column),
                    "{table} does not select its identifying column {column}"
                );
            }
            let mut leading = columns[..identity.len()].to_vec();
            leading.sort_unstable();
            let mut wanted = identity.to_vec();
            wanted.sort_unstable();
            assert_eq!(
                leading, wanted,
                "{table} must select its identifying columns first"
            );
        }
    }

    #[test]
    fn every_amount_in_the_pile_decodes_as_a_euro_amount() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.ingest(&options());
        let facts = fixture.facts();
        let mut seen = 0usize;
        for attribute in [
            sales::net_total.id(),
            sales::tax_total.id(),
            sales::gross_total.id(),
            line::unit_price.id(),
            line::total.id(),
            line::net_full.id(),
            payment::amount.id(),
            allocation::amount.id(),
        ] {
            for trible in facts.iter() {
                if trible.a() != &attribute {
                    continue;
                }
                let raw: [u8; 32] = trible.v::<triblespace::core::inline::encodings::UnknownInline>().raw;
                let amount = Euros::try_from_inline(&EuroValue::new(raw))
                    .expect("every stored amount is a canonical rational");
                assert!(
                    amount.ratio().denom() > &0,
                    "reduced with a positive denominator"
                );
                seen += 1;
            }
        }
        assert!(seen >= 8, "the fixture exercises every money-bearing kind");
    }

    #[test]
    fn content_is_the_intrinsic_root_of_the_field_facts() {
        let fixture = Fixture::new();
        snapshot_v1(&fixture, "119000");
        fixture.ingest(&options());
        let facts = fixture.facts();
        let (_, invoice) = record_anchor(KIND_SALES_DOCUMENT, 1, 10);
        let head = head_of(&facts, invoice).unwrap();
        assert_eq!(
            recomputed_content(&facts, head),
            content_digest(&facts, head)
        );
    }

    fn content_digest(facts: &TribleSet, head: Id) -> Id {
        let digest: Vec<Id> = find!(
            id: Id,
            pattern!(facts, [{ head @ revolver::content: ?id }])
        )
        .collect();
        assert_eq!(digest.len(), 1, "a snapshot carries exactly one digest");
        digest[0]
    }

    fn recomputed_content(facts: &TribleSet, head: Id) -> Id {
        let mut rows: Vec<IntrinsicEntityRow> = Vec::new();
        for trible in facts.iter() {
            if trible.e() != &head {
                continue;
            }
            let attribute = *trible.a();
            if attribute == metadata::tag.id()
                || attribute == revolver::of.id()
                || attribute == revolver::content.id()
                || attribute == metadata::created_at.id()
                || attribute == metadata::supersedes.id()
            {
                continue;
            }
            let value: [u8; 32] = trible.v::<triblespace::core::inline::encodings::UnknownInline>().raw;
            rows.push(IntrinsicEntityRow::new(attribute, value));
        }
        build_intrinsic_entity(rows).0
    }
}
