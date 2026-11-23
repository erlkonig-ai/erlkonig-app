//! Source-neutral evidence and review claims; no payment implies an expense purpose.
//!
//! The USD gross total uses the purchase amount anchor with USD encoding.

use triblespace::prelude::*;

pub const KIND_ACCOUNT: Id = id_hex!("60FAC139F54FBEB82824D0779F5E0CEB");
pub const KIND_ACTIVITY: Id = id_hex!("1469DA1088ECB38F0E7970A120CE7472");
pub const KIND_INVOICE: Id = id_hex!("9E34275E29C6C2153CBA565EDFCF4E53");
pub const KIND_PURPOSE: Id = id_hex!("E124613DF7FD681463E8F53AD82DE8A9");
pub const KIND_MATCH: Id = id_hex!("F828AD50A503F6F762F673AE3D1EA57C");
/// One immutable attributed assertion of correlated document metadata.
pub const KIND_INVOICE_METADATA: Id = id_hex!("8BBE827DCE180ED682936E59377C1200");
pub const COMPANY: Id = id_hex!("773F3FB981931C8F9B376D844DF1E345");
pub const PRIVATE: Id = id_hex!("E8BD2C695FE4A35DBB3B848A8C26CF07");
/// Minted with `trible genid` 2026-10-22, together with the observation kind
/// and seven attribute anchors below. Exact mint receipt is in this commit.
/// Provider-record convention: opaque card account + verbatim nonblank reference.
/// Not a guarantee that Amex references universally identify economic events.
pub const KIND_AMEX_TRANSACTION: Id = id_hex!("4AED7D9458C3D5FB5720B78D227EBEAD");
/// Correlated semantic observation, independent of CSV column layout.
pub const KIND_AMEX_OBSERVATION: Id = id_hex!("3DEAFD7337D256C74711AEB10A70CB3A");

attributes! {
    /// The transaction described by this correlated immutable observation.
    "FAF00B193B46ABCA85EA1913B8E292C9" as pub transaction: inlineencodings::GenId;
    /// Source occurrence supports this business observation.
    "A6018FD7C02F1E852BBC737E1A486CFE" as pub observed: inlineencodings::GenId;
    /// Verbatim Karteninhaber; source evidence, never inferred account identity.
    "E8B2B864E9EF87DC4CEAEB5C6D2C32E5" as pub cardholder: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Verbatim Konto #; not a globally unique account identifier.
    "81A682EE09D39900D83D41F694F17697" as pub card_number: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Legacy occurrence explicitly associated with a transaction by migration.
    "D0D138B93B73E319EE7D81BB7DA55298" as pub legacy_activity: inlineencodings::GenId;
    /// Original decision projected onto the new subject, not a new decision.
    "1604B06869006DDAC386C75D63EB87D0" as pub original_claim: inlineencodings::GenId;
    /// The exact source evidence selected for an explicit migration assertion.
    "A51E053A0E3871C7EC8330AC388BBFB2" as pub migration_evidence: inlineencodings::GenId;
    /// Original invoice gross total in USD. Reuses purchase::gross_total's
    /// existing semantic anchor; Currency<UsDollar> derives a distinct attribute
    /// from its EUR sibling without changing or relabelling historical facts.
    "42A144ED37608BC3FC8DE4467E94EB36" as pub gross_total_usd: inlineencodings::Currency<triblespace::core::inline::encodings::money::UsDollar>;
    /// Opaque account entity. Labels and provider references never identify it.
    "FFE198F9F6D9095C00FFFDF01F87895F" as pub account: inlineencodings::GenId;
    /// Exact original bytes, part of source-occurrence identity, not transaction identity.
    "B41A9A6FAFE364E7220B3E3B21D590A6" as pub source_bytes: inlineencodings::Handle<blobencodings::RawBytes>;
    /// Canonical Files entity describing these original bytes. Repeated provenance.
    "754E9852F8DC4F1F31CC1AA2425CEB5D" as pub source_file: inlineencodings::GenId;
    /// One-based CSV data-record index, not physical line number (quoted newlines exist).
    "6D05C3701E7098112BE46EFED62964A7" as pub row_index: inlineencodings::U256BE;
    /// Provider's verbatim Betreff. Amex record cores scope it by opaque account;
    /// this convention does not prove universal economic equivalence.
    "6199A5DB7E5A80D8366CE2142A93926F" as pub provider_reference: inlineencodings::Handle<blobencodings::UTF8String>;
    /// The whole activity or invoice this purpose claim describes.
    "1E75C967BCE4C70C1B1C4772A228A108" as pub purpose_of: inlineencodings::GenId;
    /// Whole company or whole private. No fact means unresolved, not private.
    "7E2AF2D6CF63F83AFCC91167B1CD98A9" as pub purpose: inlineencodings::GenId;
    /// The observed card activity an explicit match claim describes.
    "98287F7568149F558525CB3A3DD88FBB" as pub activity: inlineencodings::GenId;
    /// Invoice described by a documentary match or metadata observation.
    "A46EC023CA59539BAACEBAEA7E17A949" as pub invoice: inlineencodings::GenId;
}
