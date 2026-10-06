//! Fixed bootstrap only. App settings and managed-pile intent live in self.pile.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use triblespace::core::collection::CollectionHandle;

#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub pile: PathBuf,
    pub key: PathBuf,
}

impl Bootstrap {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            pile: data_dir.join("self.pile"),
            key: data_dir.join("self.key"),
        }
    }
}

pub fn is_node_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn is_handle(s: &str) -> bool {
    is_node_id(s.strip_prefix("blake3:").unwrap_or(s))
}

pub fn parse_handle(s: &str) -> Result<CollectionHandle> {
    if !is_handle(s) {
        bail!("expected an exact collection handle, blake3:<64 hex>");
    }
    let raw: [u8; 32] = hex::decode(s.strip_prefix("blake3:").unwrap_or(s))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("collection handle must contain 32 bytes"))?;
    Ok(CollectionHandle::new(raw))
}

pub fn protected_folder_warnings(pile: &Path, key: &Path, home: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    for (what, path) in [("pile", pile), ("key", key)] {
        for folder in ["Desktop", "Documents", "Downloads"] {
            if path.starts_with(home.join(folder)) {
                warnings.push(format!("The {what} {} is inside {folder}; macOS may require permission (again after an ad-hoc rebuild).", path.display()));
            }
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_has_no_external_configuration() {
        let b = Bootstrap::new(Path::new("/d"));
        assert_eq!(b.pile, Path::new("/d/self.pile"));
        assert_eq!(b.key, Path::new("/d/self.key"));
        assert!(parse_handle("wiki").is_err());
    }
}
