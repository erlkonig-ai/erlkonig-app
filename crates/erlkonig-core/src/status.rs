//! In-memory application status. The UI never guesses liveness from another PID.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    #[default]
    Disabled,
    Starting,
    Running,
    Stopping,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupFailure {
    EndpointShutdownFailed,
    EndpointShutdownTimeout,
    StoreCloseFailed,
}

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
    pub services: ServiceState,
    /// The initiating cancellation, kept independently of session/cleanup errors.
    pub stop_reason: Option<String>,
    /// A replacement stopped before readiness; the original cause stays above.
    pub retry_stop_reason: Option<String>,
    pub cleanup_failures: Vec<CleanupFailure>,
    pub retry_pending: bool,
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
        let sync = if self.node.services == ServiceState::Stopped {
            "services stopped · Retry services to recover"
        } else if self.node.services == ServiceState::Stopping {
            "services stopping"
        } else if self.node.services == ServiceState::Starting {
            "services starting"
        } else if self.node.sync_running {
            "syncing accessible collections"
        } else {
            "sync paused"
        };
        let mut text = if self.running {
            format!("Running · {sync} · {}", if self.problem.is_none() && self.mcp_url.is_some() {
                "authenticated MCP"
            } else { "MCP unavailable" })
        } else { "Erlkönig is stopped".into() };
        if self.node.maintenance_running {
            text.push_str(" · maintenance running");
        }
        if let Some(reason) = &self.node.stop_reason {
            text.push_str(&format!(" · {reason}"));
        }
        if let Some(reason) = &self.node.retry_stop_reason {
            text.push_str(&format!(" · retry stopped: {reason}"));
        }
        if let Some(problem) = &self.node.problem {
            text.push_str(&format!(" · {problem}"));
        }
        if let Some(problem) = &self.problem {
            text.push_str(&format!(" · Problem: {problem}"));
        }
        if let Some(problem) = &self.config_problem {
            text.push_str(&format!(" · Configuration: {problem}"));
        }
        if let Some(problem) = &self.metrics_problem {
            text.push_str(&format!(" · metrics could not be saved: {problem}"));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_failures_do_not_hide_stopped_service_diagnostics() {
        let status = Status {
            running: true, mcp_url: Some("http://127.0.0.1:1/".into()),
            config_problem: Some("bad config".into()),
            metrics_problem: Some("metrics append failed".into()),
            node: NodeStatus {
                services: ServiceState::Stopped,
                stop_reason: Some("original stop".into()),
                retry_stop_reason: Some("retry guard refused".into()),
                problem: Some("endpoint cleanup failed".into()),
                ..NodeStatus::default()
            }, ..Status::default()
        };
        let text = status.summary();
        for expected in ["services stopped", "authenticated MCP", "original stop",
            "retry guard refused", "endpoint cleanup failed", "bad config", "metrics append failed"] {
            assert!(text.contains(expected), "{text}");
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
