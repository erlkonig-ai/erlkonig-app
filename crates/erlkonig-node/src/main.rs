//! erlkonig-node: the per-user supervisor behind the Erlkonig app.
//!
//! `erlkonig-node [run]` holds `D/node.lock`, creates `D`, the pile and the key
//! on first start, runs the sync and maintenance children that `D/node.toml`
//! asks for, and writes `D/node.status`. The LaunchAgent starts it as
//! `run --launchd`: it then always uses the default `D`, and waits for the
//! lock instead of exiting when another node holds it.
//!
//! `erlkonig-node mcp` is the plugin's MCP launcher: it execs
//! `faculties mcp --pile <pile> --key <key>` with a scrubbed environment.

use erlkonig_core::config::{NodeConfig, Resolved};
use erlkonig_core::install;
use erlkonig_core::logs::RotatingLog;
use erlkonig_core::paths::{self, Layout, ensure_private_dir};
use erlkonig_core::status::{self, NodeStatus};
use erlkonig_core::supervise::{self, Backoff, ChildSpec, Supervisor};
use erlkonig_core::words::{now_unix, rfc3339};
use erlkonig_core::{envscrub, init};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

const USAGE: &str = "\
erlkonig-node: the Erlkonig supervisor

Usage:
  erlkonig-node [run] [--data-dir D] [--launchd] [--retry-secs N]
                                       supervise sync and maintenance (the LaunchAgent runs
                                       this with --launchd: default D, wait for the lock)
  erlkonig-node mcp [--data-dir D]     start the faculties MCP server on D's pile (plugins run this)
  erlkonig-node status [--data-dir D]  print D/node.status in one line, then the file
  erlkonig-node id [--data-dir D]      print this node's id (the public key of its signing key)
  erlkonig-node install-chatgpt [--plugin-src DIR] [--codex PATH] [--data-dir D]
  erlkonig-node install-claude [--plugin-src DIR] [--claude-code] [--claude PATH] [--data-dir D]
  erlkonig-node help | --version

D defaults to ERLKONIG_DATA_DIR, else ~/Library/Application Support/Erlkonig;
`mcp` and `run --launchd` ignore ERLKONIG_DATA_DIR. `trible` and `faculties`
are taken from this executable's folder (tests name another with --bin-dir).
D/node.toml (optional) names the pile, key, peers, sync_collections and
maintain_targets. Children get only HOME, PATH, TMPDIR, USER, LOGNAME, LANG,
LC_ALL and LC_CTYPE from the environment.
";

/// How often a node.toml that could not be applied (a pile on a disk that is
/// not mounted yet, say) is tried again.
const RETRY_SECS: u64 = 30;

#[derive(Default)]
struct Opts {
    data_dir: Option<PathBuf>,
    bin_dir: Option<PathBuf>,
    plugin_src: Option<PathBuf>,
    codex: Option<PathBuf>,
    claude: Option<PathBuf>,
    claude_code: bool,
    launchd: bool,
    retry_secs: Option<u64>,
}

fn parse_opts(args: &[OsString]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next().cloned().map(PathBuf::from).ok_or_else(|| format!("{name} needs a value"))
        };
        match a.to_str() {
            Some("--data-dir") => o.data_dir = Some(value("--data-dir")?),
            Some("--bin-dir") => o.bin_dir = Some(value("--bin-dir")?),
            Some("--launchd") => o.launchd = true,
            Some("--retry-secs") => {
                let v = value("--retry-secs")?;
                let n = v.to_str().and_then(|t| t.parse().ok()).filter(|n| *n > 0);
                o.retry_secs = Some(n.ok_or("--retry-secs needs a positive number of seconds")?);
            }
            Some("--plugin-src") => o.plugin_src = Some(value("--plugin-src")?),
            Some("--codex") => o.codex = Some(value("--codex")?),
            Some("--claude") => o.claude = Some(value("--claude")?),
            Some("--claude-code") => o.claude_code = true,
            _ => return Err(format!("unknown argument {}", a.to_string_lossy())),
        }
    }
    Ok(o)
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let (cmd, rest) = match args.first().and_then(|a| a.to_str()) {
        Some(c) if !c.starts_with("--") || matches!(c, "--help" | "--version") => (c.to_string(), &args[1..]),
        _ => ("run".to_string(), &args[..]),
    };
    if matches!(cmd.as_str(), "help" | "--help" | "-h") {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if matches!(cmd.as_str(), "--version" | "version") {
        println!("erlkonig-node {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let opts = match parse_opts(rest) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("erlkonig-node: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    // The plugin's launcher inherits the host app's environment, and the
    // LaunchAgent must agree with the menu and with .mcp.json; neither may be
    // steered by ERLKONIG_DATA_DIR.
    let fixed_dir = cmd == "mcp" || (cmd == "run" && opts.launchd);
    let default_dir: fn() -> std::io::Result<PathBuf> =
        if fixed_dir { paths::default_data_dir } else { paths::data_dir };
    let dirs = opts.data_dir.clone().map(Ok).unwrap_or_else(default_dir).and_then(|data| {
        let bin = opts.bin_dir.clone().map(Ok).unwrap_or_else(paths::tool_dir)?;
        Ok((data, bin))
    });
    let (data_dir, bin_dir) = match dirs {
        Ok(d) => d,
        Err(e) => {
            eprintln!("erlkonig-node: {e}");
            return ExitCode::FAILURE;
        }
    };
    let layout = Layout::new(data_dir);
    let tools = Tools(bin_dir);
    let result = match cmd.as_str() {
        "run" => run(&layout, &tools, &opts),
        "mcp" => mcp(&layout, &tools),
        "status" => status_cmd(&layout),
        "id" => id_cmd(&layout, &tools),
        "install-chatgpt" => install_chatgpt(&layout, &opts),
        "install-claude" => install_claude(&layout, &opts),
        other => {
            eprintln!("erlkonig-node: unknown command {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("erlkonig-node: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The folder holding the bundled `trible` and `faculties`.
struct Tools(PathBuf);

impl Tools {
    fn get(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

/// Creates the default pile and key when they are missing; a pile or key
/// named in node.toml must already exist.
fn prepare(r: &Resolved, trible: &Path) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    if init::ensure_pile(&r.pile, !r.pile_named)? {
        notes.push(format!("created an empty pile at {}", r.pile.display()));
    }
    if init::ensure_key(trible, &r.pile, &r.key, !r.key_named)? {
        notes.push(format!("created a signing key at {}", r.key.display()));
    }
    Ok(notes)
}

// ---------------------------------------------------------------- mcp

fn mcp(layout: &Layout, tools: &Tools) -> Result<ExitCode, String> {
    // Check the install before creating anything, so a broken or missing app
    // leaves no folder behind.
    let faculties = tools.get("faculties");
    let trible = tools.get("trible");
    if !supervise::is_executable(&faculties) {
        return Err(format!("Erlkonig is not installed correctly: {} is missing", faculties.display()));
    }
    ensure_private_dir(&layout.dir).map_err(|e| format!("cannot create {}: {e}", layout.dir.display()))?;
    let cfg = NodeConfig::load(&layout.config())?;
    let r = cfg.resolve(&layout.dir);
    for note in prepare(&r, &trible)? {
        eprintln!("erlkonig: {note}");
    }
    let mut cmd = Command::new(&faculties);
    cmd.arg("mcp").arg("--pile").arg(&r.pile).arg("--key").arg(&r.key).current_dir(&layout.dir);
    envscrub::apply(&mut cmd);
    let err = cmd.exec();
    Err(format!("cannot start {}: {err}", faculties.display()))
}

// ---------------------------------------------------------------- status / id

fn status_cmd(layout: &Layout) -> Result<ExitCode, String> {
    let path = layout.status();
    println!("{}", status::summary_at(&path, now_unix()));
    if let Ok(text) = fs::read_to_string(&path) {
        print!("{text}");
    }
    Ok(ExitCode::SUCCESS)
}

fn id_cmd(layout: &Layout, tools: &Tools) -> Result<ExitCode, String> {
    let r = NodeConfig::load(&layout.config())?.resolve(&layout.dir);
    println!("{}", init::node_id(&tools.get("trible"), &r.key)?);
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------- installs

fn plugin_src(opts: &Opts) -> Result<PathBuf, String> {
    match &opts.plugin_src {
        Some(p) => Ok(p.clone()),
        None => paths::bundled_plugin_dir().map_err(|e| e.to_string()),
    }
}

fn install_chatgpt(layout: &Layout, opts: &Opts) -> Result<ExitCode, String> {
    let codex = opts
        .codex
        .clone()
        .or_else(install::find_codex)
        .ok_or("neither ChatGPT's bundled codex CLI nor codex on PATH was found")?;
    let report = install::install_chatgpt(layout, &plugin_src(opts)?, &codex)?;
    for step in &report.steps {
        eprintln!("{step}");
    }
    println!("{}", report.message);
    Ok(ExitCode::SUCCESS)
}

fn install_claude(layout: &Layout, opts: &Opts) -> Result<ExitCode, String> {
    let src = plugin_src(opts)?;
    let (zip, size) = install::build_claude_zip(layout, &src)?;
    println!("wrote {} ({size} bytes)", zip.display());
    println!("{}", install::claude_upload_steps(&zip));
    if opts.claude_code {
        let claude = opts.claude.clone().or_else(install::find_claude).ok_or("claude was not found")?;
        let report = install::install_claude_code(layout, &src, &claude)?;
        for step in &report.steps {
            eprintln!("{step}");
        }
        println!("{}", report.message);
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------- run

struct NodeLog(RotatingLog);

impl NodeLog {
    fn say(&mut self, text: &str) {
        let line = format!("{} {text}", rfc3339(now_unix()));
        eprintln!("erlkonig-node: {text}");
        let _ = self.0.line(&line);
    }
}

fn config_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    fs::metadata(path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
}

fn specs_for(r: &Resolved, trible: &Path, cwd: &Path) -> Vec<ChildSpec> {
    let mut specs = Vec::new();
    if !r.sync_collections.is_empty() {
        let mut args: Vec<OsString> = vec!["pile".into(), "net".into(), "sync".into(), r.pile.clone().into()];
        args.extend(["--key".into(), r.key.clone().into()]);
        for p in &r.peers {
            args.extend(["--peers".into(), p.into()]);
        }
        for h in &r.sync_collections {
            args.extend(["--collection".into(), h.into()]);
        }
        args.extend(["--direction", "bidirectional", "--replication", "full"].map(OsString::from));
        specs.push(ChildSpec { name: "sync".into(), program: trible.to_path_buf(), args, cwd: Some(cwd.to_path_buf()) });
    }
    if !r.maintain_targets.is_empty() {
        let mut args: Vec<OsString> =
            vec!["pile".into(), "collection".into(), "maintain-all".into(), r.pile.clone().into()];
        args.extend(r.maintain_targets.iter().map(OsString::from));
        args.extend(["--key".into(), r.key.clone().into(), "--watch".into()]);
        specs.push(ChildSpec {
            name: "maintain".into(),
            program: trible.to_path_buf(),
            args,
            cwd: Some(cwd.to_path_buf()),
        });
    }
    specs
}

struct NodeState {
    resolved: Option<Resolved>,
    node_id: Option<(PathBuf, String)>,
    problem: Option<String>,
    warnings: Vec<String>,
    /// When to try node.toml again after it could not be applied.
    retry_at: Option<Instant>,
}

impl NodeState {
    /// Records `problem`, logging it only when it is new, so a retry every
    /// few seconds does not fill the log with the same line.
    fn fail(&mut self, log: &mut NodeLog, problem: String) {
        if self.problem.as_deref() != Some(problem.as_str()) {
            log.say(&problem);
        }
        self.problem = Some(problem);
    }
}

fn apply_config(
    layout: &Layout,
    trible: &Path,
    retry: Duration,
    sup: &mut Supervisor,
    st: &mut NodeState,
    log: &mut NodeLog,
) {
    st.retry_at = None;
    let cfg = match NodeConfig::load(&layout.config()) {
        Ok(c) => c,
        Err(e) => {
            // Keep whatever runs; a bad edit must not stop sync. Only an
            // edit can fix the file, so there is no retry.
            st.fail(log, format!("config not applied: {e}"));
            return;
        }
    };
    let r = cfg.resolve(&layout.dir);
    let warnings = std::env::var_os("HOME")
        .filter(|h| !h.is_empty() && cfg!(target_os = "macos"))
        .map(|home| r.protected_folder_warnings(Path::new(&home)))
        .unwrap_or_default();
    if warnings != st.warnings {
        warnings.iter().for_each(|w| log.say(&format!("warning: {w}")));
        st.warnings = warnings;
    }
    match prepare(&r, trible) {
        Ok(notes) => notes.iter().for_each(|n| log.say(n)),
        Err(e) => {
            // A pile on a disk that is not mounted yet, or a trible that an
            // app update briefly removed: try again later without an edit.
            st.fail(log, e);
            st.resolved = Some(r);
            st.retry_at = Some(Instant::now() + retry);
            sup.set_specs(Vec::new());
            return;
        }
    }
    if st.node_id.as_ref().map(|(k, _)| k != &r.key).unwrap_or(true) {
        match init::node_id(trible, &r.key) {
            Ok(id) => {
                log.say(&format!("node id {id}"));
                st.node_id = Some((r.key.clone(), id));
            }
            Err(e) => {
                log.say(&e);
                st.node_id = None;
                st.retry_at = Some(Instant::now() + retry);
            }
        }
    }
    let specs = specs_for(&r, trible, &layout.dir);
    if specs != sup.specs() {
        log.say(&format!(
            "children: {}",
            if specs.is_empty() {
                "none (no sync_collections or maintain_targets in node.toml)".to_string()
            } else {
                specs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")
            }
        ));
    }
    sup.set_specs(specs);
    if st.problem.take().is_some() {
        log.say("node.toml applied");
    }
    st.resolved = Some(r);
}

fn snapshot(layout: &Layout, started_at: u64, state: &str, st: &NodeState, sup: &Supervisor) -> NodeStatus {
    let now = now_unix();
    let r = st.resolved.as_ref();
    NodeStatus {
        format: status::FORMAT,
        pid: std::process::id(),
        state: state.into(),
        started_at,
        updated_at: now,
        updated: rfc3339(now),
        data_dir: layout.dir.clone(),
        pile: r.map(|r| r.pile.clone()),
        key: r.map(|r| r.key.clone()),
        node_id: st.node_id.as_ref().map(|(_, id)| id.clone()),
        problem: st.problem.clone(),
        warnings: st.warnings.clone(),
        configured_peers: r.map_or(0, |r| r.peers.len()),
        sync_collections: r.map_or(0, |r| r.sync_collections.len()),
        maintain_targets: r.map_or(0, |r| r.maintain_targets.len()),
        children: sup.statuses(),
        peers: sup.peers(),
    }
}

/// Takes `D/node.lock`. A node run by hand exits 0 when another holds it. A
/// node run by launchd waits for it instead (checking `stop` each second):
/// exiting would only make launchd start it again, and once the other node
/// stops this one takes over.
fn take_lock(layout: &Layout, launchd: bool, stop: &AtomicBool, log: &mut NodeLog) -> Result<Option<fs::File>, String> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(layout.lock())
        .map_err(|e| format!("cannot open {}: {e}", layout.lock().display()))?;
    let mut waiting = false;
    loop {
        match lock.try_lock() {
            Ok(()) => {
                if waiting {
                    log.say("took over the lock");
                }
                return Ok(Some(lock));
            }
            Err(fs::TryLockError::WouldBlock) if launchd => {
                if !waiting {
                    log.say(&format!(
                        "another erlkonig-node holds {}; waiting for it (pid {})",
                        layout.lock().display(),
                        std::process::id()
                    ));
                    waiting = true;
                }
                if stop.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(fs::TryLockError::WouldBlock) => {
                eprintln!("erlkonig-node: another erlkonig-node holds {}; exiting", layout.lock().display());
                return Ok(None);
            }
            Err(fs::TryLockError::Error(e)) => return Err(format!("cannot lock {}: {e}", layout.lock().display())),
        }
    }
}

fn run(layout: &Layout, tools: &Tools, opts: &Opts) -> Result<ExitCode, String> {
    ensure_private_dir(&layout.dir).map_err(|e| format!("cannot create {}: {e}", layout.dir.display()))?;
    ensure_private_dir(&layout.logs()).map_err(|e| format!("cannot create the log directory: {e}"))?;
    // From here on every error also reaches node.log: under launchd stderr
    // goes nowhere.
    let mut log = NodeLog(RotatingLog::new(
        layout.logs().join("node.log"),
        RotatingLog::DEFAULT_MAX,
        RotatingLog::DEFAULT_KEEP,
    ));
    let result = supervise_loop(layout, tools, opts, &mut log);
    if let Err(e) = &result {
        log.say(&format!("exiting: {e}"));
    }
    result
}

fn supervise_loop(layout: &Layout, tools: &Tools, opts: &Opts, log: &mut NodeLog) -> Result<ExitCode, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let reload = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, stop.clone()).map_err(|e| e.to_string())?;
    }
    signal_hook::flag::register(signal_hook::consts::SIGHUP, reload.clone()).map_err(|e| e.to_string())?;

    let Some(mut lock) = take_lock(layout, opts.launchd, &stop, log)? else {
        return Ok(ExitCode::SUCCESS);
    };
    let _ = lock.set_len(0);
    let _ = writeln!(lock, "{}", std::process::id());
    log.say(&format!(
        "starting, pid {}, data directory {}{}",
        std::process::id(),
        layout.dir.display(),
        if opts.launchd { " (LaunchAgent)" } else { "" }
    ));

    if let Ok(previous) = NodeStatus::read(&layout.status()) {
        for note in supervise::reap_stale(&previous, Duration::from_secs(3)) {
            log.say(&note);
        }
    }

    let trible = tools.get("trible");
    if !supervise::is_executable(&trible) {
        log.say(&format!("{} is missing; nothing can start", trible.display()));
    }
    let retry = Duration::from_secs(opts.retry_secs.unwrap_or(RETRY_SECS));
    let started_at = now_unix();
    let mut sup = Supervisor::new(layout.logs(), Backoff::default());
    let mut st = NodeState { resolved: None, node_id: None, problem: None, warnings: Vec::new(), retry_at: None };
    let mut stamp = config_stamp(&layout.config());
    apply_config(layout, &trible, retry, &mut sup, &mut st, log);

    let mut last_write = Instant::now() - Duration::from_secs(60);
    while !stop.load(Ordering::Relaxed) {
        let now_stamp = config_stamp(&layout.config());
        if reload.swap(false, Ordering::Relaxed) || now_stamp != stamp {
            stamp = now_stamp;
            log.say("node.toml changed; applying");
            apply_config(layout, &trible, retry, &mut sup, &mut st, log);
        } else if st.retry_at.is_some_and(|t| Instant::now() >= t) {
            apply_config(layout, &trible, retry, &mut sup, &mut st, log);
        }
        // A new child's pid goes into the status file at once: if this node
        // dies before its next periodic write, its successor must still find
        // the child to stop it.
        let started = sup.tick();
        if started || last_write.elapsed() >= Duration::from_secs(2) {
            if let Err(e) = snapshot(layout, started_at, "running", &st, &sup).write(&layout.status()) {
                log.say(&format!("cannot write the status file: {e}"));
            }
            last_write = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    log.say("stopping");
    sup.stop_all();
    let _ = snapshot(layout, started_at, "stopped", &st, &sup).write(&layout.status());
    log.say("stopped");
    Ok(ExitCode::SUCCESS)
}
