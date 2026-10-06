//! In-memory application status. The UI never guesses liveness from another PID.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub running: bool,
    pub node_id: Option<String>,
    pub mcp_url: Option<String>,
    pub problem: Option<String>,
    pub warnings: Vec<String>,
    pub sync_collections: usize,
    pub maintenance_targets: usize,
    /// Actual payload bytes transferred. Connection/idle events never advance it.
    pub transfer_bytes: u64,
    pub maintenance_publications: u64,
}

impl Status {
    pub fn summary(&self) -> String {
        if let Some(problem) = &self.problem {
            return format!("Problem: {problem}");
        }
        if !self.running {
            return "Erlkönig is stopped".into();
        }
        let sync = if self.sync_collections == 0 {
            "sync off".into()
        } else {
            format!("{} sync collections", self.sync_collections)
        };
        format!("Running · {sync} · authenticated MCP")
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
