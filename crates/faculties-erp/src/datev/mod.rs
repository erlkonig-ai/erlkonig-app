//! DATEV EXTF and DTVF Buchungsstapel records. Account identifiers retain
//! their text form, and the debit/credit column determines the signed amount.

pub mod schema;

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

const DATA_CATEGORY_BUCHUNGSSTAPEL: &str = "21";
const SUPPORTED_FORMAT_VERSIONS: [&str; 2] = ["12", "13"];

mod columns {
    pub const AMOUNT: usize = 0;
    pub const DEBIT_CREDIT: usize = 1;
    pub const ACCOUNT: usize = 6;
    pub const CONTRA_ACCOUNT: usize = 7;
    pub const TAX_KEY: usize = 8;
    pub const POSTED_ON: usize = 9;
    pub const DOCUMENT_NUMBER: usize = 10;
    pub const DOCUMENT_FIELD_2: usize = 11;
    pub const TEXT: usize = 13;
    pub const MINIMUM_WIDTH: usize = 14;
}

mod header {
    pub const MAGIC: usize = 0;
    pub const DATA_CATEGORY: usize = 2;
    pub const FORMAT_VERSION: usize = 4;
    pub const CONSULTANT: usize = 10;
    pub const CLIENT: usize = 11;
    pub const FISCAL_YEAR_START: usize = 12;
    pub const PERIOD_FROM: usize = 14;
    pub const PERIOD_TO: usize = 15;
    pub const MINIMUM_WIDTH: usize = 16;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchHeader {
    pub consultant: i64,
    pub client: i64,
    pub fiscal_year_start: NaiveDate,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NaiveDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl NaiveDate {
    fn parse_compact(text: &str) -> Result<Self> {
        if text.len() != 8 || !text.bytes().all(|b| b.is_ascii_digit()) {
            bail!("expected a YYYYMMDD date, found {text:?}");
        }
        Ok(Self {
            year: text[0..4].parse()?,
            month: text[4..6].parse()?,
            day: text[6..8].parse()?,
        })
    }

    fn parse_belegdatum(text: &str, fiscal_year_start: NaiveDate) -> Result<Self> {
        let digits = text.trim();
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            bail!("expected a DDMM Belegdatum, found {text:?}");
        }
        let day: u32 = digits[0..2].parse()?;
        let month: u32 = digits[2..4].parse()?;
        let year = if month < fiscal_year_start.month {
            fiscal_year_start.year + 1
        } else {
            fiscal_year_start.year
        };
        Ok(Self { year, month, day })
    }
}

fn split_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                current.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ';' if !quoted => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

pub fn parse_header(line: &str) -> Result<BatchHeader> {
    let fields = split_line(line);
    if fields.len() < header::MINIMUM_WIDTH {
        bail!(
            "header has {} fields, expected at least {}",
            fields.len(),
            header::MINIMUM_WIDTH
        );
    }
    let magic = fields[header::MAGIC].trim();
    if magic != "EXTF" && magic != "DTVF" {
        bail!("not a DATEV export: first field is {magic:?}, expected EXTF or DTVF");
    }
    let category = fields[header::DATA_CATEGORY].trim();
    if category != DATA_CATEGORY_BUCHUNGSSTAPEL {
        bail!(
            "this is data category {category:?}, not {DATA_CATEGORY_BUCHUNGSSTAPEL} \
             (Buchungsstapel). Only bookings are read here"
        );
    }
    let version = fields[header::FORMAT_VERSION].trim();
    if !SUPPORTED_FORMAT_VERSIONS.contains(&version) {
        bail!(
            "format version {version:?} is not one this ingest has been checked against \
             ({}). Reading it would take columns from the wrong positions, so it stops here \
             rather than mis-reading money",
            SUPPORTED_FORMAT_VERSIONS.join(" or ")
        );
    }
    Ok(BatchHeader {
        consultant: fields[header::CONSULTANT].trim().parse().with_context(|| {
            format!("Beraternummer {:?}", fields[header::CONSULTANT])
        })?,
        client: fields[header::CLIENT]
            .trim()
            .parse()
            .with_context(|| format!("Mandantennummer {:?}", fields[header::CLIENT]))?,
        fiscal_year_start: NaiveDate::parse_compact(fields[header::FISCAL_YEAR_START].trim())
            .context("WJ-Beginn")?,
        period_from: NaiveDate::parse_compact(fields[header::PERIOD_FROM].trim())
            .context("Datum von")?,
        period_to: NaiveDate::parse_compact(fields[header::PERIOD_TO].trim())
            .context("Datum bis")?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookingRow {
    pub amount_units: i128,
    pub amount_scale: u32,
    pub account: String,
    pub contra_account: String,
    pub tax_key: String,
    pub posted_on: NaiveDate,
    pub document_number: String,
    pub document_field_2: String,
    pub text: String,
}

fn parse_amount(text: &str, debit_credit: &str) -> Result<(i128, u32)> {
    let cleaned: String = text.trim().chars().filter(|c| *c != '.').collect();
    if cleaned.is_empty() {
        bail!("empty Umsatz");
    }
    let (whole, fraction) = match cleaned.split_once(',') {
        Some((whole, fraction)) => (whole, fraction),
        None => (cleaned.as_str(), ""),
    };
    if !whole.bytes().all(|b| b.is_ascii_digit()) || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        bail!("Umsatz {text:?} is not a plain decimal");
    }
    let digits = format!("{whole}{fraction}");
    let magnitude: i128 = digits
        .parse()
        .with_context(|| format!("Umsatz {text:?} does not fit"))?;
    let sign = match debit_credit.trim() {
        "S" | "s" => 1,
        "H" | "h" => -1,
        other => bail!("Soll/Haben-Kennzeichen {other:?} is neither S nor H"),
    };
    Ok((sign * magnitude, fraction.len() as u32))
}

pub fn parse_row(line: &str, header: &BatchHeader) -> Result<BookingRow> {
    let fields = split_line(line);
    if fields.len() < columns::MINIMUM_WIDTH {
        bail!(
            "row has {} fields, expected at least {}",
            fields.len(),
            columns::MINIMUM_WIDTH
        );
    }
    let (amount_units, amount_scale) = parse_amount(
        &fields[columns::AMOUNT],
        &fields[columns::DEBIT_CREDIT],
    )?;
    Ok(BookingRow {
        amount_units,
        amount_scale,
        account: fields[columns::ACCOUNT].trim().to_string(),
        contra_account: fields[columns::CONTRA_ACCOUNT].trim().to_string(),
        tax_key: fields[columns::TAX_KEY].trim().to_string(),
        posted_on: NaiveDate::parse_belegdatum(
            &fields[columns::POSTED_ON],
            header.fiscal_year_start,
        )?,
        document_number: fields[columns::DOCUMENT_NUMBER].trim().to_string(),
        document_field_2: fields[columns::DOCUMENT_FIELD_2].trim().to_string(),
        text: fields[columns::TEXT].trim().to_string(),
    })
}

#[derive(Clone, Debug)]
pub struct Buchungsstapel {
    pub header: BatchHeader,
    pub rows: Vec<BookingRow>,
    pub rejected: Vec<(usize, String)>,
}

pub fn read(path: &Path) -> Result<Buchungsstapel> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text: String = bytes.iter().map(|b| *b as char).collect();
    let mut lines = text.lines();
    let header_line = lines.next().context("file is empty")?;
    let header = parse_header(header_line)?;
    lines.next().context("file has a header but no captions row")?;

    let mut rows = Vec::new();
    let mut rejected = Vec::new();
    for (offset, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_row(line, &header) {
            Ok(row) => rows.push(row),
            Err(error) => rejected.push((offset + 3, format!("{error:#}"))),
        }
    }
    Ok(Buchungsstapel {
        header,
        rows,
        rejected,
    })
}

pub fn collapse(rows: &[BookingRow]) -> BTreeMap<BookingRow, usize> {
    let mut counts: BTreeMap<BookingRow, usize> = BTreeMap::new();
    for row in rows {
        *counts.entry(row.clone()).or_default() += 1;
    }
    counts
}

impl Ord for BookingRow {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (
            self.posted_on,
            &self.account,
            &self.contra_account,
            self.amount_units,
            self.amount_scale,
            &self.tax_key,
            &self.document_number,
            &self.document_field_2,
            &self.text,
        )
            .cmp(&(
                other.posted_on,
                &other.account,
                &other.contra_account,
                other.amount_units,
                other.amount_scale,
                &other.tax_key,
                &other.document_number,
                &other.document_field_2,
                &other.text,
            ))
    }
}

impl PartialOrd for BookingRow {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_line(version: &str, category: &str) -> String {
        format!(
            "\"EXTF\";700;{category};\"Buchungsstapel\";{version};20310502103000000;;\"\";\"\";\"\";\
             10001;20002;20310101;4;20310101;20311231;\"Example Batch\";\"\";1;;0;\"EUR\""
        )
    }

    fn captions() -> &'static str {
        "Umsatz;S/H;WKZ;Kurs;Basis;WKZ-Basis;Konto;Gegenkonto;BU;Belegdatum;Belegfeld1;Belegfeld2;Skonto;Buchungstext"
    }

    #[test]
    fn an_unchecked_format_version_refuses_rather_than_shifting_columns() {
        let error = parse_header(&header_line("9", "21")).unwrap_err().to_string();
        assert!(error.contains("format version"), "{error}");
        assert!(
            error.contains("mis-reading money"),
            "the refusal should say why it matters: {error}"
        );
    }

    #[test]
    fn another_data_category_is_not_read_as_bookings() {
        let error = parse_header(&header_line("13", "20")).unwrap_err().to_string();
        assert!(error.contains("data category"), "{error}");
    }

    #[test]
    fn a_header_states_the_client_and_the_period() {
        let header = parse_header(&header_line("13", "21")).unwrap();
        assert_eq!(header.consultant, 10001);
        assert_eq!(header.client, 20002);
        assert_eq!(header.fiscal_year_start.year, 2031);
        assert_eq!(header.period_to.month, 12);
    }

    #[test]
    fn the_direction_character_carries_the_sign() {
        assert_eq!(parse_amount("1234,56", "S").unwrap(), (123456, 2));
        assert_eq!(parse_amount("1234,56", "H").unwrap(), (-123456, 2));
        assert_eq!(parse_amount("1.234,56", "S").unwrap(), (123456, 2));
        assert_eq!(parse_amount("1000", "S").unwrap(), (1000, 0));
        assert!(parse_amount("1234,56", "X").is_err());
    }

    #[test]
    fn a_belegdatum_takes_its_year_from_the_financial_year() {
        let july = NaiveDate {
            year: 2025,
            month: 7,
            day: 1,
        };
        let august = NaiveDate::parse_belegdatum("1508", july).unwrap();
        assert_eq!((august.year, august.month, august.day), (2025, 8, 15));
        let march = NaiveDate::parse_belegdatum("0303", july).unwrap();
        assert_eq!((march.year, march.month, march.day), (2026, 3, 3));
    }

    #[test]
    fn a_quoted_field_may_contain_the_separator() {
        let fields = split_line("1;\"a;b\";\"say \"\"hi\"\"\";4");
        assert_eq!(fields, vec!["1", "a;b", "say \"hi\"", "4"]);
    }

    #[test]
    fn a_row_reads_against_its_header() {
        let header = parse_header(&header_line("13", "21")).unwrap();
        let row = parse_row(
            "4,25;S;EUR;;;;1100;4100;9;0204;\"SAMPLE-A\";\"\";;\"Example token\"",
            &header,
        )
        .unwrap();
        assert_eq!(row.amount_units, 425);
        assert_eq!(row.amount_scale, 2);
        assert_eq!(row.account, "1100");
        assert_eq!(row.contra_account, "4100");
        assert_eq!(row.tax_key, "9");
        assert_eq!((row.posted_on.year, row.posted_on.month), (2031, 4));
        assert_eq!(row.document_number, "SAMPLE-A");
        assert_eq!(row.text, "Example token");
    }

    #[test]
    fn an_unreadable_row_is_reported_rather_than_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("EXTF_Buchungsstapel.csv");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n{}\n",
                header_line("13", "21"),
                captions(),
                "4,25;S;EUR;;;;1100;4100;9;0204;\"SAMPLE-A\";\"\";;\"ok\"",
                "not;a;booking"
            ),
        )
        .unwrap();
        let stapel = read(&path).unwrap();
        assert_eq!(stapel.rows.len(), 1);
        assert_eq!(stapel.rejected.len(), 1);
        assert_eq!(stapel.rejected[0].0, 4, "reports the file's own line number");
    }

    #[test]
    fn identical_rows_collapse_and_say_how_many() {
        let header = parse_header(&header_line("13", "21")).unwrap();
        let line = "100,00;S;EUR;;;;10000;8400;9;1503;\"RE-1\";\"\";;\"same\"";
        let rows = vec![
            parse_row(line, &header).unwrap(),
            parse_row(line, &header).unwrap(),
            parse_row(
                "100,00;S;EUR;;;;10000;8400;9;1503;\"RE-2\";\"\";;\"different\"",
                &header,
            )
            .unwrap(),
        ];
        let collapsed = collapse(&rows);
        assert_eq!(collapsed.len(), 2);
        assert_eq!(*collapsed.values().max().unwrap(), 2);
    }

    #[test]
    fn re_reading_the_same_export_yields_the_same_bookings() {
        let header = parse_header(&header_line("13", "21")).unwrap();
        let line = "42,00;H;EUR;;;;8400;10000;;0102;\"G-1\";\"\";;\"Gutschrift\"";
        let first = parse_row(line, &header).unwrap();
        let second = parse_row(line, &header).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.amount_units, -4200, "Haben is negative");
    }
}
