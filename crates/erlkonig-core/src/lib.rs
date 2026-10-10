//! Native services owned by the one Erlkonig application process.

pub mod config;
mod config_mcp;
pub mod credentials;
pub mod init;
pub mod install;
pub mod migration;
pub mod node_cli;
pub mod node_metrics;
pub mod node_schema;
pub mod node_store;
pub mod paths;
mod resource_guard;
pub mod runtime;
pub mod status;
pub mod transport;
pub mod words;
pub mod workspace;
pub mod workspace_dispatch;
