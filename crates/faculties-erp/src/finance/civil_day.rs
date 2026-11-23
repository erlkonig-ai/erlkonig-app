//! Sortable Gregorian civil days, independent of time zones and instants.
//!
//! Canonical bytes: year as u16 big-endian at 0..2, month at 2, day at 3,
//! then 28 zero bytes. Domain: Gregorian CE years 1..9999, valid calendar
//! month/day only. Lexicographic byte order is chronological order.
//! The encoded value represents a civil date independent of any timezone.
use std::fmt;

use anyhow::{bail, Result};
use triblespace::core::inline::Encodes;
use triblespace::core::metadata;
use triblespace::macros::id_hex;
use triblespace::prelude::*;

/// A civil day, without an invented timezone or instant. Presented canonically
/// as YYYY-MM-DD; CivilDay stores sortable calendar bytes. Validation belongs to this writer, not to collection reads.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct CivilDate(String);

impl CivilDate {
    pub fn parse(text: &str) -> Result<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes
                .iter()
                .enumerate()
                .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
        {
            bail!("expected civil date YYYY-MM-DD, got {text:?}");
        }
        let year: u32 = text[..4].parse()?;
        let month: u32 = text[5..7].parse()?;
        let day: u32 = text[8..].parse()?;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            2 => {
                if leap {
                    29
                } else {
                    28
                }
            }
            4 | 6 | 9 | 11 => 30,
            1..=12 => 31,
            _ => 0,
        };
        if year == 0 || day == 0 || day > days {
            bail!("invalid civil date {text:?}");
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CivilDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub struct CivilDay;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidCivilDay;
impl fmt::Display for InvalidCivilDay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid Gregorian CE civil day or nonzero padding")
    }
}
impl std::error::Error for InvalidCivilDay {}

impl MetaDescribe for CivilDay {
    fn describe() -> Fragment {
        let id = id_hex!("B7E54DD404EC12804AB277F33CB4CA21");
        entity! { ExclusiveId::force_ref(&id) @
            metadata::tag: metadata::KIND_INLINE_ENCODING,
            metadata::name: "erp/civil-day",
            metadata::description: "Gregorian CE civil day, years 1..9999. Bytes 0..2: u16 big-endian year; byte 2: month; byte 3: day; bytes 4..32: zero. Canonical calendar dates only. Byte ordering is chronological; no timezone or instant is implied.",
        }
    }
}

impl InlineEncoding for CivilDay {
    type ValidationError = InvalidCivilDay;
    type Encoding = Self;
    fn validate(value: Inline<Self>) -> std::result::Result<Inline<Self>, InvalidCivilDay> {
        CivilDate::try_from_inline(&value)?;
        Ok(value)
    }
}

impl Encodes<&CivilDate> for CivilDay {
    type Output = Inline<Self>;
    fn encode(date: &CivilDate) -> Self::Output {
        // CivilDate's private representation is created only after validation.
        let year: u16 = date.as_str()[..4].parse().expect("validated year");
        let month: u8 = date.as_str()[5..7].parse().expect("validated month");
        let day: u8 = date.as_str()[8..].parse().expect("validated day");
        let mut bytes = [0u8; 32];
        bytes[..2].copy_from_slice(&year.to_be_bytes());
        bytes[2] = month;
        bytes[3] = day;
        Inline::new(bytes)
    }
}
impl Encodes<CivilDate> for CivilDay {
    type Output = Inline<Self>;
    fn encode(date: CivilDate) -> Self::Output {
        Self::encode(&date)
    }
}
impl TryFromInline<'_, CivilDay> for CivilDate {
    type Error = InvalidCivilDay;
    fn try_from_inline(value: &Inline<CivilDay>) -> std::result::Result<Self, Self::Error> {
        if value.raw[4..].iter().any(|byte| *byte != 0) {
            return Err(InvalidCivilDay);
        }
        let year = u16::from_be_bytes([value.raw[0], value.raw[1]]);
        let text = format!("{year:04}-{:02}-{:02}", value.raw[2], value.raw[3]);
        Self::parse(&text).map_err(|_| InvalidCivilDay)
    }
}

/// Inclusive byte boundary immediately before a valid date. This may not be a
/// valid CivilDay (padding can be nonzero): QUERY SENTINEL ONLY, never persist.
pub(crate) fn before(date: &CivilDate) -> Inline<CivilDay> {
    let mut value: Inline<CivilDay> = date.to_inline();
    for byte in value.raw.iter_mut().rev() {
        let (next, borrow) = byte.overflowing_sub(1);
        *byte = next;
        if !borrow {
            return value;
        }
    }
    unreachable!("CE minimum is greater than the all-zero byte boundary")
}

/// Inclusive byte boundary immediately after a valid date. Query sentinel
/// only: the low padding byte becomes 1 and intentionally does not decode.
pub(crate) fn after(date: &CivilDate) -> Inline<CivilDay> {
    let mut value: Inline<CivilDay> = date.to_inline();
    value.raw[31] = 1;
    value
}
