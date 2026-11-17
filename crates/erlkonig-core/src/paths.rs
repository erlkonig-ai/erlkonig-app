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
    node: crate::config::Bootstrap,
    application: bool,
}

impl Layout {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let node = crate::config::Bootstrap::new(&dir);
        Layout {
            dir,
            node,
            application: false,
        }
    }
    /// Conventional fresh-install location; resolution and legacy refusal
    /// happen inside Runtime::start so the native UI can show the error.
    pub fn application(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let home = env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| io::Error::other("HOME is not set"))?;
        let mut layout = Self::new(dir);
        layout.node = crate::config::Bootstrap::new(
            &PathBuf::from(home).join(".local/share/triblespace/custody"),
        );
        layout.application = true;
        Ok(layout)
    }
    /// Bind one explicit store and key. Never consult ambient Faculty variables.
    pub fn with_node(mut self, pile: impl Into<PathBuf>, key: impl Into<PathBuf>) -> Self {
        self.node = crate::config::Bootstrap {
            pile: pile.into(),
            key: key.into(),
        };
        self.application = false;
        self
    }
    pub fn selected_node(&self) -> anyhow::Result<(crate::config::Bootstrap, Option<[u8; 32]>)> {
        if self.application {
            let locator = self.dir.join("node.json");
            match fs::read(&locator) {
                Ok(bytes) => {
                    let locator: crate::config::NodeLocator = serde_json::from_slice(&bytes)?;
                    let binding = locator.binding()?;
                    return Ok((
                        crate::config::Bootstrap {
                            pile: binding.pile,
                            key: binding.key,
                        },
                        Some(binding.signer),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if self.dir.join("self.pile").exists() || self.dir.join("self.key").exists() {
                anyhow::bail!(
                    "legacy application identity is present; explicitly migrate settings and bind-node before starting the single-store node"
                );
            }
        }
        Ok((self.node.clone(), None))
    }
    pub(crate) fn uses_locator(&self) -> bool {
        self.application
    }
    /// Unresolved construction default, for explicit isolated fixtures.
    /// Application callers use selected_node; running callers use Runtime::binding.
    pub fn bootstrap(&self) -> crate::config::Bootstrap {
        self.node.clone()
    }
    pub fn lock(&self) -> PathBuf {
        self.dir.join("node.lock")
    }
    /// Unresolved fixture default, never runtime diagnostics or routing.
    pub fn default_pile(&self) -> PathBuf {
        self.node.pile.clone()
    }
    /// Unresolved fixture default, never runtime diagnostics or routing.
    pub fn default_key(&self) -> PathBuf {
        self.node.key.clone()
    }
}

/// Files whose presence marks a folder as an Erlkonig data or log folder.
const OUR_FILES: &[&str] = &[
    "node.lock",
    "node.json",
    "self.pile",
    "self.key",
    "mcp.token",
];

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

/// Durable program path for generated host commands. AppImage's mounted ELF
/// disappears after exit; the outer image remains the executable entry point.
/// Native and app-owned short-lived child paths keep current_exe semantics.
pub fn launcher_executable() -> io::Result<PathBuf> {
    let executable = env::current_exe()?;
    #[cfg(target_os = "linux")]
    {
        appimage_launcher(executable, env::var_os("APPIMAGE").as_deref(), env::var_os("APPDIR").as_deref())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(executable)
    }
}

#[cfg(any(target_os = "linux", test))]
fn appimage_launcher(executable: PathBuf, appimage: Option<&std::ffi::OsStr>, appdir: Option<&std::ffi::OsStr>) -> io::Result<PathBuf> {
    let Some(appdir) = appdir.map(Path::new).filter(|p| p.is_absolute()) else {
        return Ok(executable);
    };
    // Ignore AppImage variables inherited from an unrelated parent process.
    if executable.strip_prefix(appdir).ok() != Some(Path::new("usr/bin/erlkonig")) {
        return Ok(executable);
    }
    let outer = appimage.map(PathBuf::from).filter(|p| p.is_absolute())
        .ok_or_else(|| io::Error::other("AppImage did not supply its absolute outer executable path"))?;
    let metadata = fs::metadata(&outer)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::other("The outer AppImage is not an executable file"));
    }
    Ok(outer)
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
    fn appimage_host_commands_use_outer_file_with_spaces() {
        let scratch = tempfile::tempdir().unwrap();
        let mount = scratch.path().join("mount with spaces");
        let outer = scratch.path().join("Erlkonig candidate.AppImage");
        fs::write(&outer, b"synthetic executable").unwrap();
        fs::set_permissions(&outer, fs::Permissions::from_mode(0o700)).unwrap();
        let inner = mount.join("usr/bin/erlkonig");
        assert_eq!(appimage_launcher(inner.clone(), Some(outer.as_os_str()), Some(mount.as_os_str())).unwrap(), outer);
        assert!(appimage_launcher(inner.clone(), Some(std::ffi::OsStr::new("relative.AppImage")), Some(mount.as_os_str())).is_err());
        assert!(appimage_launcher(inner.clone(), None, Some(mount.as_os_str())).is_err());
        fs::set_permissions(&outer, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(appimage_launcher(inner, Some(outer.as_os_str()), Some(mount.as_os_str())).is_err());
    }

    #[test]
    fn native_launcher_ignores_unrelated_appimage_environment() {
        let native = PathBuf::from("/usr/bin/erlkonig");
        assert_eq!(appimage_launcher(native.clone(), None, None).unwrap(), native);
        assert_eq!(appimage_launcher(native.clone(), Some(std::ffi::OsStr::new("/other.AppImage")), Some(std::ffi::OsStr::new("/tmp/other-app"))).unwrap(), native);
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
