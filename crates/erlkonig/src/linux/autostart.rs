//! "Open at login" as an XDG autostart entry: no service manager, no restart
//! policy; the session starts the same executable once at login.

use std::io;
use std::path::{Path, PathBuf};

pub(crate) const FILE_NAME: &str = "ai.erlkonig.app.desktop";

/// `$XDG_CONFIG_HOME/autostart`, else `~/.config/autostart`.
pub(crate) fn dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(config.join("autostart"))
}

/// One `Exec=` argument, quoted as the Desktop Entry Specification requires.
pub(crate) fn exec_arg(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+,@".contains(c))
    {
        return text.into_owned();
    }
    // Inside quotes `"`, `` ` ``, `$` and `\` take a backslash; the string
    // value is unescaped once more before that, so each backslash doubles.
    let mut quoted = String::from("\"");
    for c in text.chars() {
        match c {
            '"' | '`' | '$' => quoted.push_str("\\\\"),
            '\\' => quoted.push_str("\\\\\\"),
            '%' => quoted.push('%'),
            _ => {}
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

pub(crate) fn entry(executable: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Erlkonig\nComment=Local Erlkonig node and tools\nExec={}\nIcon=ai.erlkonig.app\nTerminal=false\nStartupNotify=false\nX-GNOME-Autostart-enabled=true\n",
        exec_arg(executable)
    )
}

/// Enabled when the entry exists and does not say `Hidden=true`.
pub(crate) fn enabled(dir: &Path) -> bool {
    match std::fs::read_to_string(dir.join(FILE_NAME)) {
        Ok(text) => !text
            .lines()
            .any(|line| line.trim().replace(' ', "") == "Hidden=true"),
        Err(_) => false,
    }
}

pub(crate) fn set(dir: &Path, on: bool, executable: &Path) -> io::Result<()> {
    let file = dir.join(FILE_NAME);
    if on {
        std::fs::create_dir_all(dir)?;
        let partial = dir.join(format!(".{FILE_NAME}.partial"));
        std::fs::write(&partial, entry(executable))?;
        std::fs::rename(&partial, &file)
    } else {
        match std::fs::remove_file(&file) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("erlkonig-autostart-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn enable_disable_round_trip_in_an_injected_config_dir() {
        let dir = scratch("round-trip").join("autostart");
        let exe = Path::new("/home/someone/.local/bin/erlkonig");
        assert!(!enabled(&dir));
        set(&dir, true, exe).unwrap();
        assert!(enabled(&dir));
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("\nExec=/home/someone/.local/bin/erlkonig\n"));
        assert!(text.contains("\nType=Application\n"));
        set(&dir, true, exe).unwrap(); // Idempotent.
        assert!(enabled(&dir));
        set(&dir, false, exe).unwrap();
        assert!(!enabled(&dir));
        assert!(!dir.join(FILE_NAME).exists());
        set(&dir, false, exe).unwrap(); // Disabling twice is fine.
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "no partial file left"
        );
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_hidden_entry_counts_as_disabled() {
        let dir = scratch("hidden");
        set(&dir, true, Path::new("/usr/bin/erlkonig")).unwrap();
        let file = dir.join(FILE_NAME);
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{text}Hidden = true\n")).unwrap();
        assert!(!enabled(&dir));
        std::fs::write(&file, format!("{text}Hidden=false\n")).unwrap();
        assert!(enabled(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exec_quotes_paths_with_reserved_characters() {
        assert_eq!(
            exec_arg(Path::new("/opt/erlkonig/bin/erlkonig")),
            "/opt/erlkonig/bin/erlkonig"
        );
        assert_eq!(
            exec_arg(Path::new("/home/a b/erlkonig")),
            "\"/home/a b/erlkonig\""
        );
        assert_eq!(exec_arg(Path::new("/x/$y")), "\"/x/\\\\$y\"");
        assert_eq!(exec_arg(Path::new("/x/a\"b")), "\"/x/a\\\\\"b\"");
        assert_eq!(exec_arg(Path::new("/x/a\\b")), "\"/x/a\\\\\\\\b\"");
        assert_eq!(exec_arg(Path::new("/x/100%")), "\"/x/100%%\"");
    }
}
