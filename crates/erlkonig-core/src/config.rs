//! `D/node.toml`: which pile and key the node uses, whom it syncs with, and
//! which collections it syncs and maintains.
//!
//! ```toml
//! # all optional; relative paths are relative to the data directory
//! pile = "self.pile"
//! key = "self.key"
//! peers = ["<64 hex node id>"]
//! sync_collections = ["blake3:<64 hex>"]
//! maintain_targets = ["blake3:<64 hex>"]
//! ```
//!
//! A pile or key that the file names is used as it is and never created: the
//! node only creates the defaults inside the data directory, so pointing it at
//! an existing colony pile cannot make a fresh one at a mistyped path.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub pile: Option<PathBuf>,
    pub key: Option<PathBuf>,
    #[serde(default)]
    pub peers: Vec<String>,
    #[serde(default)]
    pub sync_collections: Vec<String>,
    #[serde(default)]
    pub maintain_targets: Vec<String>,
}

/// The config with paths made absolute and defaults filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub pile: PathBuf,
    pub key: PathBuf,
    /// True when node.toml named the pile, so it must already exist.
    pub pile_named: bool,
    /// True when node.toml named the key, so it must already exist.
    pub key_named: bool,
    pub peers: Vec<String>,
    pub sync_collections: Vec<String>,
    pub maintain_targets: Vec<String>,
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A bare node id: the 64-hex public key of a node's signing key.
pub fn is_node_id(s: &str) -> bool {
    is_hex64(s)
}

/// An exact collection handle, `blake3:<64 hex>` or the bare 64 hex.
pub fn is_handle(s: &str) -> bool {
    is_hex64(s.strip_prefix("blake3:").unwrap_or(s))
}

fn dedup(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for item in list {
        if !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

impl NodeConfig {
    pub fn parse(text: &str) -> Result<Self, String> {
        let cfg: NodeConfig = toml::from_str(text).map_err(|e| format!("node.toml: {e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reads `path`; a missing file is the empty (default) config.
    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(NodeConfig::default()),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        for p in &self.peers {
            if !is_node_id(p) {
                return Err(format!("peer {p:?} is not a node id (64 hex characters)"));
            }
        }
        for (field, list) in [
            ("sync_collections", &self.sync_collections),
            ("maintain_targets", &self.maintain_targets),
        ] {
            for h in list {
                if !is_handle(h) {
                    return Err(format!(
                        "{field}: {h:?} is not an exact collection handle (blake3:<64 hex>); names are not accepted"
                    ));
                }
            }
        }
        for (field, path) in [("pile", &self.pile), ("key", &self.key)] {
            if path.as_ref().is_some_and(|p| p.as_os_str().is_empty()) {
                return Err(format!("{field} is empty"));
            }
        }
        Ok(())
    }

    pub fn resolve(&self, data_dir: &Path) -> Resolved {
        let abs = |p: &PathBuf| if p.is_absolute() { p.clone() } else { data_dir.join(p) };
        Resolved {
            pile: self.pile.as_ref().map(abs).unwrap_or_else(|| data_dir.join("self.pile")),
            key: self.key.as_ref().map(abs).unwrap_or_else(|| data_dir.join("self.key")),
            pile_named: self.pile.is_some(),
            key_named: self.key.is_some(),
            peers: dedup(&self.peers),
            sync_collections: dedup(&self.sync_collections),
            maintain_targets: dedup(&self.maintain_targets),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "blake3:00a12ca4fc0122d7ed58c9e29d874fe0f84eae5adca15fb8073a825903af65e5";
    const P: &str = "6b6e39a755a1533accdd592d6312cf59228041104ee6cf854e3365c437f67cfb";

    #[test]
    fn empty_config_resolves_to_defaults_inside_d() {
        let r = NodeConfig::parse("").unwrap().resolve(Path::new("/d"));
        assert_eq!(r.pile, Path::new("/d/self.pile"));
        assert_eq!(r.key, Path::new("/d/self.key"));
        assert!(!r.pile_named && !r.key_named);
        assert!(r.peers.is_empty() && r.sync_collections.is_empty() && r.maintain_targets.is_empty());
    }

    #[test]
    fn full_config_parses_and_resolves_relative_paths() {
        let text = format!(
            "pile = \"/colony/self.pile\"\nkey = \"keys/k\"\npeers = [\"{P}\", \"{P}\"]\nsync_collections = [\"{H}\"]\nmaintain_targets = [\"{}\"]\n",
            &H[7..]
        );
        let r = NodeConfig::parse(&text).unwrap().resolve(Path::new("/d"));
        assert_eq!(r.pile, Path::new("/colony/self.pile"));
        assert_eq!(r.key, Path::new("/d/keys/k"));
        assert!(r.pile_named && r.key_named);
        assert_eq!(r.peers, vec![P.to_string()]);
        assert_eq!(r.sync_collections, vec![H.to_string()]);
        assert_eq!(r.maintain_targets, vec![H[7..].to_string()]);
    }

    #[test]
    fn names_and_bad_ids_are_refused() {
        assert!(NodeConfig::parse("sync_collections = [\"wiki\"]").is_err());
        assert!(NodeConfig::parse("maintain_targets = [\"blake3:abc\"]").is_err());
        assert!(NodeConfig::parse("peers = [\"not-a-node\"]").is_err());
        assert!(NodeConfig::parse("pile = \"\"").is_err());
    }

    #[test]
    fn unknown_fields_are_refused() {
        let e = NodeConfig::parse("peer = []").unwrap_err();
        assert!(e.contains("unknown field"), "{e}");
    }

    #[test]
    fn missing_file_is_the_default() {
        let c = NodeConfig::load(Path::new("/nonexistent/erlkonig/node.toml")).unwrap();
        assert_eq!(c, NodeConfig::default());
    }
}
