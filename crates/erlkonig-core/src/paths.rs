//! Where things live: the data directory `D`, the files inside it, and the
//! bundled tools next to the running executable.

use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Overrides the data directory (tests, or a second node on one account).
pub const DATA_DIR_ENV: &str = "ERLKONIG_DATA_DIR";
/// Overrides the directory holding `trible` and `faculties` (tests).
pub const BIN_DIR_ENV: &str = "ERLKONIG_BIN_DIR";

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

/// `ERLKONIG_DATA_DIR` if set, else the per-OS default below `$HOME`.
pub fn data_dir() -> io::Result<PathBuf> {
    if let Some(dir) = env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
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
    pub fn status(&self) -> PathBuf {
        self.dir.join("node.status")
    }
    pub fn config(&self) -> PathBuf {
        self.dir.join("node.toml")
    }
    pub fn logs(&self) -> PathBuf {
        self.dir.join("logs")
    }
    pub fn bin_link(&self) -> PathBuf {
        self.dir.join("bin")
    }
    pub fn plugins(&self) -> PathBuf {
        self.dir.join("plugins")
    }
    pub fn claude_zip(&self) -> PathBuf {
        self.dir.join("erlkonig-claude-plugin.zip")
    }
    pub fn default_pile(&self) -> PathBuf {
        self.dir.join("self.pile")
    }
    pub fn default_key(&self) -> PathBuf {
        self.dir.join("self.key")
    }
}

/// Creates `dir` (and missing parents) and makes `dir` itself mode 0700.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let meta = fs::metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(format!("{} is not a directory", dir.display())));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The directory holding the bundled `trible` and `faculties`: the running
/// executable's own directory, or `ERLKONIG_BIN_DIR`.
pub fn tool_dir() -> io::Result<PathBuf> {
    if let Some(dir) = env::var_os(BIN_DIR_ENV).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let exe = env::current_exe()?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("the running executable has no parent directory"))
}

pub fn tool(name: &str) -> io::Result<PathBuf> {
    Ok(tool_dir()?.join(name))
}

/// The plugin template inside the bundle: `Contents/MacOS/../Resources/plugin`.
pub fn bundled_plugin_dir() -> io::Result<PathBuf> {
    let exe = env::current_exe()?;
    let macos = exe
        .parent()
        .ok_or_else(|| io::Error::other("the running executable has no parent directory"))?;
    Ok(macos.join("../Resources/plugin"))
}

/// Gatekeeper runs a quarantined app from a random read-only mount
/// (`/private/var/folders/.../AppTranslocation/...`) until the user moves it.
pub fn is_translocated(path: &Path) -> bool {
    path.to_string_lossy().contains("/AppTranslocation/")
}

#[derive(Debug, PartialEq, Eq)]
pub enum LinkOutcome {
    Created,
    Replaced,
    Unchanged,
}

/// Points the symlink `link` at `target`, replacing an older symlink
/// atomically. Refuses to touch anything at `link` that is not a symlink.
pub fn ensure_symlink(link: &Path, target: &Path) -> io::Result<LinkOutcome> {
    let outcome = match fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if fs::read_link(link)? == target {
                return Ok(LinkOutcome::Unchanged);
            }
            LinkOutcome::Replaced
        }
        Ok(_) => {
            return Err(io::Error::other(format!(
                "{} exists and is not a symlink; leaving it alone",
                link.display()
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => LinkOutcome::Created,
        Err(e) => return Err(e),
    };
    let name = link
        .file_name()
        .ok_or_else(|| io::Error::other("symlink path has no file name"))?
        .to_string_lossy()
        .into_owned();
    let tmp = link.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    if let Err(e) = fs::rename(&tmp, link) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(outcome)
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
        assert_eq!(fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&d).unwrap();
        assert_eq!(fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn translocation_is_detected() {
        assert!(is_translocated(Path::new(
            "/private/var/folders/xy/T/AppTranslocation/1234/d/Erlkonig.app/Contents/MacOS/erlkonig"
        )));
        assert!(!is_translocated(Path::new("/Applications/Erlkonig.app/Contents/MacOS/erlkonig")));
    }

    #[test]
    fn symlink_is_created_replaced_and_left_alone() {
        let root = scratch("link");
        let a = root.join("a");
        let b = root.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let link = root.join("bin");
        assert_eq!(ensure_symlink(&link, &a).unwrap(), LinkOutcome::Created);
        assert_eq!(ensure_symlink(&link, &a).unwrap(), LinkOutcome::Unchanged);
        assert_eq!(ensure_symlink(&link, &b).unwrap(), LinkOutcome::Replaced);
        assert_eq!(fs::read_link(&link).unwrap(), b);
        let real = root.join("real");
        fs::create_dir_all(&real).unwrap();
        assert!(ensure_symlink(&real, &a).is_err());
        assert!(fs::symlink_metadata(&real).unwrap().is_dir());
    }

    #[test]
    fn layout_names() {
        let l = Layout::new("/d");
        assert_eq!(l.lock(), Path::new("/d/node.lock"));
        assert_eq!(l.status(), Path::new("/d/node.status"));
        assert_eq!(l.config(), Path::new("/d/node.toml"));
        assert_eq!(l.default_pile(), Path::new("/d/self.pile"));
        assert_eq!(l.default_key(), Path::new("/d/self.key"));
    }
}
