//! A machine-local HTTP credential, never included in a plugin or a diagnostic.

use crate::status::write_atomic;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub fn load_or_create(path: &Path) -> Result<String, String> {
    let absent = match fs::symlink_metadata(path) {
        Ok(_) => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.to_string()),
    };
    if absent {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|e| format!("cannot generate MCP credential: {e}"))?;
        write_atomic(path, hex::encode(bytes).as_bytes())
            .map_err(|e| format!("cannot save MCP credential: {e}"))?;
    }
    load(path)
}

pub fn load(path: &Path) -> Result<String, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| format!("cannot open the machine-local MCP credential: {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.permissions().mode() & 0o777 != 0o600 {
        return Err("MCP credential must be a regular private mode-0600 file".into());
    }
    let mut text = String::new();
    file.take(65)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("MCP credential is invalid; restore its private file or explicitly regenerate it and reconnect clients".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_private_credential_and_insecure_file_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let first = load_or_create(&path).unwrap();
        assert_eq!(load_or_create(&path).unwrap(), first);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path).is_err());
        let link = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("missing"), &link).unwrap();
        assert!(load_or_create(&link).is_err());
        assert!(fs::symlink_metadata(link).unwrap().is_symlink());
    }
}
