//! Shared financial observation vocabulary.
//!
//! Stable attribute identity combines the declared anchor and encoding.
//!
//! These five kinds are writer views, not a closed-world store schema.
//! Amounts use one anchor with Currency<C>; currency is in the encoding.

use super::civil_day::CivilDay;
use triblespace::core::attribute::Attribute;
use triblespace::core::inline::encodings::money::{Currency, CurrencyUnit};
use triblespace::macros::id_hex;
use triblespace::prelude::*;
pub const KIND_STOCK: Id = id_hex!("E540A95D338672636481A92A71272F20");
pub const KIND_TRANSFER: Id = id_hex!("F94C73933EC256DF060683553664D8B1");
pub const KIND_CAPACITY: Id = id_hex!("E2043A5DF6564FC11CD4629DC2F99D0B");
pub const KIND_COMMITMENT: Id = id_hex!("8F2ED280E91265606AC949BEE81CA0BE");
pub const KIND_PERIOD_TOTAL: Id = id_hex!("D9C96BB478677FA4CE2EAE9200F795D1");
pub const AMOUNT_ANCHOR: Id = id_hex!("63C57FD14813F9DD0F7C88C1EADA36FE");

/// Shared quantity attribute, with exact signed rational money in currency C.
pub fn amount<C: CurrencyUnit>() -> Attribute<Currency<C>> {
    Attribute::anchored(AMOUNT_ANCHOR)
}

attributes! {
    /// Recording receipt -> asserted observation; preserves actor/time pairing.
    "5C389723720A6839FF527F2C3C831869" as pub recording_of: inlineencodings::GenId;
    /// Exact provider/source event-kind wording; no inferred financial category.
    "284EDE2FBBDE1D12A6404B55EB3D2F16" as pub source_kind_wording: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Civil date reported for a source-specific event; not a settlement date.
    "8B24F29D34B1C2C7494C3B2C03DBCD47" as pub source_event_date: CivilDay;
    /// Subject of this observation; an opaque organization, account, or other entity ID.
    "41C4E51B88FB3A346270008A00C3367F" as pub entity: inlineencodings::GenId;
    /// Optional shared measure identity; never inferred from a label.
    "4354EF247CDCDED3B5AE8798D092801A" as pub measure: inlineencodings::GenId;
    /// Exact source wording for what the quantity measures (e.g. increase versus level).
    "883A964F3630AB0DAB527B4F5E9D813A" as pub measure_label: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Optional identified source issuer, distinct from the recording actor.
    "A14097954868C42AABC84750B0100728" as pub claimant: inlineencodings::GenId;
    /// Source issuer wording, retained even when identity is unknown.
    "CFF93AD5DF77FB92B5E2866C2174728B" as pub claimant_label: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Exact source entity (e.g. an ERP snapshot); not the observation subject.
    "E382AC6E6109A87A9EFA29A68C57F7DA" as pub evidence: inlineencodings::GenId;
    /// Exact page, table, row, or other locator within the cited source.
    "DAEB2B9E93892CE087C5F82D194ADAA9" as pub locator: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Quoted organizational or accounting scope; absence does not imply consolidation.
    "DD9FFFCE6C697D13B73F0C0FDCCDA342" as pub scope: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Quoted measurement/accounting basis; no inferred cash/accrual or gross/net basis.
    "C7156D9362E91C15450D34E28919204E" as pub basis: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Attributed source classification, including legal wording; not an adjudication.
    "A7544F70917C1A5B151F98DFA7857FBC" as pub classification: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Stock measurement date, civil YYYY-MM-DD; not publication or recording time.
    "FD8310B59A73882CAD94D960BEB41977" as pub as_of: CivilDay;
    /// Claimed transfer date, civil YYYY-MM-DD.
    "06A136D539D4A870070F8A3F18028EDD" as pub occurred: CivilDay;
    /// Claimed transfer sender; never inferred from a documentary match.
    "B93B5C3FF0C17728EBCE8B4E0C9A4B2B" as pub sender: inlineencodings::GenId;
    /// Claimed transfer receiver; never inferred from a documentary match.
    "93C3FD1D8152928A38587A9AB1882D45" as pub receiver: inlineencodings::GenId;
    /// Inclusive civil reporting-period boundary, when explicitly known.
    "953A2DF31442B58AC1F074F246E85E72" as pub period_start: CivilDay;
    /// Exclusive civil reporting-period boundary, when explicitly known.
    "CBC15E034C149744E4E0E16D4D8FC5EE" as pub period_end: CivilDay;
    /// Original reporting-period wording; does not manufacture boundaries.
    "2CD007AA789470A2F23CA89FFA3F4D24" as pub period_label: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Commitment due date, when explicitly known.
    "B407F1C09C61B08B646105014927128F" as pub due: CivilDay;
    /// Recording actor, an existing Relations persona ID; separate from issuer.
    "7DF9275C5649565191E78EBE5FA41DB9" as pub recorded_by: inlineencodings::GenId;
    /// Original figure wording, including unsupported currency or uncertainty.
    "A4ABB7F8E37DB4AEE48942F8A2F78374" as pub amount_wording: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Exact URI of cited source; source content need not be fetched by this writer.
    "1BB92CA75B4D6DE4045D10A8FE80D2B2" as pub source_uri: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Source publication date, separate from reporting period.
    "90C5732F404EE0918E24E238BA3314A3" as pub publication_date: CivilDay;
    /// When the source was observed, separate from recording this assertion.
    "78C1F4F69DE0CEAD998DD71324C94EE8" as pub source_observed_at: inlineencodings::NsTAIInterval;
    /// Original source date wording, including uncertainty.
    "8C30603DE258255712FCD186327BB98F" as pub source_date_wording: inlineencodings::Handle<blobencodings::UTF8String>;
}
