//! Erlkonig facts in the node pile's existing private `config` collection.
//! The Faculty `config::selects` attribute keeps its collection-handle meaning;
//! workspace role selections use it only for exact collection handles.
//!
//! Anchors below were minted verbatim with installed `trible genid`, 2026-10-06.
//! Attribute identity also incorporates the declared encoding. Entity IDs are
//! opaque references, never reconstructed or validated by readers.

use triblespace::macros::id_hex;
use triblespace::prelude::*;

/// Node-local settings register, not a managed data pile.
pub const NODE_SETTINGS: Id = id_hex!("E0A9165929ABA080F204EB244CD49C3A");
/// Node-local native health publication destination. Minted with `trible genid` on 2026-11-18.
pub const HEALTH_REPORTING: Id = id_hex!("3A85799153CD8CB38ABF582C4EFB6B7E");
/// Node-local sync telemetry destination and worker. Minted with `trible genid` on 2026-11-18.
pub const SYNC_TELEMETRY: Id = id_hex!("0AB2536130D68525125C9B550DD40F8E");
/// A revision of one explicitly enrolled managed pile.
pub const MANAGED_PILE: Id = id_hex!("55BCBF80A24F613FF984BD79DF1D6360");
/// An explicit query workspace revision, separate from service enrollment.
/// Minted with `trible genid` on 2026-10-08.
pub const WORKSPACE: Id = id_hex!("F4D15E6C2AA3263C1B6DBDD0D5880539");

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
    /// Workspace revision -> role selection (config::anchor + config::selects).
    /// Minted with `trible genid` on 2026-10-08.
    "57DCA7F297BF848DA19CFD2730D6293E" as workspace_route: inlineencodings::GenId;
    /// Public identity expected when loading the explicitly selected key file.
    /// Not an attestation author or an admission-policy authority.
    /// Minted with `trible genid` on 2026-10-08.
    "D461447F2732ABF6FF9E68AE2528BF8F" as expected_signer: inlineencodings::ED25519PublicKey;
    /// On a workspace collection descriptor: the faculty role (scope ID) the
    /// bundle created it for. A grouping hint for enrollment, never authority
    /// or a selection; readers without it fall back to the descriptor name.
    /// Minted with `trible genid` on 2026-10-13.
    "DDE29291E38207495A90FE4FF1204DB4" as collection_role: inlineencodings::GenId;
}
