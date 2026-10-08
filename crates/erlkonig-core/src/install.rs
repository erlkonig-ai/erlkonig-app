//! Host-owned plugin lifecycle. Never edit client settings or package secrets.
//! Commands are direct, cancellable children; inventory confirms every change.

use crate::{paths::Layout, status::write_atomic};
use serde_json::{Value, json};
use std::fs;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const MARKETPLACE: &str = "erlkonig-local";
pub const PLUGIN_ID: &str = "erlkonig@erlkonig-local";
pub const COWORK_URL: &str = "https://claude.ai/customize/plugins";
pub const COWORK_HELP: &str = "Cowork plugins are managed on your Claude account, separately from local Claude Code installs. In Customize → Plugins, use Add → Upload plugin and choose the prepared ZIP. Remove it there too. Erlkönig cannot observe or check your account installation. The package contains no secret and works on this Mac only while Erlkönig is open. This is a Cowork/Code plugin, not a standalone chat extension.";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    OpenAi,
    ClaudeCode,
}

impl Host {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI local plugin",
            Self::ClaudeCode => "Claude Code plugin",
        }
    }
    fn program(self) -> &'static str {
        match self {
            Self::OpenAi => "codex",
            Self::ClaudeCode => "claude",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    /// Local user-scope installation only; never an account-install claim.
    pub installed: bool,
    pub enabled: bool,
    pub account_managed: bool,
    pub load_errors: bool,
}

/// Unknown/malformed inventory is an error, not an unchecked installation.
pub fn parse_inventory(host: Host, bytes: &[u8]) -> Result<Inventory, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| "Client returned unreadable plugin inventory".to_owned())?;
    let rows = match host {
        Host::OpenAi => value.get("installed").and_then(Value::as_array),
        Host::ClaudeCode => value.as_array(),
    }
    .ok_or("Client returned an unsupported plugin inventory format")?;
    let mut inventory = Inventory {
        installed: false,
        enabled: false,
        account_managed: false,
        load_errors: false,
    };
    for row in rows {
        let key = match host {
            Host::OpenAi => "pluginId",
            Host::ClaudeCode => "id",
        };
        let id = row
            .get(key)
            .and_then(Value::as_str)
            .ok_or("Client inventory contains a plugin without an ID")?;
        if host == Host::ClaudeCode && id == "erlkonig@synced" {
            inventory.account_managed = true;
        }
        if id != PLUGIN_ID {
            continue;
        }
        if host == Host::ClaudeCode && row.get("scope").and_then(Value::as_str) != Some("user") {
            return Err("Erlkönig is installed at another Claude Code scope; manage that installation in Claude Code".into());
        }
        if inventory.installed {
            return Err("Client reported multiple local Erlkönig installations".into());
        }
        let installed = match host {
            Host::OpenAi => row
                .get("installed")
                .and_then(Value::as_bool)
                .ok_or("Client omitted installation state")?,
            Host::ClaudeCode => true,
        };
        inventory.installed = installed;
        inventory.enabled = row
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or("Client omitted enablement state")?
            && installed;
        inventory.load_errors = row
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty());
    }
    Ok(inventory)
}

pub fn find_client(host: Host) -> Option<PathBuf> {
    let mut candidates = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(host.program()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin").join(host.program()));
    }
    for prefix in ["/opt/homebrew/bin", "/usr/local/bin"] {
        candidates.push(Path::new(prefix).join(host.program()));
    }
    if host == Host::OpenAi {
        candidates.push("/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex".into());
    }
    candidates.into_iter().find(|path| {
        path.is_absolute()
            && fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// Kill/reap only our own child group on timeout, Quit, overflow or early return.
struct OwnedChild {
    child: Child,
    reaped: bool,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            // The unreaped leader pins this PID/PGID. Never signal the numeric
            // identity again after try_wait reaped it; it could be reused.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn nonblocking(fd: std::os::fd::RawFd) -> Result<(), String> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(format!(
                "Cannot capture client output: {}",
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn drain(reader: &mut impl Read, output: &mut Vec<u8>) -> Result<(), String> {
    let mut bytes = [0; 8192];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                if output.len() + count > OUTPUT_LIMIT {
                    return Err("Client output exceeded the safety limit".into());
                }
                output.extend_from_slice(&bytes[..count]);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("Cannot read client output".into()),
        }
    }
}

fn run(program: &Path, args: &[&str], cwd: &Path, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Plugin operation cancelled".into());
    }
    let child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("DISABLE_TELEMETRY", "1")
        .process_group(0)
        .spawn()
        .map_err(|e| format!("Cannot start {}: {e}", program.display()))?;
    let mut child = OwnedChild {
        child,
        reaped: false,
    };
    let mut stdout = child.child.stdout.take().ok_or("Missing client stdout")?;
    let mut stderr = child.child.stderr.take().ok_or("Missing client stderr")?;
    nonblocking(stdout.as_raw_fd())?;
    nonblocking(stderr.as_raw_fd())?;
    let (mut output, mut diagnostic) = (Vec::new(), Vec::new());
    let began = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Plugin operation cancelled; refresh status before trying again".into());
        }
        if began.elapsed() > COMMAND_TIMEOUT {
            return Err("Client command timed out; refresh status before trying again".into());
        }
        drain(&mut stdout, &mut output)?;
        drain(&mut stderr, &mut diagnostic)?;
        if let Some(status) = child.child.try_wait().map_err(|e| e.to_string())? {
            child.reaped = true;
            // Successful lifecycle commands must not daemonize. The updater
            // is disabled above; do not discover/kill unrelated descendants.
            drain(&mut stdout, &mut output)?;
            // Other plugins may include private values in diagnostics. Do not
            // surface arbitrary stdout/stderr; use exit status and readback.
            return if status.success() {
                Ok(output)
            } else {
                Err(format!(
                    "{} {} failed ({status}); inspect this client for details",
                    program.display(),
                    args.join(" ")
                ))
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn inventory(host: Host, layout: &Layout, cancel: &AtomicBool) -> Result<Inventory, String> {
    let program =
        find_client(host).ok_or_else(|| format!("{} CLI was not found", host.program()))?;
    read_inventory(host, &program, &layout.dir, cancel)
}

/// Diagnostic only: a legacy server may be user-authored. Never remove or
/// rewrite it, even when its name or URL looks like an old Erlkönig install.
pub fn legacy_config_notice(host: Host) -> Option<&'static str> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let present = match host {
        Host::OpenAi => {
            let dir = std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"));
            fs::read_to_string(dir.join("config.toml"))
                .ok()
                .and_then(|text| text.parse::<toml::Value>().ok())
                .is_some_and(|doc| {
                    doc.get("mcp_servers")
                        .and_then(|servers| servers.get("erlkonig"))
                        .is_some()
                })
        }
        Host::ClaudeCode => {
            let mut files = vec![home.join(".claude.json")];
            if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
                files.push(PathBuf::from(dir).join(".claude.json"));
            }
            files.iter().any(|path| {
                fs::read(path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|doc| {
                        doc.get("mcpServers")
                            .and_then(|servers| servers.get("erlkonig"))
                            .is_some()
                    })
            })
        }
    };
    present.then_some("A separate MCP configuration named erlkonig also exists. It may expose duplicate tools. It is not owned by this plugin installer and has been left untouched; review it in the client before using the plugin.")
}

fn read_inventory(
    host: Host,
    program: &Path,
    cwd: &Path,
    cancel: &AtomicBool,
) -> Result<Inventory, String> {
    parse_inventory(
        host,
        &run(program, &["plugin", "list", "--json"], cwd, cancel)?,
    )
}

pub fn marketplace_root(layout: &Layout) -> PathBuf {
    layout.dir.join("plugins/erlkonig-local")
}

fn directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_dir() => {
            Err(format!("Refusing non-directory {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn json_file(path: &Path, value: &Value) -> Result<(), String> {
    write_atomic(
        path,
        &serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

/// Reused staging directories may not smuggle an old hook, executable, secret,
/// or symlink into what the host installs. Unknown contents are left untouched.
fn validate_package_tree(root: &Path, directory: &Path, manifest: &str) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        let expected_directory = [
            Path::new(manifest),
            Path::new("skills"),
            Path::new("skills/faculties"),
        ]
        .contains(&relative);
        let expected_file = [
            PathBuf::from(manifest).join("plugin.json"),
            PathBuf::from(".mcp.json"),
            PathBuf::from("skills/faculties/SKILL.md"),
        ]
        .iter()
        .any(|p| p == relative);
        if kind.is_dir() && expected_directory {
            validate_package_tree(root, &path, manifest)?;
        } else if !kind.is_file() || !expected_file {
            return Err(format!(
                "Unexpected package content {}; leaving it untouched",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Two provider packages, one skill. Code and the account ZIP use exactly the
/// same Claude directory. No credential is read by package/lifecycle code.
pub fn prepare(layout: &Layout, executable: &Path, url: &str) -> Result<PathBuf, String> {
    if !executable.is_absolute() || !layout.dir.is_absolute() {
        return Err("Plugin executable and app data paths must be absolute".into());
    }
    let executable = executable.to_str().ok_or("Executable path is not UTF-8")?;
    let data = layout.dir.to_str().ok_or("Data path is not UTF-8")?;
    directory(&layout.dir.join("plugins"))?;
    let root = marketplace_root(layout);
    directory(&root)?;
    let mcp = json!({"mcpServers":{"faculties":{"command":executable,
        "args":["mcp-stdio", "--data-dir", data, "--url", url]}}});
    for (provider, manifest_dir, manifest) in [
        (
            "openai",
            ".codex-plugin",
            include_str!("../../../plugin/.codex-plugin/plugin.json"),
        ),
        (
            "claude",
            ".claude-plugin",
            include_str!("../../../plugin/.claude-plugin/plugin.json"),
        ),
    ] {
        let plugin = root.join(provider);
        directory(&plugin)?;
        validate_package_tree(&plugin, &plugin, manifest_dir)?;
        directory(&plugin.join(manifest_dir))?;
        directory(&plugin.join("skills"))?;
        directory(&plugin.join("skills/faculties"))?;
        write_atomic(
            &plugin.join(manifest_dir).join("plugin.json"),
            manifest.as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        write_atomic(
            &plugin.join("skills/faculties/SKILL.md"),
            include_bytes!("../../../plugin/skills/faculties/SKILL.md"),
        )
        .map_err(|e| e.to_string())?;
        json_file(&plugin.join(".mcp.json"), &mcp)?;
    }
    directory(&root.join(".agents"))?;
    directory(&root.join(".agents/plugins"))?;
    json_file(
        &root.join(".agents/plugins/marketplace.json"),
        &json!({
            "name":MARKETPLACE, "interface":{"displayName":"Erlkönig on this Mac"},
            "plugins":[{"name":"erlkonig", "source":{"source":"local", "path":"./openai"},
                "policy":{"installation":"AVAILABLE", "authentication":"ON_USE"}, "category":"Productivity"}]
        }),
    )?;
    directory(&root.join(".claude-plugin"))?;
    json_file(
        &root.join(".claude-plugin/marketplace.json"),
        &json!({
            "name":MARKETPLACE, "owner":{"name":"Erlkönig"},
            "plugins":[{"name":"erlkonig", "source":"./claude"}]
        }),
    )?;
    Ok(root)
}

/// A marketplace name alone is not ownership evidence. Verify the source path.
fn owns_marketplace(
    host: Host,
    program: &Path,
    root: &Path,
    cwd: &Path,
    cancel: &AtomicBool,
) -> Result<bool, String> {
    let output = run(
        program,
        &["plugin", "marketplace", "list", "--json"],
        cwd,
        cancel,
    )?;
    let value: Value =
        serde_json::from_slice(&output).map_err(|_| "Cannot verify marketplace ownership")?;
    marketplace_owned(host, &value, root)
}

fn marketplace_owned(host: Host, value: &Value, root: &Path) -> Result<bool, String> {
    let rows = value
        .as_array()
        .or_else(|| value.get("marketplaces").and_then(Value::as_array))
        .ok_or("Unsupported marketplace inventory; refusing to change plugins")?;
    let mut owned = false;
    for row in rows {
        if row.get("name").and_then(Value::as_str) != Some(MARKETPLACE) {
            continue;
        }
        if owned {
            return Err(
                "Client reported multiple Erlkönig marketplaces; refusing to change plugins".into(),
            );
        }
        let path = match host {
            Host::OpenAi => row.get("root"),
            Host::ClaudeCode => row.get("installLocation"),
        }
        .and_then(Value::as_str)
        .ok_or("Marketplace source cannot be verified; refusing to change it")?;
        let source = match host {
            Host::OpenAi => row.get("marketplaceSource").and_then(|source| {
                (source.get("sourceType").and_then(Value::as_str) == Some("local"))
                    .then(|| source.get("source").and_then(Value::as_str))
                    .flatten()
            }),
            Host::ClaudeCode => (row.get("source").and_then(Value::as_str) == Some("directory"))
                .then(|| row.get("path").and_then(Value::as_str))
                .flatten(),
        };
        if Path::new(path) != root || source.map(Path::new) != Some(root) {
            return Err(
                "Another marketplace is named erlkonig-local; leaving it and its plugins unchanged"
                    .into(),
            );
        }
        owned = true;
    }
    Ok(owned)
}

pub fn set_installed(
    host: Host,
    wanted: bool,
    layout: &Layout,
    executable: &Path,
    url: &str,
    cancel: &AtomicBool,
) -> Result<Inventory, String> {
    let program =
        find_client(host).ok_or_else(|| format!("{} CLI was not found", host.program()))?;
    let before = read_inventory(host, &program, &layout.dir, cancel)?;
    if before.account_managed && !before.installed {
        return Err("This Claude plugin is account-managed. Manage or remove it in Customize → Plugins on your Claude account".into());
    }
    let root = marketplace_root(layout);
    let registered = owns_marketplace(host, &program, &root, &layout.dir, cancel)?;
    if before.installed && !registered {
        return Err("Cannot verify the installed plugin's source; leaving it unchanged".into());
    }
    if wanted {
        let root = prepare(layout, executable, url)?;
        if !registered {
            let root_text = root.to_str().ok_or("Marketplace path is not UTF-8")?;
            let mut args = vec!["plugin", "marketplace", "add", root_text];
            if host == Host::OpenAi {
                args.push("--json");
            }
            run(&program, &args, &layout.dir, cancel)?;
            if !owns_marketplace(host, &program, &root, &layout.dir, cancel)? {
                return Err("Client did not register the expected marketplace".into());
            }
        }
        // Explicit Refresh reinstalls even at the same version, so a cached
        // command cannot silently remain pinned to a previous app bundle path.
        // If the second step fails, inventory reports the partial outcome.
        if before.installed {
            let args: &[&str] = match host {
                Host::OpenAi => &["plugin", "remove", PLUGIN_ID, "--json"],
                Host::ClaudeCode => &[
                    "plugin",
                    "uninstall",
                    PLUGIN_ID,
                    "--scope",
                    "user",
                    "--keep-data",
                ],
            };
            run(&program, args, &layout.dir, cancel)?;
        }
        let args: &[&str] = match host {
            Host::OpenAi => &["plugin", "add", PLUGIN_ID, "--json"],
            Host::ClaudeCode => &["plugin", "install", PLUGIN_ID, "--scope", "user"],
        };
        run(&program, args, &layout.dir, cancel)?;
    } else if before.installed {
        let args: &[&str] = match host {
            Host::OpenAi => &["plugin", "remove", PLUGIN_ID, "--json"],
            Host::ClaudeCode => &["plugin", "uninstall", PLUGIN_ID, "--scope", "user"],
        };
        run(&program, args, &layout.dir, cancel)?;
    }
    let after = read_inventory(host, &program, &layout.dir, cancel)?;
    if after.installed != wanted {
        return Err(
            "Client did not confirm the requested state; refresh its plugin inventory".into(),
        );
    }
    Ok(after)
}

pub fn cowork_archive(
    layout: &Layout,
    executable: &Path,
    url: &str,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let root = prepare(layout, executable, url)?;
    let archive = layout.dir.join("erlkonig-claude-plugin.zip");
    // zip updates old archives; use a fresh archive so removed files cannot survive.
    let temporary = layout.dir.join(format!(
        ".claude-plugin-{:x}.zip",
        getrandom::u64().map_err(|e| e.to_string())?
    ));
    let result = (|| {
        run(
            Path::new("/usr/bin/zip"),
            &[
                "-q",
                temporary.to_str().ok_or("Archive path is not UTF-8")?,
                ".claude-plugin/plugin.json",
                ".mcp.json",
                "skills/faculties/SKILL.md",
            ],
            &root.join("claude"),
            cancel,
        )?;
        fs::rename(&temporary, &archive).map_err(|e| e.to_string())?;
        Ok(archive)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_is_not_enablement_or_account_state() {
        let state = parse_inventory(Host::OpenAi, br#"{"installed":[{"pluginId":"erlkonig@erlkonig-local","installed":true,"enabled":false}],"available":[]}"#).unwrap();
        assert!(state.installed && !state.enabled);
        let state = parse_inventory(
            Host::ClaudeCode,
            br#"[{"id":"erlkonig@synced","scope":"synced","enabled":true}]"#,
        )
        .unwrap();
        assert!(!state.installed && state.account_managed);
        assert!(parse_inventory(Host::OpenAi, b"[]").is_err());
        assert!(
            parse_inventory(
                Host::ClaudeCode,
                br#"[{"id":"erlkonig@erlkonig-local","scope":"user"}]"#
            )
            .is_err()
        );
        assert!(
            parse_inventory(
                Host::ClaudeCode,
                br#"[{"id":"erlkonig@erlkonig-local","scope":"project","enabled":true}]"#
            )
            .is_err()
        );
    }
    #[test]
    fn packages_share_skill_and_argv_without_reading_a_secret() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().join("App Data"));
        fs::create_dir(&layout.dir).unwrap();
        let exe = Path::new("/Applications/Erlkönig Test.app/Contents/MacOS/erlkonig");
        let root = prepare(&layout, exe, "http://127.0.0.1:41773/").unwrap();
        assert!(!layout.dir.join("mcp.token").exists());
        assert_eq!(
            fs::read(root.join("openai/skills/faculties/SKILL.md")).unwrap(),
            fs::read(root.join("claude/skills/faculties/SKILL.md")).unwrap()
        );
        let a = fs::read(root.join("openai/.mcp.json")).unwrap();
        assert_eq!(a, fs::read(root.join("claude/.mcp.json")).unwrap());
        let mcp: Value = serde_json::from_slice(&a).unwrap();
        assert_eq!(
            mcp["mcpServers"]["faculties"]["command"],
            exe.to_str().unwrap()
        );
        assert_eq!(mcp["mcpServers"]["faculties"]["args"][0], "mcp-stdio");
        assert!(mcp["mcpServers"]["faculties"].get("env").is_none());
        assert!(!root.join("claude/.codex-plugin").exists());
        assert!(!root.join("openai/.claude-plugin").exists());
    }
    #[test]
    fn staging_refuses_symlinked_package_directory() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().join("data"));
        fs::create_dir(&layout.dir).unwrap();
        std::os::unix::fs::symlink(temp.path(), layout.dir.join("plugins")).unwrap();
        assert!(
            prepare(
                &layout,
                Path::new("/Applications/Erlkonig.app/Contents/MacOS/erlkonig"),
                "http://127.0.0.1:41773/"
            )
            .is_err()
        );
    }
    #[test]
    fn stale_hooks_are_not_installed_or_archived() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().to_path_buf());
        let exe = Path::new("/Applications/Erlkonig.app/Contents/MacOS/erlkonig");
        let root = prepare(&layout, exe, "http://127.0.0.1:41773/").unwrap();
        fs::create_dir(root.join("claude/hooks")).unwrap();
        assert!(prepare(&layout, exe, "http://127.0.0.1:41773/").is_err());
        assert!(root.join("claude/hooks").exists());
        assert!(!layout.dir.join("erlkonig-claude-plugin.zip").exists());
    }
    #[test]
    fn cancellation_prevents_starting_a_client() {
        assert!(
            run(
                Path::new("/not/a/client"),
                &[],
                Path::new("/"),
                &AtomicBool::new(true)
            )
            .unwrap_err()
            .contains("cancelled")
        );
    }
    #[test]
    fn marketplace_name_collision_is_never_ownership() {
        let root = Path::new("/app/data/plugins/erlkonig-local");
        let codex = json!({"marketplaces":[{"name":MARKETPLACE,"root":root,
            "marketplaceSource":{"sourceType":"local","source":root}}]});
        assert_eq!(marketplace_owned(Host::OpenAi, &codex, root), Ok(true));
        assert!(marketplace_owned(Host::OpenAi, &codex, Path::new("/another/root")).is_err());
        let claude =
            json!([{"name":MARKETPLACE,"source":"directory","path":root,"installLocation":root}]);
        assert_eq!(marketplace_owned(Host::ClaudeCode, &claude, root), Ok(true));
        assert_eq!(
            marketplace_owned(Host::ClaudeCode, &json!([]), root),
            Ok(false)
        );
        assert!(marketplace_owned(Host::ClaudeCode, &json!([{"name":MARKETPLACE}]), root).is_err());
        assert!(
            marketplace_owned(
                Host::OpenAi,
                &json!({"marketplaces":[{"name":MARKETPLACE,"root":root,
            "marketplaceSource":{"sourceType":"git","source":"https://example.com/elsewhere"}}]}),
                root
            )
            .is_err()
        );
    }
}
