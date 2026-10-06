//! Native first-start setup. No CLI invocation or ambient pile/key selection.

use crate::config::Resolved;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use triblespace::core::signing_key_file;

pub fn ensure_pile(pile: &Path, create: bool) -> Result<(), String> {
    if create && fs::symlink_metadata(pile).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(
            "the default pile is a symlink; explicitly name another pile in node.toml".into(),
        );
    }
    match fs::metadata(pile) {
        Ok(meta) if meta.is_file() => return Ok(()),
        Ok(_) => return Err(format!("{} is not a file", pile.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound && create => {}
        Err(e) => return Err(format!("cannot open pile {}: {e}", pile.display())),
    }
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(pile)
        .map_err(|e| format!("cannot create pile {}: {e}", pile.display()))?;
    Ok(())
}

pub fn prepare(config: &Resolved) -> Result<ed25519_dalek::SigningKey, String> {
    ensure_pile(&config.pile, !config.pile_named)?;
    if !config.key_named && fs::symlink_metadata(&config.key).is_err() {
        signing_key_file::init(&config.key)
            .map_err(|e| format!("cannot initialize signing identity: {e}"))?;
    }
    signing_key_file::load_existing(&config.key)
        .map_err(|e| format!("cannot load signing identity: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_private_stable_and_named_paths_are_never_created() {
        let d = tempfile::tempdir().unwrap();
        let r = crate::config::NodeConfig::default().resolve(d.path());
        let first = prepare(&r).unwrap();
        assert_eq!(first.to_bytes(), prepare(&r).unwrap().to_bytes());
        let mut named = r.clone();
        named.pile = d.path().join("missing.pile");
        named.pile_named = true;
        assert!(prepare(&named).is_err());
        assert!(!named.pile.exists());
    }
    #[test]
    fn default_pile_never_follows_a_link() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        let path = d.path().join("self.pile");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(ensure_pile(&path, true).is_err());
        assert!(!target.exists());
    }
}
