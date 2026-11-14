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
/// The one manual step: Cowork plugins live on the Claude account. The
/// caveats are in README.md and plugin/README.md, not in this dialog.
pub const COWORK_HELP: &str = "In Claude, open Customize → Plugins, choose Add → Upload plugin and pick the prepared ZIP. Remove it there too.";
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
    /// The client application a person recognises in a short message.
    pub fn client(self) -> &'static str {
        match self {
            Self::OpenAi => "Codex",
            Self::ClaudeCode => "Claude Code",
        }
    }
    /// The staged package directory this client installs from.
    fn provider(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::ClaudeCode => "claude",
        }
    }
    fn program(self) -> &'static str {
        match self {
            Self::OpenAi => "codex",
            Self::ClaudeCode => "claude",
        }
    }
}

/// Detection is separate from inventory: a present CLI may fail to report its
/// plugin state, and a desktop app may have no supported management CLI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientAvailability {
    pub openai_app: Option<String>,
    pub claude_desktop: bool,
    pub openai_cli: Option<PathBuf>,
    pub claude_cli: Option<PathBuf>,
}

impl ClientAvailability {
    pub fn openai_label(&self) -> Option<String> {
        match (self.openai_app.as_deref(), self.openai_cli.is_some()) {
            (Some("Codex"), _) | (None, true) => Some("Codex plugin".into()),
            (Some(app), true) => Some(format!("{app} / Codex plugin")),
            (Some(app), false) => Some(format!("{app} plugin")),
            (None, false) => None,
        }
    }

    pub fn cli(&self, host: Host) -> Option<&Path> {
        match host {
            Host::OpenAi => self.openai_cli.as_deref(),
            Host::ClaudeCode => self.claude_cli.as_deref(),
        }
    }

    pub fn any(&self) -> bool {
        self.openai_label().is_some() || self.claude_cli.is_some() || self.claude_desktop
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

/// A missing check value means retain the last observation, not "uninstalled".
pub fn inventory_menu_state(
    state: Option<&Result<Inventory, String>>,
) -> (Option<bool>, bool, &'static str) {
    match state {
        Some(Ok(state)) => (
            Some(state.installed),
            true,
            if state.installed && state.load_errors {
                "load errors"
            } else if state.installed && !state.enabled {
                "disabled"
            } else if state.installed {
                "installed"
            } else if state.account_managed {
                "account-managed"
            } else {
                "not installed"
            },
        ),
        Some(Err(_)) => (None, false, "status unavailable"),
        None => (None, false, "checking…"),
    }
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
            return Err("Erlkonig is installed at another Claude Code scope; manage that installation in Claude Code".into());
        }
        if inventory.installed {
            return Err("Client reported multiple local Erlkonig installations".into());
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
    find_client_with_apps(host, &[PathBuf::from("/Applications/ChatGPT.app")])
}

/// Native callers supply paths returned by NSWorkspace; neither discovery nor
/// this resolver launches a desktop app. Only the known CLI resource is used.
pub fn find_client_with_apps(host: Host, openai_apps: &[PathBuf]) -> Option<PathBuf> {
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
        candidates.extend(openai_apps.iter().map(|app| bundled_openai_cli(app)));
    }
    first_executable(candidates)
}

fn bundled_openai_cli(app: &Path) -> PathBuf {
    app.join("Contents/Resources/codex-cli/bin/codex")
}

fn first_executable(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
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
    inventory_with_client(host, &program, layout, cancel)
}

pub fn inventory_with_client(
    host: Host,
    program: &Path,
    layout: &Layout,
    cancel: &AtomicBool,
) -> Result<Inventory, String> {
    read_inventory(host, program, &layout.dir, cancel)
}

/// Whether the client's own settings name a separate MCP server `erlkonig`.
/// Diagnostic only: a legacy server may be user-authored. Never remove or
/// rewrite it, even when its name or URL looks like an old Erlkonig install.
pub fn manual_config(host: Host) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    match host {
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
    }
}

/// The status-details line for a [`manual_config`] client.
pub fn manual_config_note(host: Host) -> String {
    format!(
        "{}: a separate MCP server named erlkonig is also configured and may duplicate the plugin's tools; Erlkonig leaves it alone.",
        host.client()
    )
}

fn read_inventory(
    host: Host,
    program: &Path,
    cwd: &Path,
    cancel: &AtomicBool,
) -> Result<Inventory, String> {
    let args: &[&str] = match host {
        // Only this local inventory request suppresses remote plugin discovery.
        Host::OpenAi => &[
            "--disable",
            "remote_plugin",
            "plugin",
            "list",
            "--marketplace",
            MARKETPLACE,
            "--json",
        ],
        Host::ClaudeCode => &["plugin", "list", "--json"],
    };
    parse_inventory(host, &run(program, args, cwd, cancel)?)
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

/// Provider directory, manifest directory and manifest of each package.
const PROVIDERS: [(&str, &str, &str); 2] = [
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
];
const SKILL: &[u8] = include_bytes!("../../../plugin/skills/faculties/SKILL.md");

/// The `.mcp.json` this executable stages: its absolute path, data directory
/// and the running endpoint, as separate argument values.
fn mcp_json(layout: &Layout, executable: &Path, url: &str) -> Result<Vec<u8>, String> {
    if !executable.is_absolute() || !layout.dir.is_absolute() {
        return Err("Plugin executable and app data paths must be absolute".into());
    }
    let executable = executable.to_str().ok_or("Executable path is not UTF-8")?;
    let data = layout.dir.to_str().ok_or("Data path is not UTF-8")?;
    let mcp = json!({"mcpServers":{"faculties":{"command":executable,
        "args":["mcp-stdio", "--data-dir", data, "--url", url]}}});
    serde_json::to_vec_pretty(&mcp).map_err(|e| e.to_string())
}

/// Two provider packages, one skill. Code and the account ZIP use exactly the
/// same Claude directory. No credential is read by package/lifecycle code.
pub fn prepare(layout: &Layout, executable: &Path, url: &str) -> Result<PathBuf, String> {
    let mcp = mcp_json(layout, executable, url)?;
    directory(&layout.dir.join("plugins"))?;
    let root = marketplace_root(layout);
    directory(&root)?;
    for (provider, manifest_dir, manifest) in PROVIDERS {
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
        write_atomic(&plugin.join("skills/faculties/SKILL.md"), SKILL)
            .map_err(|e| e.to_string())?;
        write_atomic(&plugin.join(".mcp.json"), &mcp).map_err(|e| e.to_string())?;
    }
    directory(&root.join(".agents"))?;
    directory(&root.join(".agents/plugins"))?;
    json_file(
        &root.join(".agents/plugins/marketplace.json"),
        &json!({
            "name":MARKETPLACE, "interface":{"displayName":"Erlkonig on this computer"},
            "plugins":[{"name":"erlkonig", "source":{"source":"local", "path":"./openai"},
                "policy":{"installation":"AVAILABLE", "authentication":"ON_USE"}, "category":"Productivity"}]
        }),
    )?;
    directory(&root.join(".claude-plugin"))?;
    json_file(
        &root.join(".claude-plugin/marketplace.json"),
        &json!({
            "name":MARKETPLACE, "owner":{"name":"Erlkonig"},
            "plugins":[{"name":"erlkonig", "source":"./claude"}]
        }),
    )?;
    Ok(root)
}

/// A digest of exactly what this executable stages for `host`: the
/// executable path, data directory, endpoint, manifest and skill.
fn package_identity(
    layout: &Layout,
    host: Host,
    executable: &Path,
    url: &str,
) -> Result<String, String> {
    let (_, _, manifest) = PROVIDERS
        .iter()
        .find(|(provider, _, _)| *provider == host.provider())
        .ok_or("Unknown plugin provider")?;
    let mut digest = blake3::Hasher::new();
    for part in [
        &mcp_json(layout, executable, url)?[..],
        manifest.as_bytes(),
        SKILL,
    ] {
        digest.update(&(part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    Ok(digest.finalize().to_hex().to_string())
}

/// Where the identity of the package a client last confirmed installing is
/// recorded: inside the app's own plugin staging directory, nowhere else.
fn installed_record(layout: &Layout, host: Host) -> PathBuf {
    layout
        .dir
        .join("plugins")
        .join(format!("installed-{}", host.provider()))
}

/// Whether `host` last confirmed installing exactly the package this
/// executable would stage now. Clients install a copy, so a moved or updated
/// app (or a new port) leaves an installed plugin pointing at the old one
/// until it is reinstalled. A failed reinstall leaves the record behind, so
/// the next refresh retries that client.
pub fn installed_is_current(layout: &Layout, host: Host, executable: &Path, url: &str) -> bool {
    package_identity(layout, host, executable, url).is_ok_and(|identity| {
        fs::read_to_string(installed_record(layout, host)).is_ok_and(|text| text == identity)
    })
}

/// Quietly bring installed plugins up to date with this executable. A host
/// whose recorded package is current is not even asked. Any other named host
/// whose client still reports our plugin installed is re-staged and
/// reinstalled; one the client reports absent is left absent.
pub fn refresh_if_stale(
    installed: &[(Host, PathBuf)],
    layout: &Layout,
    executable: &Path,
    url: &str,
    cancel: &AtomicBool,
) -> Vec<(Host, Result<Inventory, String>)> {
    installed
        .iter()
        .filter(|(host, _)| !installed_is_current(layout, *host, executable, url))
        .map(|(host, program)| {
            let result = change_with_client(
                *host,
                program,
                Change::Refresh,
                layout,
                executable,
                url,
                cancel,
            );
            (*host, result)
        })
        .collect()
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
                "Client reported multiple Erlkonig marketplaces; refusing to change plugins".into(),
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
    set_installed_with_client(host, &program, wanted, layout, executable, url, cancel)
}

/// What a plugin lifecycle step asks of the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Install,
    Remove,
    /// Reinstall from a freshly staged package, only if already installed.
    Refresh,
}

/// Use the exact CLI whose inventory the menu observed, not a fresh PATH choice.
pub fn set_installed_with_client(
    host: Host,
    program: &Path,
    wanted: bool,
    layout: &Layout,
    executable: &Path,
    url: &str,
    cancel: &AtomicBool,
) -> Result<Inventory, String> {
    let change = if wanted {
        Change::Install
    } else {
        Change::Remove
    };
    change_with_client(host, program, change, layout, executable, url, cancel)
}

/// The only plugin mutations are the client's own commands for our exact
/// plugin ID at user scope; nothing here deletes a file or directory.
pub fn change_with_client(
    host: Host,
    program: &Path,
    change: Change,
    layout: &Layout,
    executable: &Path,
    url: &str,
    cancel: &AtomicBool,
) -> Result<Inventory, String> {
    let wanted = change != Change::Remove;
    let before = read_inventory(host, &program, &layout.dir, cancel)?;
    if change == Change::Refresh && !before.installed {
        return Ok(before);
    }
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
        // Reinstall even at the same version, so a cached command cannot
        // silently remain pinned to a previous app bundle path. If the second
        // step fails, inventory reports the partial outcome.
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
    if wanted {
        // Only a confirmed install records its package. If recording fails,
        // the next inventory merely reinstalls once more.
        if let Ok(identity) = package_identity(layout, host, executable, url) {
            let _ = write_atomic(&installed_record(layout, host), identity.as_bytes());
        }
    }
    Ok(after)
}

/// Info-ZIP's `zip` from the given `PATH`, else the distribution's usual
/// location; the archive step then reports a missing tool as "Cannot start".
fn zip_program(path: Option<&std::ffi::OsStr>) -> PathBuf {
    let on_path = path.map(std::env::split_paths).into_iter().flatten();
    first_executable(on_path.map(|dir| dir.join("zip")))
        .unwrap_or_else(|| PathBuf::from("/usr/bin/zip"))
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
            &zip_program(std::env::var_os("PATH").as_deref()),
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
    fn client_presence_keeps_apps_and_management_separate() {
        // All absent, either app-only, either CLI-only, and mixed/both cases.
        for bits in 0..16 {
            let clients = ClientAvailability {
                openai_app: (bits & 1 != 0).then(|| "ChatGPT".into()),
                claude_desktop: bits & 2 != 0,
                openai_cli: (bits & 4 != 0).then(|| PathBuf::from("/bin/codex")),
                claude_cli: (bits & 8 != 0).then(|| PathBuf::from("/bin/claude")),
            };
            assert_eq!(clients.any(), bits != 0);
            assert_eq!(clients.openai_label().is_some(), bits & 5 != 0);
            assert_eq!(clients.cli(Host::OpenAi).is_some(), bits & 4 != 0);
            assert_eq!(clients.cli(Host::ClaudeCode).is_some(), bits & 8 != 0);
            assert_eq!(clients.claude_desktop, bits & 2 != 0);
            if bits & 5 == 5 {
                assert_eq!(
                    clients.openai_label().as_deref(),
                    Some("ChatGPT / Codex plugin")
                );
            } else if bits & 1 != 0 {
                assert_eq!(clients.openai_label().as_deref(), Some("ChatGPT plugin"));
            } else if bits & 4 != 0 {
                assert_eq!(clients.openai_label().as_deref(), Some("Codex plugin"));
            }
        }
        let clients = ClientAvailability {
            openai_app: Some("Codex".into()),
            openai_cli: Some("/bin/codex".into()),
            ..Default::default()
        };
        assert_eq!(clients.openai_label().as_deref(), Some("Codex plugin"));
    }

    #[test]
    fn inventory_failure_retains_check_and_disabled_stays_installed() {
        let disabled = Ok(Inventory {
            installed: true,
            enabled: false,
            account_managed: false,
            load_errors: false,
        });
        assert_eq!(
            inventory_menu_state(Some(&disabled)),
            (Some(true), true, "disabled")
        );
        let unavailable = Err("CLI inventory failed".into());
        assert_eq!(
            inventory_menu_state(Some(&unavailable)),
            (None, false, "status unavailable")
        );
        assert_eq!(inventory_menu_state(None), (None, false, "checking…"));
        let absent = parse_inventory(Host::ClaudeCode, b"[]");
        assert_eq!(
            inventory_menu_state(Some(&absent)),
            (Some(false), true, "not installed")
        );
        let synced = parse_inventory(
            Host::ClaudeCode,
            br#"[{"id":"erlkonig@synced","scope":"synced","enabled":true}]"#,
        );
        assert_eq!(
            inventory_menu_state(Some(&synced)),
            (Some(false), true, "account-managed")
        );
    }

    #[test]
    fn inventory_argv_is_local_for_codex_and_unchanged_for_claude() {
        for (host, response, expected) in [
            (
                Host::OpenAi,
                r#"{"installed":[{"pluginId":"erlkonig@erlkonig-local","installed":true,"enabled":true}]}"#,
                "--disable\nremote_plugin\nplugin\nlist\n--marketplace\nerlkonig-local\n--json\n",
            ),
            (
                Host::ClaudeCode,
                r#"[{"id":"erlkonig@erlkonig-local","scope":"user","enabled":true}]"#,
                "plugin\nlist\n--json\n",
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let layout = Layout::new(temp.path().to_path_buf());
            let program = temp.path().join(host.program());
            fs::write(
                &program,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > argv.txt\ncat inventory.json\n",
            )
            .unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(temp.path().join("inventory.json"), response).unwrap();
            let state =
                inventory_with_client(host, &program, &layout, &AtomicBool::new(false)).unwrap();
            assert_eq!(
                fs::read_to_string(temp.path().join("argv.txt")).unwrap(),
                expected
            );
            assert_eq!(
                inventory_menu_state(Some(&Ok(state))),
                (Some(true), true, "installed")
            );
        }
    }

    #[test]
    fn bundled_cli_uses_detected_location_and_requires_executable_file() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Elsewhere/My ChatGPT.app");
        let cli = bundled_openai_cli(&app);
        assert_eq!(cli, app.join("Contents/Resources/codex-cli/bin/codex"));
        assert_eq!(first_executable([cli.clone()]), None);
        fs::create_dir_all(cli.parent().unwrap()).unwrap();
        fs::write(&cli, "fixture, never executed").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(first_executable([cli.clone()]), None);
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(first_executable([cli.clone()]), Some(cli));
        assert_eq!(
            first_executable([app, PathBuf::from("relative/codex")]),
            None
        );
    }

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
    fn appimage_mcp_command_keeps_outer_path_and_exact_arguments() {
        let layout = Layout::new("/home/test/App Data");
        let executable = Path::new("/home/test/Applications/Erlkonig 日本語.AppImage");
        let url = "http://127.0.0.1:41773/";
        let value: Value = serde_json::from_slice(&mcp_json(&layout, executable, url).unwrap()).unwrap();
        let server = &value["mcpServers"]["faculties"];
        assert_eq!(server["command"], executable.to_str().unwrap());
        assert_eq!(server["args"], json!(["mcp-stdio", "--data-dir", "/home/test/App Data", "--url", url]));
        assert!(server.get("env").is_none());
    }

    #[test]
    fn packages_share_skill_and_argv_without_reading_a_secret() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().join("App Data"));
        fs::create_dir(&layout.dir).unwrap();
        let exe = Path::new("/Applications/Erlkonig 日本語 Test.app/Contents/MacOS/erlkonig");
        let root = prepare(&layout, exe, "http://127.0.0.1:41773/").unwrap();
        assert!(!layout.dir.join("mcp.token").exists());
        assert_eq!(
            fs::read(root.join("openai/skills/faculties/SKILL.md")).unwrap(),
            fs::read(root.join("claude/skills/faculties/SKILL.md")).unwrap()
        );
        assert!(
            fs::read_to_string(root.join("openai/skills/faculties/SKILL.md"))
                .unwrap()
                .contains("Erlkonig app")
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
        let openai: Value = serde_json::from_slice(
            &fs::read(root.join("openai/.codex-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        let claude: Value = serde_json::from_slice(
            &fs::read(root.join("claude/.claude-plugin/plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(openai["name"], "erlkonig");
        assert_eq!(openai["interface"]["displayName"], "Erlkonig");
        assert_eq!(openai["interface"]["developerName"], "Erlkonig");
        assert_eq!(claude["name"], "erlkonig");
        assert_eq!(claude["displayName"], "Erlkonig");
        assert_eq!(claude["author"]["name"], "Erlkonig");
        let openai_market: Value = serde_json::from_slice(
            &fs::read(root.join(".agents/plugins/marketplace.json")).unwrap(),
        )
        .unwrap();
        let claude_market: Value = serde_json::from_slice(
            &fs::read(root.join(".claude-plugin/marketplace.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(openai_market["name"], MARKETPLACE);
        assert_eq!(
            openai_market["interface"]["displayName"],
            "Erlkonig on this computer"
        );
        assert_eq!(claude_market["name"], MARKETPLACE);
        assert_eq!(claude_market["owner"]["name"], "Erlkonig");
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
    fn cowork_help_is_the_manual_step_only() {
        assert!(COWORK_HELP.contains("Upload plugin"));
        assert!(!COWORK_HELP.contains("Mac"));
        assert!(COWORK_HELP.len() < 140);
    }
    #[test]
    fn zip_is_resolved_on_path_with_a_system_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let (first, second) = (temp.path().join("first"), temp.path().join("second"));
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("zip"), b"not executable").unwrap();
        fs::write(second.join("zip"), b"#!/bin/sh\n").unwrap();
        fs::set_permissions(second.join("zip"), fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([first.as_path(), second.as_path()]).unwrap();
        assert_eq!(zip_program(Some(&path)), second.join("zip"));
        let fallback = Path::new("/usr/bin/zip");
        assert_eq!(
            zip_program(Some(&std::env::join_paths([&first]).unwrap())),
            fallback
        );
        assert_eq!(zip_program(Some(std::ffi::OsStr::new(""))), fallback);
        assert_eq!(zip_program(None), fallback);
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
