//! First-start setup: the pile (append-created, never truncated) and the
//! signing key (made by the bundled `trible`, which never replaces one).

use crate::envscrub;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Makes sure `pile` exists. With `create` false the pile must already exist.
/// Returns true when this call created it. A 0-byte file is a valid pile, and
/// opening for append never truncates, so a concurrent creator is harmless.
pub fn ensure_pile(pile: &Path, create: bool) -> Result<bool, String> {
    match fs::metadata(pile) {
        Ok(meta) if meta.is_file() => return Ok(false),
        Ok(_) => return Err(format!("{} exists but is not a file", pile.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect {}: {e}", pile.display())),
    }
    if !create {
        return Err(format!(
            "the pile {} named in node.toml does not exist; Erlkonig creates only its own default pile",
            pile.display()
        ));
    }
    OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(pile)
        .map_err(|e| format!("cannot create {}: {e}", pile.display()))?;
    Ok(true)
}

/// Makes sure the signing key exists, creating it with
/// `trible pile signing-key init <pile> --key <key>` when allowed.
/// Returns true when this call created it.
pub fn ensure_key(trible: &Path, pile: &Path, key: &Path, create: bool) -> Result<bool, String> {
    if fs::symlink_metadata(key).is_ok() {
        return Ok(false);
    }
    if !create {
        return Err(format!(
            "the key {} named in node.toml does not exist; Erlkonig creates only its own default key",
            key.display()
        ));
    }
    let mut cmd = Command::new(trible);
    cmd.args(["pile", "signing-key", "init"])
        .arg(pile)
        .arg("--key")
        .arg(key)
        .stdin(Stdio::null());
    envscrub::apply(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {e}", trible.display()))?;
    // A concurrent first start may have won the race; its key is the key.
    if fs::symlink_metadata(key).is_ok() {
        return Ok(out.status.success());
    }
    Err(format!(
        "trible pile signing-key init failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// The node id: the public key of `key`, as 64 lowercase hex, read from
/// `trible pile net identity --key <key>`.
pub fn node_id(trible: &Path, key: &Path) -> Result<String, String> {
    let mut cmd = Command::new(trible);
    cmd.args(["pile", "net", "identity", "--key"]).arg(key).stdin(Stdio::null());
    envscrub::apply(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {e}", trible.display()))?;
    if !out.status.success() {
        return Err(format!(
            "trible pile net identity failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_identity(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "trible pile net identity printed no `node:` line".to_string())
}

pub fn parse_identity(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let id = line.trim().strip_prefix("node:")?.trim().to_ascii_lowercase();
        crate::config::is_node_id(&id).then_some(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("erlkonig-init-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn pile_is_append_created_and_never_truncated() {
        let d = scratch("pile");
        let p = d.join("self.pile");
        assert!(ensure_pile(&p, true).unwrap());
        assert_eq!(fs::metadata(&p).unwrap().len(), 0);
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"records").unwrap();
        assert!(!ensure_pile(&p, true).unwrap());
        assert_eq!(fs::read(&p).unwrap(), b"records");
    }

    #[test]
    fn a_named_pile_is_never_created() {
        let d = scratch("named");
        let p = d.join("colony.pile");
        assert!(ensure_pile(&p, false).is_err());
        assert!(!p.exists());
    }

    #[test]
    fn a_named_key_is_never_created() {
        let d = scratch("key");
        let k = d.join("k");
        assert!(ensure_key(Path::new("/nonexistent/trible"), &d.join("p"), &k, false).is_err());
        assert!(!k.exists());
    }

    #[test]
    fn identity_line_is_parsed() {
        let out = "node: 6b6e39a755a1533accdd592d6312cf59228041104ee6cf854e3365c437f67cfb\n";
        assert_eq!(
            parse_identity(out).unwrap(),
            "6b6e39a755a1533accdd592d6312cf59228041104ee6cf854e3365c437f67cfb"
        );
        assert!(parse_identity("ticket: abc\n").is_none());
    }
}
