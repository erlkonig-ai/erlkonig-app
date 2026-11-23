//! Strict German Amex activity CSV trust boundary. No portal, statement PDF,
//! account guessing, economic deduplication, or classification inference.

use crate::revolver::schema::Euros;
use anyhow::{bail, ensure, Context, Result};

pub const HEADERS: [&str; 11] = [
    "Datum",
    "Beschreibung",
    "Betrag",
    "Weitere Details",
    "Erscheint auf Ihrer Abrechnung als",
    "Adresse",
    "Stadt",
    "PLZ",
    "Land",
    "Betreff",
    "Kategorie",
];

/// Parsing DTO only; never a materialized view of a pile.
#[derive(Debug, Clone)]
pub struct Row {
    pub index: u64,
    pub date: String,
    pub amount: Euros,
    pub description: String,
    pub details: String,
    pub provider_reference: String,
    pub cardholder: Option<String>,
    pub card_number: Option<String>,
}

/// Explicit calendar selection, including a validated Gregorian boundary.
#[derive(Clone, Copy, Debug)]
pub struct Month {
    year: u16,
    month: u8,
}

impl Month {
    pub fn parse(value: &str) -> Result<Self> {
        ensure!(
            value.len() == 7 && value.as_bytes()[4] == b'-',
            "month must be YYYY-MM"
        );
        ensure!(
            value[..4]
                .bytes()
                .chain(value[5..].bytes())
                .all(|b| b.is_ascii_digit()),
            "month must be YYYY-MM"
        );
        let year = value[..4].parse()?;
        let month = value[5..].parse()?;
        ensure!(
            (1..=9999).contains(&year) && (1..=12).contains(&month),
            "invalid month"
        );
        Ok(Self { year, month })
    }
    pub fn contains(self, iso: &str) -> bool {
        iso.starts_with(&format!("{:04}-{:02}-", self.year, self.month))
    }
    pub fn start(self) -> String {
        format!("{:04}-{:02}-01", self.year, self.month)
    }
    pub fn end(self) -> String {
        let (year, month) = if self.month == 12 {
            (self.year + 1, 1)
        } else {
            (self.year, self.month + 1)
        };
        format!("{year:04}-{month:02}-01")
    }
}

/// Exact signed decimal comma; never accept locale guesses, grouping or floats.
pub fn euros(value: &str) -> Result<Euros> {
    Ok(Euros::from_units(decimal_comma_hundredths(value)?, 2)?)
}

/// Exact source decimal with two fractional digits, before any denomination.
/// Both the EUR card export and explicitly denominated invoice inputs use it.
pub(crate) fn decimal_comma_hundredths(value: &str) -> Result<i128> {
    let (negative, magnitude) = match value.strip_prefix('-') {
        Some(v) => (true, v),
        None => (false, value),
    };
    let (whole, cents) = magnitude
        .split_once(',')
        .context("amount requires decimal comma and two fractional digits")?;
    ensure!(
        !whole.is_empty()
            && cents.len() == 2
            && whole
                .bytes()
                .chain(cents.bytes())
                .all(|b| b.is_ascii_digit()),
        "invalid exact amount"
    );
    let units = whole
        .parse::<i128>()
        .context("amount is outside supported domain")?
        .checked_mul(100)
        .and_then(|n| n.checked_add(cents.parse::<i128>().ok()?))
        .context("amount is outside supported domain")?;
    Ok(if negative { -units } else { units })
}

pub fn iso_date(value: &str) -> Result<String> {
    ensure!(
        value.len() == 10 && value.as_bytes()[2] == b'/' && value.as_bytes()[5] == b'/',
        "date must be DD/MM/YYYY"
    );
    ensure!(
        value
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 2 || i == 5 || b.is_ascii_digit()),
        "date must be DD/MM/YYYY"
    );
    let day: u8 = value[..2].parse()?;
    let month: u8 = value[3..5].parse()?;
    let year: u16 = value[6..].parse()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    ensure!(year > 0 && day > 0 && day <= days, "invalid Gregorian date");
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// Parse ALL records before selecting the month: malformed outside-period input
/// is not silently accepted. Original bytes (including other months) are retained.
pub fn parse(bytes: &[u8], currency: &str, month: Month) -> Result<Vec<Row>> {
    ensure!(
        currency == "EUR",
        "explicit currency must be EUR; no other currency is modelled"
    );
    let text = std::str::from_utf8(bytes).context("Amex CSV must be UTF-8")?;
    ensure!(
        !text.starts_with('\u{feff}'),
        "Amex CSV BOM is not supported"
    );
    let records = records(text)?;
    let headers = records.first().context("empty Amex CSV")?;
    let extended = headers.len() == 13
        && headers
            .iter()
            .filter(|h| h.as_str() == "Karteninhaber")
            .count()
            == 1
        && headers.iter().filter(|h| h.as_str() == "Konto #").count() == 1
        && headers
            .iter()
            .filter(|h| !["Karteninhaber", "Konto #"].contains(&h.as_str()))
            .map(String::as_str)
            .eq(HEADERS);
    ensure!(
        headers.iter().map(String::as_str).eq(HEADERS) || extended,
        "Amex CSV header does not match the supported German activity export"
    );
    let positions: Vec<usize> = HEADERS
        .iter()
        .map(|h| headers.iter().position(|v| v == h).unwrap())
        .collect();
    let holder = headers.iter().position(|h| h == "Karteninhaber");
    let number = headers.iter().position(|h| h == "Konto #");
    let width = headers.len();
    let mut rows = Vec::new();
    for (index, record) in records.into_iter().skip(1).enumerate() {
        ensure!(
            record.len() == width,
            "CSV data record {} has the wrong field count",
            index + 1
        );
        let fields: Vec<String> = positions.iter().map(|&p| record[p].clone()).collect();
        let date =
            iso_date(&fields[0]).with_context(|| format!("CSV data record {} date", index + 1))?;
        let amount =
            euros(&fields[2]).with_context(|| format!("CSV data record {} amount", index + 1))?;
        if month.contains(&date) {
            rows.push(Row {
                index: (index + 1) as u64,
                date,
                amount,
                description: fields[1].clone(),
                details: fields[3].clone(),
                provider_reference: fields[9].clone(),
                cardholder: holder.map(|p| record[p].clone()),
                card_number: number.map(|p| record[p].clone()),
            });
        }
    }
    Ok(rows)
}

/// RFC-style comma/double-quote records with strict quote placement. Unlike
/// permissive CSV readers, a quote in a bare field and text after a closing
/// quote are errors. Embedded LF/CRLF and doubled quotes are preserved.
fn records(text: &str) -> Result<Vec<Vec<String>>> {
    #[derive(Clone, Copy)]
    enum State {
        Start,
        Bare,
        Quoted,
        Closed,
    }
    let mut state = State::Start;
    let mut field = String::new();
    let mut record = Vec::new();
    let mut records = Vec::new();
    let mut chars = text.chars().peekable();
    let mut pending = false;
    while let Some(ch) = chars.next() {
        pending = true;
        match (state, ch) {
            (State::Quoted, '"') => state = State::Closed,
            (State::Quoted, ch) => field.push(ch),
            (State::Closed, '"') => {
                field.push('"');
                state = State::Quoted;
            }
            (State::Start, '"') => state = State::Quoted,
            (State::Bare, '"') => bail!("quote in unquoted CSV field"),
            (_, ',') => {
                record.push(std::mem::take(&mut field));
                state = State::Start;
            }
            (_, '\n' | '\r') => {
                if ch == '\r' {
                    ensure!(chars.next() == Some('\n'), "bare CR outside a quoted field");
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                state = State::Start;
                pending = false;
            }
            (State::Closed, _) => bail!("text after closing CSV quote"),
            (_, ch) => {
                field.push(ch);
                state = State::Bare;
            }
        }
    }
    ensure!(
        !matches!(state, State::Quoted),
        "unterminated quoted CSV field"
    );
    if pending {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn csv(rows: &str) -> String {
        format!("{}\n{rows}", HEADERS.join(","))
    }
    #[test]
    fn strict_quotes_and_newlines() {
        let input = csv("01/09/2026,\"shop, \"\"quoted\"\"\nname\",\"7,05\",,,,,,,ref,\n");
        let rows = parse(input.as_bytes(), "EUR", Month::parse("2026-09").unwrap()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].description, "shop, \"quoted\"\nname");
        let fx = csv(
            "01/09/2026,Synthetic shop,\"7,05\",Original FX USD 8.19 / rate 1.162,,,,,,fx-ref,\n",
        );
        let rows = parse(fx.as_bytes(), "EUR", Month::parse("2026-09").unwrap()).unwrap();
        assert_eq!(rows[0].details, "Original FX USD 8.19 / rate 1.162");
        for bad in ["a\"b,c", "\"a\"x,c", "\"a", "a\rb"] {
            assert!(records(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn exact_signed_euros_and_month_boundary() {
        assert_eq!(
            euros("-12,34").unwrap(),
            Euros::from_units(-1234_i128, 2).unwrap()
        );
        for bad in [
            "8.96", " 7,05", "+7,05", "1.000,00", "7,0", "7,050", "(7,05)", "1e2,00",
        ] {
            assert!(euros(bad).is_err(), "{bad}");
        }
        assert!(iso_date("29/02/2026").is_err());
        assert!(iso_date("29/02/2024").is_ok());
        let input = csv("31/08/2026,a,\"1,00\",,,,,,,r1,\n01/09/2026,b,\"-2,00\",,,,,,,r2,\n30/09/2026,c,\"3,00\",,,,,,,r3,\n01/10/2026,d,\"4,00\",,,,,,,r4,\n");
        let rows = parse(input.as_bytes(), "EUR", Month::parse("2026-09").unwrap()).unwrap();
        assert_eq!(rows.iter().map(|r| r.index).collect::<Vec<_>>(), [2, 3]);
        assert!(parse(input.as_bytes(), "USD", Month::parse("2026-09").unwrap()).is_err());
    }
    #[test]
    fn malformed_unicode_and_calendar_inputs_never_panic() {
        for value in [
            "",
            "éé-09",
            "2026-💰",
            "💰-09",
            "2026-00",
            "2026-13",
            "0000-01",
            "２０２６-09",
        ] {
            assert!(
                std::panic::catch_unwind(|| Month::parse(value))
                    .unwrap()
                    .is_err(),
                "{value}"
            );
        }
        for value in [
            "é/09/2026",
            "01/💰/26",
            "01/09/💰",
            "00/09/2026",
            "31/09/2026",
            "01/13/2026",
        ] {
            assert!(
                std::panic::catch_unwind(|| iso_date(value))
                    .unwrap()
                    .is_err(),
                "{value}"
            );
        }
    }
}
