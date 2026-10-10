//! In-memory application status. The UI never guesses liveness from another PID.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeStatus {
    pub pile: String,
    pub key: String,
    pub node_id: Option<String>,
    pub bound_sockets: Vec<String>,
    pub sync_enabled: bool,
    pub maintenance_enabled: bool,
    pub sync_running: bool,
    pub maintenance_running: bool,
    pub problem: Option<String>,
    pub transfer_bytes: u64,
    pub maintenance_publications: u64,
    pub sessions_started: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub running: bool,
    pub node_id: Option<String>,
    pub mcp_url: Option<String>,
    pub problem: Option<String>,
    pub warnings: Vec<String>,
    pub sync_collections: usize,
    pub maintenance_targets: usize,
    /// App-owned reconciliation, index maintenance and executing Faculty calls.
    /// Overlapping scopes count independently; idle service loops do not count.
    pub active_work: usize,
    /// Actual payload bytes transferred. Connection/idle events never advance it.
    pub transfer_bytes: u64,
    pub maintenance_publications: u64,
    pub node: NodeStatus,
    pub config_generation: u64,
    pub config_problem: Option<String>,
    pub metrics_problem: Option<String>,
}

impl Status {
    pub fn summary(&self) -> String {
        if let Some(problem) = &self.problem {
            return format!("Problem: {problem}");
        }
        if let Some(problem) = &self.config_problem {
            return format!("Configuration: {problem}");
        }
        if !self.running {
            return "Erlkönig is stopped".into();
        }
        if self.metrics_problem.is_some() {
            return "Running · metrics could not be saved".into();
        }
        let sync = if self.node.sync_running {
            "syncing accessible collections"
        } else {
            "sync paused"
        };
        if let Some(problem) = &self.node.problem {
            format!("Running · {sync} · {problem}")
        } else {
            format!("Running · {sync} · authenticated MCP")
        }
    }
}

/// Writes a private sibling then renames. Never follows a pre-existing temp link.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?;
    let nonce = getrandom::u64().map_err(io::Error::other)?;
    let tmp = path.with_file_name(format!(".{}.tmp-{nonce:x}", name.to_string_lossy()));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
