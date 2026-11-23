// Included under the caller's TribleSpace prelude: no desktop runtime dependency.
// Existing node-schema identities minted 2026-10-06, moved without changing encoding.
// config_anchor is the existing Faculty config::anchor (minted 2026-09-22).
pub const NODE_SETTINGS: Id = id_hex!("E0A9165929ABA080F204EB244CD49C3A");
attributes! {
    "74027DA8641D5E3EECDC59089BF5D989" as config_anchor: inlineencodings::GenId;
    /// Desired replication state. Absence is not permission to start.
    "BD42BDE1DA2C76EF7A388966D8990863" as sync_enabled: inlineencodings::Boolean;
    /// Desired maintenance state, independent of replication.
    "7EFEF8B8D2A71A97F8363EDD889D1EE9" as maintenance_enabled: inlineencodings::Boolean;
    /// Loopback MCP listener port (zero asks the OS in fixtures).
    "FA1ABA337B3E770DA0A8263670494518" as mcp_port: inlineencodings::U256BE;
}
