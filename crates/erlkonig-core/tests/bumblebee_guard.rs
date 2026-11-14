//! No Bumblebee: install, quiet refresh and uninstall run through the real
//! installer entry points against fake clients and a scratch HOME full of
//! sentinels. Every sentinel must survive byte for byte, the clients must
//! receive exactly our plugin's argv, and nothing outside the app's data
//! directory may be written. This file holds one test: it sets HOME.

use erlkonig_core::install::{self, Host, PLUGIN_ID};
use erlkonig_core::paths::Layout;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

const URL: &str = "http://127.0.0.1:41773/";

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Dir(u32, SystemTime),
    File(u32, SystemTime, Vec<u8>),
    Link(PathBuf),
}

/// Every path under `root`, never following a symlink.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        let mode = meta.permissions().mode();
        let entry = if meta.file_type().is_symlink() {
            Entry::Link(fs::read_link(&path).unwrap())
        } else if meta.is_dir() {
            for child in fs::read_dir(&path).unwrap() {
                stack.push(child.unwrap().path());
            }
            Entry::Dir(mode, meta.modified().unwrap())
        } else {
            Entry::File(mode, meta.modified().unwrap(), fs::read(&path).unwrap())
        };
        out.insert(path, entry);
    }
    out
}

fn fake_client(dir: &Path, host: Host) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let name = match host {
        Host::OpenAi => "codex",
        Host::ClaudeCode => "claude",
    };
    let program = dir.join(name);
    fs::write(&program, include_str!("fixtures/fake-client.sh")).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    program
}

/// The argv lists a fake client received since the last call.
fn take_argv(program: &Path) -> Vec<Vec<String>> {
    let log = program.with_file_name("argv.log");
    let text = fs::read_to_string(&log).unwrap_or_default();
    fs::write(&log, "").unwrap();
    text.lines()
        .map(|line| line.split_terminator('\u{1f}').map(String::from).collect())
        .collect()
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn install_refresh_and_uninstall_touch_nothing_but_their_own_data() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path().join("root");
    let home = root.join("home");
    let data = home.join(".local/share/Erlkonig App");
    // Sentinels: ordinary, spaced, empty-looking, dash-led, glob-looking and
    // look-alike neighbours of the data directory, plus symlinks to the root.
    for dir in [
        "usr/lib",
        "home/.claude/plugins/cache/other@market",
        "home/.codex/plugins",
        "home/.local/share/Erlkonig App/sessions",
        "home/.local/share/Erlkonig App-other",
        "home/.local/share/Erlkonig App ",
        "home/.local/share/Erlkonig",
        "home/dir with spaces/inner dir",
        "home/ ",
        "home/-rf",
        "home/~",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for (file, bytes) in [
        ("usr/lib/libc.so.6", &b"\x7fELF sentinel"[..]),
        (
            "home/.claude.json",
            br#"{"mcpServers":{"erlkonig":{"command":"/old/erlkonig"}}}"#,
        ),
        ("home/.claude/plugins/cache/other@market/plugin.json", b"{}"),
        (
            "home/.codex/config.toml",
            b"[mcp_servers.erlkonig]\ncommand = \"/old\"\n",
        ),
        (
            "home/.local/share/Erlkonig App/mcp.token",
            b"secret sentinel",
        ),
        ("home/.local/share/Erlkonig App/sessions/keep", b"keep"),
        ("home/.local/share/Erlkonig App-other/keep", b"keep"),
        ("home/.local/share/Erlkonig App /keep", b"keep"),
        ("home/.local/share/Erlkonig/keep", b"keep"),
        ("home/dir with spaces/inner dir/file with spaces", b"spaces"),
        ("home/ / ", b"blank"),
        ("home/''", b"quotes"),
        ("home/\"\"", b"double quotes"),
        ("home/-rf/--no-preserve-root", b"dash"),
        ("home/*", b"star"),
        ("home/~/tilde", b"tilde"),
    ] {
        fs::write(root.join(file), bytes).unwrap();
    }
    symlink(&root, home.join("link to root")).unwrap();
    symlink(&root, data.join("link to root")).unwrap();
    // SAFETY: this test binary holds this one test, so no thread reads the
    // environment concurrently.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude"));
        std::env::set_var("CODEX_HOME", home.join(".codex"));
        std::env::set_var("XDG_DATA_HOME", home.join(".local/share"));
    }
    // Establish shell-runtime state before measuring installer mutations.
    // Rosetta creates HOME/.cache/rosetta even for this neutral command.
    assert!(std::process::Command::new("/bin/sh")
        .args(["-c", ":"]).status().unwrap().success());
    let before = snapshot(&root);

    let layout = Layout::new(data.clone());
    let claude = fake_client(&scratch.path().join("clients/claude"), Host::ClaudeCode);
    let codex = fake_client(&scratch.path().join("clients/codex"), Host::OpenAi);
    let cancel = AtomicBool::new(false);
    let first = Path::new("/Applications/Erlkonig.app/Contents/MacOS/erlkonig");
    let moved = Path::new("/Users/someone/Erlkonig 2.app/Contents/MacOS/erlkonig");
    let market = install::marketplace_root(&layout);
    let market = market.to_str().unwrap();
    let list = |host| match host {
        Host::ClaudeCode => argv(&["plugin", "list", "--json"]),
        Host::OpenAi => argv(&[
            "--disable",
            "remote_plugin",
            "plugin",
            "list",
            "--marketplace",
            "erlkonig-local",
            "--json",
        ]),
    };
    let markets = argv(&["plugin", "marketplace", "list", "--json"]);

    // An empty executable path or a relative data directory is refused before
    // any client mutation and before anything is written.
    for (program, host) in [(&claude, Host::ClaudeCode), (&codex, Host::OpenAi)] {
        assert!(
            install::set_installed_with_client(
                host,
                program,
                true,
                &layout,
                Path::new(""),
                URL,
                &cancel
            )
            .is_err()
        );
        assert_eq!(take_argv(program), [list(host), markets.clone()]);
        assert!(
            install::set_installed_with_client(
                host,
                program,
                true,
                &Layout::new(PathBuf::new()),
                first,
                URL,
                &cancel
            )
            .is_err()
        );
        assert!(
            take_argv(program)
                .iter()
                .all(|a| *a == list(host) || *a == markets)
        );
    }
    assert!(!data.join("plugins").exists());

    // Install.
    for (program, host) in [(&claude, Host::ClaudeCode), (&codex, Host::OpenAi)] {
        let state =
            install::set_installed_with_client(host, program, true, &layout, first, URL, &cancel)
                .unwrap();
        assert!(state.installed);
        let (add, install) = match host {
            Host::ClaudeCode => (
                argv(&["plugin", "marketplace", "add", market]),
                argv(&["plugin", "install", PLUGIN_ID, "--scope", "user"]),
            ),
            Host::OpenAi => (
                argv(&["plugin", "marketplace", "add", market, "--json"]),
                argv(&["plugin", "add", PLUGIN_ID, "--json"]),
            ),
        };
        assert_eq!(
            take_argv(program),
            [
                list(host),
                markets.clone(),
                add,
                markets.clone(),
                install,
                list(host)
            ]
        );
    }
    for host in [Host::ClaudeCode, Host::OpenAi] {
        assert!(install::installed_is_current(&layout, host, first, URL));
        assert!(!install::installed_is_current(&layout, host, moved, URL));
        let port = "http://127.0.0.1:1/";
        assert!(!install::installed_is_current(&layout, host, first, port));
    }

    // A current package is left alone: no client is even asked.
    let installed = [
        (Host::ClaudeCode, claude.clone()),
        (Host::OpenAi, codex.clone()),
    ];
    assert!(install::refresh_if_stale(&installed, &layout, first, URL, &cancel).is_empty());
    assert!(take_argv(&claude).is_empty() && take_argv(&codex).is_empty());

    // The app moved: both installed plugins are reinstalled for the new path.
    // Codex refuses the first time; only Claude Code is then current.
    fs::write(codex.with_file_name("fail"), "1\n").unwrap();
    let refreshed = install::refresh_if_stale(&installed, &layout, moved, URL, &cancel);
    assert_eq!(refreshed.len(), 2);
    assert!(refreshed[0].1.as_ref().unwrap().installed);
    assert!(refreshed[1].1.is_err());
    assert!(install::installed_is_current(
        &layout,
        Host::ClaudeCode,
        moved,
        URL
    ));
    assert!(!install::installed_is_current(
        &layout,
        Host::OpenAi,
        moved,
        URL
    ));
    let codex_reinstall = [
        list(Host::OpenAi),
        markets.clone(),
        argv(&["plugin", "remove", PLUGIN_ID, "--json"]),
        argv(&["plugin", "add", PLUGIN_ID, "--json"]),
        list(Host::OpenAi),
    ];
    assert_eq!(
        take_argv(&claude),
        [
            list(Host::ClaudeCode),
            markets.clone(),
            argv(&[
                "plugin",
                "uninstall",
                PLUGIN_ID,
                "--scope",
                "user",
                "--keep-data"
            ]),
            argv(&["plugin", "install", PLUGIN_ID, "--scope", "user"]),
            list(Host::ClaudeCode),
        ]
    );
    assert_eq!(take_argv(&codex), codex_reinstall[..3]);
    // The next refresh retries Codex alone, although the package is staged.
    fs::write(codex.with_file_name("fail"), "0\n").unwrap();
    let refreshed = install::refresh_if_stale(&installed, &layout, moved, URL, &cancel);
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].0, Host::OpenAi);
    assert!(refreshed[0].1.as_ref().unwrap().installed);
    assert!(take_argv(&claude).is_empty());
    assert_eq!(take_argv(&codex), codex_reinstall);
    assert!(install::refresh_if_stale(&installed, &layout, moved, URL, &cancel).is_empty());
    assert!(take_argv(&claude).is_empty() && take_argv(&codex).is_empty());
    let mcp: serde_json::Value = serde_json::from_slice(
        &fs::read(data.join("plugins/erlkonig-local/claude/.mcp.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        mcp["mcpServers"]["faculties"]["command"],
        moved.to_str().unwrap()
    );

    // Uninstall.
    for (program, host) in [(&claude, Host::ClaudeCode), (&codex, Host::OpenAi)] {
        let state =
            install::set_installed_with_client(host, program, false, &layout, moved, URL, &cancel)
                .unwrap();
        assert!(!state.installed);
        let remove = match host {
            Host::ClaudeCode => argv(&["plugin", "uninstall", PLUGIN_ID, "--scope", "user"]),
            Host::OpenAi => argv(&["plugin", "remove", PLUGIN_ID, "--json"]),
        };
        assert_eq!(
            take_argv(program),
            [list(host), markets.clone(), remove, list(host)]
        );
    }

    // A stale package never installs a plugin that is not installed, even
    // when a caller names it: the client is asked, and that is all.
    assert!(!install::installed_is_current(
        &layout,
        Host::ClaudeCode,
        first,
        URL
    ));
    let refreshed = install::refresh_if_stale(&installed, &layout, first, URL, &cancel);
    assert!(
        refreshed
            .iter()
            .all(|(_, r)| !r.as_ref().unwrap().installed)
    );
    assert_eq!(take_argv(&claude), [list(Host::ClaudeCode)]);
    assert_eq!(take_argv(&codex), [list(Host::OpenAi)]);

    // Every sentinel survived byte for byte, and every change is inside the
    // app's own data directory (its plugin staging, nothing else).
    let after = snapshot(&root);
    // Diagnostic only: retain every delta before the first guard assertion.
    let describe = |entry: Option<&Entry>| match entry {
        None => "missing".to_owned(),
        Some(Entry::Dir(mode, time)) => format!("directory mode={mode:o} mtime={time:?}"),
        Some(Entry::File(mode, time, bytes)) =>
            format!("file mode={mode:o} mtime={time:?} length={}", bytes.len()),
        Some(Entry::Link(target)) => format!("symlink target={target:?}"),
    };
    for path in before.keys().chain(after.keys()).collect::<std::collections::BTreeSet<_>>() {
        let (old, new) = (before.get(path), after.get(path));
        if old != new {
            let bytes_equal = match (old, new) {
                (Some(Entry::File(_, _, a)), Some(Entry::File(_, _, b))) => Some(a == b),
                _ => None,
            };
            eprintln!("guard_delta path={path:?} before={} after={} file_bytes_equal={bytes_equal:?}",
                describe(old), describe(new));
        }
    }
    let staging = data.join("plugins");
    for (path, entry) in &before {
        if path.starts_with(&staging) || *path == data {
            continue;
        }
        assert_eq!(after.get(path), Some(entry), "{} changed", path.display());
    }
    for path in after.keys() {
        assert!(
            before.contains_key(path) || path.starts_with(&staging),
            "{} was written outside the plugin staging directory",
            path.display()
        );
    }
}
