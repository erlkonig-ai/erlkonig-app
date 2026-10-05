//! `D/node.status`: what the node is doing, rewritten atomically every few
//! seconds and read by the menu-bar UI (and by anyone with `cat`).

use crate::{procs, words};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const FORMAT: u32 = 1;
/// A status older than this is stale: the node that wrote it is gone or hung.
pub const STALE_AFTER_SECS: u64 = 20;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeStatus {
    pub format: u32,
    pub pid: u32,
    /// "running" or "stopped".
    pub state: String,
    pub started_at: u64,
    pub updated_at: u64,
    pub updated: String,
    pub data_dir: PathBuf,
    pub pile: Option<PathBuf>,
    pub key: Option<PathBuf>,
    /// The public key of the node's signing key, 64 hex.
    pub node_id: Option<String>,
    /// Why the node cannot do what node.toml asks, if it cannot.
    pub problem: Option<String>,
    pub configured_peers: usize,
    pub sync_collections: usize,
    pub maintain_targets: usize,
    pub children: Vec<ChildStatus>,
    pub peers: Vec<PeerStatus>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChildStatus {
    pub name: String,
    /// "running" or "waiting" (backing off before the next start).
    pub state: String,
    pub pid: Option<u32>,
    pub program: PathBuf,
    pub running_since: Option<u64>,
    pub restarts: u32,
    pub last_exit: Option<String>,
    pub last_error: Option<String>,
    pub next_start_in_secs: Option<u64>,
    pub log: PathBuf,
}

/// The last `reconciled with peer <hex>: <n> collections` line per peer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerStatus {
    pub peer: String,
    pub collections: u64,
    pub at: u64,
    pub line: String,
}

/// Parses the stable sync line `reconciled with peer <hex>: <n> collections`.
pub fn parse_reconciled(line: &str) -> Option<(String, u64)> {
    let rest = &line[line.find("reconciled with peer ")? + "reconciled with peer ".len()..];
    let (peer, rest) = rest.split_once(':')?;
    let n = rest.trim_start().split_whitespace().next()?.parse().ok()?;
    let peer = peer.trim();
    (!peer.is_empty() && !peer.contains(char::is_whitespace)).then(|| (peer.to_string(), n))
}

/// Replaces `path` atomically (write a temporary sibling, then rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| io::Error::other("no file name"))?;
    let tmp = path.with_file_name(format!(".{}.tmp-{}", name.to_string_lossy(), std::process::id()));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_data()?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

impl NodeStatus {
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let mut text = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        text.push(b'\n');
        write_atomic(path, &text)
    }

    pub fn read(path: &Path) -> io::Result<NodeStatus> {
        serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)
    }

    /// True when the writer still runs and wrote recently.
    pub fn is_live(&self, now: u64) -> bool {
        self.state == "running"
            && now.saturating_sub(self.updated_at) <= STALE_AFTER_SECS
            && procs::alive(self.pid)
    }

    pub fn last_sync(&self) -> Option<u64> {
        self.peers.iter().map(|p| p.at).max()
    }

    /// One line for the menu: node state, peer count, last reconcile.
    pub fn summary(&self, now: u64) -> String {
        if !self.is_live(now) {
            return "Erlkönig is not running".into();
        }
        if let Some(problem) = &self.problem {
            let mut p: String = problem.chars().take(70).collect();
            if problem.chars().count() > 70 {
                p.push('…');
            }
            return format!("Problem: {p}");
        }
        let peers = match self.configured_peers {
            0 => "no peers".to_string(),
            1 => "1 peer".to_string(),
            n => format!("{n} peers"),
        };
        let sync = if self.sync_collections == 0 {
            "sync off".to_string()
        } else {
            match self.last_sync() {
                Some(at) => format!("last sync {}", words::ago(now.saturating_sub(at))),
                None => "not synced yet".to_string(),
            }
        };
        format!("Running · {peers} · {sync}")
    }
}

/// The menu line when there may be no status file at all.
pub fn summary_at(path: &Path, now: u64) -> String {
    match NodeStatus::read(path) {
        Ok(s) => s.summary(now),
        Err(_) => "Erlkönig is not running".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciled_lines_are_parsed() {
        let p = "6b6e39a755a1533accdd592d6312cf59228041104ee6cf854e3365c437f67cfb";
        assert_eq!(
            parse_reconciled(&format!("reconciled with peer {p}: 3 collections")),
            Some((p.to_string(), 3))
        );
        assert_eq!(
            parse_reconciled(&format!("2026-10-05T12:00:00Z stderr reconciled with peer {p}: 1 collection")),
            Some((p.to_string(), 1))
        );
        assert_eq!(parse_reconciled("node: abc"), None);
        assert_eq!(parse_reconciled("reconciled with peer x: many collections"), None);
    }

    #[test]
    fn write_is_atomic_and_round_trips() {
        let d = std::env::temp_dir().join(format!("erlkonig-status-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let path = d.join("node.status");
        let s = NodeStatus {
            format: FORMAT,
            pid: std::process::id(),
            state: "running".into(),
            updated_at: words::now_unix(),
            node_id: Some("ab".repeat(32)),
            configured_peers: 2,
            sync_collections: 1,
            peers: vec![PeerStatus { peer: "cd".repeat(32), collections: 2, at: words::now_unix() - 180, line: String::new() }],
            ..Default::default()
        };
        s.write(&path).unwrap();
        let back = NodeStatus::read(&path).unwrap();
        assert_eq!(back, s);
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "no temporary file left behind");
        assert_eq!(back.summary(words::now_unix()), "Running · 2 peers · last sync 3 minutes ago");
    }

    #[test]
    fn stale_or_dead_status_reads_as_not_running() {
        let s = NodeStatus { state: "running".into(), pid: std::process::id(), updated_at: 1, ..Default::default() };
        assert_eq!(s.summary(words::now_unix()), "Erlkönig is not running");
        assert_eq!(summary_at(Path::new("/nonexistent/node.status"), 0), "Erlkönig is not running");
    }
}
