//! Immutable process bootstrap. Workspaces never select a store or identity.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use triblespace::core::collection::CollectionHandle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bootstrap {
    pub pile: PathBuf,
    pub key: PathBuf,
}

/// One immutable node binding captured when its owner starts. Public identity
/// is pinned independently of the key path, including on every faculty call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeBinding {
    pub pile: PathBuf,
    pub key: PathBuf,
    pub signer: [u8; 32],
}

impl NodeBinding {
    /// The active storage folder, not the app's configuration or key directory.
    /// The current single-Pile store lives in its containing directory.
    pub fn storage_folder(&self) -> Option<&Path> {
        self.pile.parent()
    }
}

/// The only external node document: bootstrap location and expected public
/// identity. Application settings and workspace facts remain in the pile.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeLocator {
    pub pile: PathBuf,
    pub key: PathBuf,
    pub expected_signer: String,
}

impl NodeLocator {
    pub fn binding(&self) -> Result<NodeBinding> {
        if !self.pile.is_absolute() || !self.key.is_absolute() {
            bail!("node locator requires explicit absolute pile and key paths");
        }
        if !is_node_id(&self.expected_signer) {
            bail!("node locator expected_signer must be a full public key");
        }
        let signer: [u8; 32] = hex::decode(&self.expected_signer)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid public key"))?;
        ed25519_dalek::VerifyingKey::from_bytes(&signer)?;
        Ok(NodeBinding {
            pile: self.pile.clone(),
            key: self.key.clone(),
            signer,
        })
    }
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
                // Only macOS gates these folders behind a per-app permission.
                let rule = if cfg!(target_os = "macos") {
                    "; macOS may require permission (again after an ad-hoc rebuild)."
                } else {
                    "."
                };
                warnings.push(format!(
                    "The {what} {} is inside {folder}{rule}",
                    path.display()
                ));
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

    #[test]
    fn protected_folder_warnings_state_the_platform_rule() {
        let home = Path::new("/home/someone");
        let warnings = protected_folder_warnings(
            &home.join("Documents/node/self.pile"),
            &home.join(".local/share/erlkonig/self.key"),
            home,
        );
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0]
                .starts_with("The pile /home/someone/Documents/node/self.pile is inside Documents")
        );
        if cfg!(target_os = "macos") {
            assert!(warnings[0].contains("macOS may require permission"));
        } else {
            // No permission prompt guards these folders elsewhere.
            assert!(!warnings[0].contains("macOS"));
            assert!(!warnings[0].contains("permission"));
        }
        assert!(
            protected_folder_warnings(Path::new("/srv/self.pile"), Path::new("/srv/k"), home)
                .is_empty()
        );
    }

    #[test]
    fn storage_folder_follows_the_bound_pile_not_the_key_or_default_location() {
        let binding = NodeBinding {
            pile: PathBuf::from("/Volumes/Alternative storage/node/archive.pile"),
            key: PathBuf::from("/separate/identity/node.key"),
            signer: [0; 32],
        };
        assert_eq!(
            binding.storage_folder(),
            Some(Path::new("/Volumes/Alternative storage/node"))
        );
        assert_eq!(
            binding.pile,
            Path::new("/Volumes/Alternative storage/node/archive.pile")
        );
        assert_eq!(binding.key, Path::new("/separate/identity/node.key"));
    }
}
