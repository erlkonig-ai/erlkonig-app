//! Private application data paths. Runtime status is an in-process value.

use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The per-OS default data directory below `home`.
pub fn default_data_dir_for(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Erlkonig")
    } else {
        match env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
            Some(x) => PathBuf::from(x).join("erlkonig"),
            None => home.join(".local/share/erlkonig"),
        }
    }
}

/// The per-OS default below `$HOME`; test callers pass a Layout explicitly.
pub fn default_data_dir() -> io::Result<PathBuf> {
    let home = env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| io::Error::other("HOME is not set, so there is no data directory"))?;
    Ok(default_data_dir_for(Path::new(&home)))
}

/// The files inside one data directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub dir: PathBuf,
}

impl Layout {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Layout { dir: dir.into() }
    }
    pub fn lock(&self) -> PathBuf {
        self.dir.join("node.lock")
    }
    pub fn default_pile(&self) -> PathBuf {
        self.dir.join("self.pile")
    }
    pub fn default_key(&self) -> PathBuf {
        self.dir.join("self.key")
    }
}

/// Files whose presence marks a folder as an Erlkonig data or log folder.
const OUR_FILES: &[&str] = &["node.lock", "self.pile", "self.key", "mcp.token"];

/// True when `dir` is empty or holds one of Erlkonig's own files.
fn looks_like_ours(dir: &Path) -> io::Result<bool> {
    let mut empty = true;
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if OUR_FILES.iter().any(|f| name == *f) {
            return Ok(true);
        }
        empty = false;
    }
    Ok(empty)
}

/// Creates `dir` (and missing parents, all 0700) and makes `dir` itself mode
/// 0700. An existing folder that others can read is tightened only when it
/// is empty or already holds Erlkonig's files; any other folder (a mistyped
/// `--data-dir ~`, say) is refused rather than having its permissions changed.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    let existed = fs::symlink_metadata(dir).is_ok();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = fs::metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    if meta.permissions().mode() & 0o777 == 0o700 {
        return Ok(());
    }
    if existed && !looks_like_ours(dir)? {
        return Err(io::Error::other(format!(
            "{} already exists, is not an Erlkonig folder and is not private (mode {:o}); \
Erlkonig leaves its permissions alone. Use a new or empty folder.",
            dir.display(),
            meta.permissions().mode() & 0o777
        )));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

/// Gatekeeper runs a quarantined app from a random read-only mount
/// (`/private/var/folders/.../AppTranslocation/...`) until the user moves it.
pub fn is_translocated(path: &Path) -> bool {
    path.to_string_lossy().contains("/AppTranslocation/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = env::temp_dir().join(format!("erlkonig-paths-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn private_dir_is_0700_even_if_it_existed_wider() {
        let root = scratch("private");
        let d = root.join("a/b");
        ensure_private_dir(&d).unwrap();
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&d).unwrap();
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn a_foreign_folder_is_refused_not_chmodded() {
        let root = scratch("foreign");
        let d = root.join("shared");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("notes.txt"), "x").unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        let e = ensure_private_dir(&d).unwrap_err();
        assert!(
            e.to_string().contains("leaves its permissions alone"),
            "{e}"
        );
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // The same folder once it holds a node pile is ours, and is tightened.
        fs::write(d.join("self.pile"), "").unwrap();
        ensure_private_dir(&d).unwrap();
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn translocation_is_detected() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/xy/T/AppTranslocation/1234/d/Erlkonig.app/Contents/MacOS/erlkonig"
        )));
        assert!(!is_translocated(Path::new(
            "/Applications/Erlkonig.app/Contents/MacOS/erlkonig"
        )));
    }

    #[test]
    fn layout_names() {
        let l = Layout::new("/d");
        assert_eq!(l.lock(), Path::new("/d/node.lock"));
        assert_eq!(l.default_pile(), Path::new("/d/self.pile"));
        assert_eq!(l.default_key(), Path::new("/d/self.key"));
    }
}
