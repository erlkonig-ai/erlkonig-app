//! Erlkönig facts in the node pile's existing private `config` collection.
//! The Faculty `config::selects` attribute keeps its collection-handle meaning;
//! these application settings do not pretend to be collection selections.
//!
//! Anchors below were minted verbatim with installed `trible genid`, 2026-10-06.
//! Attribute identity also incorporates the declared encoding. Entity IDs are
//! opaque references, never reconstructed or validated by readers.

use triblespace::macros::id_hex;
use triblespace::prelude::*;

/// Node-local settings register, not a managed data pile.
pub const NODE_SETTINGS: Id = id_hex!("E0A9165929ABA080F204EB244CD49C3A");
/// A revision of one explicitly enrolled managed pile.
pub const MANAGED_PILE: Id = id_hex!("55BCBF80A24F613FF984BD79DF1D6360");

attributes! {
    /// Explicit local path to an existing managed pile; not an ownership claim.
    "1174FD9344FA43249919BC7A25471781" as pile_path: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Explicit path to a key, never secret key bytes.
    "ACE310E14E0F6E3252CBF161BDCE597B" as key_path: inlineencodings::Handle<blobencodings::UTF8String>;
    /// Desired replication state. Absence is not permission to start.
    "BD42BDE1DA2C76EF7A388966D8990863" as sync_enabled: inlineencodings::Boolean;
    /// Desired maintenance state, independent of replication.
    "7EFEF8B8D2A71A97F8363EDD889D1EE9" as maintenance_enabled: inlineencodings::Boolean;
    /// Exact collection descriptors explicitly selected for replication.
    "A7519375BC8C63CD37BB51141D384224" as sync_collection: inlineencodings::Handle<blobencodings::SimpleArchive>;
    /// Exact targets explicitly selected for local maintenance.
    "F9D2C0AC419D212A1C69A5CDFD4D1387" as maintenance_target: inlineencodings::Handle<blobencodings::SimpleArchive>;
    /// Loopback MCP listener port (zero asks the OS in fixtures).
    "FA1ABA337B3E770DA0A8263670494518" as mcp_port: inlineencodings::U256BE;
    /// Measurement subject -> enrolled managed-pile identity, when applicable.
    "C39028980C8DCF8219B07396CCC3B1C3" as managed_pile: inlineencodings::GenId;
    /// Explicit enrollment or withdrawal of a managed entry. False retains
    /// history but withdraws the node's request to manage it.
    "6029BC6E30BF94DB16A5D4C96FF6B4E8" as enrolled: inlineencodings::Boolean;
}
