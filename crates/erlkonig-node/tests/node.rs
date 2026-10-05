//! End-to-end tests of the supervisor binary against a fake `trible` and
//! `faculties` (shell scripts), so they run anywhere without a real pile.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const NODE: &str = env!("CARGO_BIN_EXE_erlkonig-node");
const ID: &str = "6b6e39a755a1533accdd592d6312cf59228041104ee6cf854e3365c437f67cfb";
const PEER: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const HANDLE: &str = "blake3:00a12ca4fc0122d7ed58c9e29d874fe0f84eae5adca15fb8073a825903af65e5";

const FAKE_TRIBLE: &str = r#"#!/bin/sh
case "$1 $2 $3" in
"pile signing-key init")
    key=""
    while [ $# -gt 0 ]; do [ "$1" = --key ] && key=$2; shift; done
    [ -e "$key" ] && exit 0
    umask 077
    printf '%s' "$(od -A n -t x1 -N 32 /dev/urandom | tr -d ' \n')" > "$key.tmp" && ln "$key.tmp" "$key"
    rm -f "$key.tmp"
    echo "path: $key"
    ;;
"pile net identity")
    echo "node: __ID__"
    ;;
"pile collection maintain-all")
    env > maintain.env
    echo "$@" > maintain.args
    echo "maintenance pass 1 done in 0.0 s"
    exec sleep 600
    ;;
"pile net sync")
    echo "$@" > sync.args
    echo "reconciled with peer __PEER__: 2 collections" >&2
    exec sleep 600
    ;;
*) echo "fake trible: unexpected $*" >&2; exit 2 ;;
esac
"#;

const FAKE_FACULTIES: &str = r#"#!/bin/sh
echo "argv: $*"
echo "cwd: $(pwd)"
env | sed 's/^/env: /'
"#;

struct Fixture {
    root: PathBuf,
    bin: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("node-{name}"));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for (file, text) in [
            ("trible", FAKE_TRIBLE.replace("__ID__", ID).replace("__PEER__", PEER)),
            ("faculties", FAKE_FACULTIES.to_string()),
        ] {
            fs::write(bin.join(file), text).unwrap();
            fs::set_permissions(bin.join(file), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let data = root.join("D");
        Fixture { root, bin, data }
    }

    /// `erlkonig-node <args> --bin-dir <fake tools>`, with ERLKONIG_DATA_DIR
    /// naming the fixture's D, and a planted ERLKONIG_BIN_DIR that must be
    /// ignored (if it were honoured, nothing would start).
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(NODE);
        c.args(args)
            .arg("--bin-dir")
            .arg(&self.bin)
            .env("ERLKONIG_DATA_DIR", &self.data)
            .env("ERLKONIG_BIN_DIR", self.root.join("planted-bin"))
            .env("TRIBLESPACE_KEY", "/must/not/leak")
            .env("PERSONA", "must-not-leak")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }

    fn start(&self) -> Child {
        self.cmd(&["run"]).spawn().unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.data.join("logs/node.log")).unwrap_or_default()
    }

    fn status(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&fs::read(self.data.join("node.status")).ok()?).ok()
    }
}

fn wait_for<T>(what: &str, timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t0.elapsed() < timeout, "timed out waiting for {what}");
        sleep(Duration::from_millis(100));
    }
}

fn sigterm_and_wait(mut child: Child) -> std::process::ExitStatus {
    unsafe { libc_kill(child.id() as i32, 15) };
    let t0 = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(t0.elapsed() < Duration::from_secs(15), "node did not stop on SIGTERM");
        sleep(Duration::from_millis(50));
    }
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn first_start_lock_and_sigterm() {
    let fx = Fixture::new("first");
    let node = fx.start();
    let status = wait_for("node id in status", Duration::from_secs(10), || {
        fx.status().filter(|s| s["node_id"] == ID)
    });
    assert_eq!(status["state"], "running");
    assert_eq!(mode(&fx.data), 0o700);
    assert_eq!(fs::metadata(fx.data.join("self.pile")).unwrap().len(), 0);
    assert_eq!(mode(&fx.data.join("self.key")), 0o600);
    let key = fs::read(fx.data.join("self.key")).unwrap();
    assert_eq!(key.len(), 64);
    assert_eq!(status["children"].as_array().unwrap().len(), 0, "no children without node.toml");

    let second = fx.cmd(&["run"]).output().unwrap();
    assert!(second.status.success(), "{second:?}");
    assert!(String::from_utf8_lossy(&second.stderr).contains("another erlkonig-node holds"));

    let exit = sigterm_and_wait(node);
    assert!(exit.success(), "{exit:?}");
    assert_eq!(fx.status().unwrap()["state"], "stopped");

    // Restart: the key is reused, never replaced.
    let node = fx.start();
    wait_for("restarted status", Duration::from_secs(10), || {
        fx.status().filter(|s| s["state"] == "running")
    });
    assert_eq!(fs::read(fx.data.join("self.key")).unwrap(), key);
    assert!(sigterm_and_wait(node).success());
    let log = fs::read_to_string(fx.data.join("logs/node.log")).unwrap();
    assert_eq!(log.matches("created a signing key").count(), 1, "{log}");
}

#[test]
fn children_restart_with_backoff_and_report_peers() {
    let fx = Fixture::new("children");
    fs::create_dir_all(&fx.data).unwrap();
    fs::write(
        fx.data.join("node.toml"),
        format!("peers = [\"{PEER}\"]\nsync_collections = [\"{HANDLE}\"]\nmaintain_targets = [\"{HANDLE}\"]\n"),
    )
    .unwrap();
    let node = fx.start();
    let child = |s: &serde_json::Value, name: &str| {
        s["children"].as_array().unwrap().iter().find(|c| c["name"] == name).cloned()
    };
    let first = wait_for("maintain running", Duration::from_secs(10), || {
        fx.status().and_then(|s| child(&s, "maintain")).filter(|c| c["state"] == "running")
    });
    let pid = first["pid"].as_u64().unwrap() as i32;
    let env = wait_for("maintain env", Duration::from_secs(5), || {
        fs::read_to_string(fx.data.join("maintain.env")).ok().filter(|t| t.contains("PATH="))
    });
    assert!(!env.contains("TRIBLESPACE_") && !env.contains("PERSONA") && !env.contains("ERLKONIG_"), "{env}");
    let args = fs::read_to_string(fx.data.join("maintain.args")).unwrap();
    assert!(args.contains(&format!("maintain-all {} {HANDLE} --key", fx.data.join("self.pile").display())), "{args}");
    assert!(args.trim_end().ends_with("--watch"), "{args}");
    let sync_args = wait_for("sync args", Duration::from_secs(5), || fs::read_to_string(fx.data.join("sync.args")).ok());
    assert!(sync_args.contains(&format!("--peers {PEER}")), "{sync_args}");
    assert!(sync_args.contains(&format!("--collection {HANDLE}")), "{sync_args}");
    assert!(sync_args.contains("--direction bidirectional --replication full"), "{sync_args}");

    unsafe { libc_kill(pid, 9) };
    let t_kill = Instant::now();
    let restarted = wait_for("maintain restarted", Duration::from_secs(10), || {
        fx.status()
            .and_then(|s| child(&s, "maintain"))
            .filter(|c| c["state"] == "running" && c["pid"].as_u64() != Some(pid as u64))
    });
    // Backoff after the first failure is one second.
    assert!(t_kill.elapsed() >= Duration::from_millis(900), "{:?}", t_kill.elapsed());
    assert_eq!(restarted["restarts"], 1);
    assert_eq!(restarted["last_exit"], "killed by signal 9");

    let s = wait_for("peer line", Duration::from_secs(5), || {
        fx.status().filter(|s| !s["peers"].as_array().unwrap().is_empty())
    });
    assert_eq!(s["peers"][0]["peer"], PEER);
    assert_eq!(s["peers"][0]["collections"], 2);
    let pids: Vec<i32> = s["children"].as_array().unwrap().iter().filter_map(|c| c["pid"].as_i64()).map(|p| p as i32).collect();
    assert_eq!(pids.len(), 2);

    assert!(sigterm_and_wait(node).success());
    sleep(Duration::from_millis(200));
    for p in pids {
        assert_ne!(unsafe { libc_kill(p, 0) }, 0, "child {p} outlived the node");
    }
    let log = fs::read_to_string(fx.data.join("logs/maintain.log")).unwrap();
    assert!(log.contains("killed by signal 9; next start in 1s"), "{log}");
}

#[test]
fn a_pile_named_in_node_toml_is_never_created() {
    let fx = Fixture::new("named");
    fs::create_dir_all(&fx.data).unwrap();
    let named = fx.root.join("colony.pile");
    fs::write(fx.data.join("node.toml"), format!("pile = \"{}\"\nmaintain_targets = [\"{HANDLE}\"]\n", named.display())).unwrap();
    let node = fx.start();
    let s = wait_for("problem", Duration::from_secs(10), || fx.status().filter(|s| s["problem"].is_string()));
    assert!(s["problem"].as_str().unwrap().contains("does not exist"), "{s}");
    assert!(!named.exists());
    assert_eq!(s["children"].as_array().unwrap().len(), 0);
    assert!(sigterm_and_wait(node).success());
}

#[test]
fn mcp_execs_faculties_with_a_scrubbed_environment() {
    let fx = Fixture::new("mcp");
    let other = fx.root.join("elsewhere");
    let out = fx
        .cmd(&["mcp", "--data-dir", fx.data.to_str().unwrap()])
        .env("ERLKONIG_DATA_DIR", &other)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let pile = fx.data.join("self.pile");
    let key = fx.data.join("self.key");
    assert!(stdout.contains(&format!("argv: mcp --pile {} --key {}", pile.display(), key.display())), "{stdout}");
    assert!(stdout.contains(&format!("cwd: {}", fx.data.canonicalize().unwrap().display())), "{stdout}");
    assert!(!stdout.contains("TRIBLESPACE_") && !stdout.contains("PERSONA") && !stdout.contains("ERLKONIG_"), "{stdout}");
    assert!(pile.exists() && key.exists());
    assert!(!other.exists(), "--data-dir wins over ERLKONIG_DATA_DIR");
}

#[test]
fn mcp_without_faculties_fails_clearly() {
    let fx = Fixture::new("mcp-missing");
    fs::remove_file(fx.bin.join("faculties")).unwrap();
    let out = fx.cmd(&["mcp", "--data-dir", fx.data.to_str().unwrap()]).output().unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "stdout belongs to MCP");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not installed correctly"));
    assert!(!fx.data.exists(), "a broken install creates no data folder");
}

#[test]
fn mcp_ignores_erlkonig_bin_dir() {
    // The plugin's launcher inherits the host app's environment: a leftover
    // ERLKONIG_BIN_DIR must not make it run tools from another folder.
    let fx = Fixture::new("mcp-bin-env");
    let out = Command::new(NODE)
        .args(["mcp", "--data-dir", fx.data.to_str().unwrap()])
        .env("ERLKONIG_BIN_DIR", &fx.bin)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "the fake faculties in ERLKONIG_BIN_DIR ran: {out:?}");
    let own = Path::new(NODE).parent().unwrap().join("faculties");
    assert!(stderr.contains(&format!("{} is missing", own.display())), "{stderr}");
}

#[test]
fn a_failed_prepare_is_retried_without_an_edit() {
    // A pile on a disk that mounts after login: the node must pick it up by
    // itself, not only when node.toml changes.
    let fx = Fixture::new("retry");
    fs::create_dir_all(&fx.data).unwrap();
    let named = fx.root.join("late.pile");
    fs::write(fx.data.join("node.toml"), format!("pile = \"{}\"\nmaintain_targets = [\"{HANDLE}\"]\n", named.display())).unwrap();
    let node = fx.cmd(&["run", "--retry-secs", "1"]).spawn().unwrap();
    wait_for("problem", Duration::from_secs(10), || fx.status().filter(|s| s["problem"].is_string()));
    sleep(Duration::from_millis(1500));
    assert!(!named.exists(), "retrying never creates a named pile");
    fs::write(&named, b"").unwrap();
    let s = wait_for("maintain running after the pile appeared", Duration::from_secs(10), || {
        fx.status().filter(|s| {
            s["problem"].is_null() && s["children"].as_array().is_some_and(|c| c.iter().any(|c| c["state"] == "running"))
        })
    });
    assert_eq!(s["children"][0]["name"], "maintain");
    assert!(sigterm_and_wait(node).success());
    let log = fx.log();
    assert_eq!(log.matches("does not exist").count(), 1, "the same problem is logged once: {log}");
    assert!(log.contains("node.toml applied"), "{log}");
}

#[test]
fn a_launchd_node_waits_for_the_lock_and_takes_over() {
    let fx = Fixture::new("launchd");
    let first = fx.start();
    let first_pid = first.id();
    wait_for("first node running", Duration::from_secs(10), || {
        fx.status().filter(|s| s["pid"] == first_pid && s["state"] == "running")
    });
    // --data-dir, because --launchd otherwise ignores ERLKONIG_DATA_DIR and
    // would use the real default folder.
    let mut waiting = fx.cmd(&["run", "--launchd", "--data-dir", fx.data.to_str().unwrap()]).spawn().unwrap();
    let waiting_pid = waiting.id();
    sleep(Duration::from_secs(3));
    assert!(waiting.try_wait().unwrap().is_none(), "a launchd node must wait, not exit (launchd would restart it)");
    assert_eq!(fx.status().unwrap()["pid"], first_pid);
    assert!(fx.log().contains(&format!("waiting for it (pid {waiting_pid})")), "{}", fx.log());

    assert!(sigterm_and_wait(first).success());
    wait_for("the waiting node took over", Duration::from_secs(10), || {
        fx.status().filter(|s| s["pid"] == waiting_pid && s["state"] == "running")
    });
    assert!(sigterm_and_wait(waiting).success());

    // SIGTERM also stops a node that is still waiting.
    let holder = fx.start();
    let holder_pid = holder.id();
    wait_for("holder running", Duration::from_secs(10), || fx.status().filter(|s| s["pid"] == holder_pid));
    let waiter = fx.cmd(&["run", "--launchd", "--data-dir", fx.data.to_str().unwrap()]).spawn().unwrap();
    sleep(Duration::from_secs(2));
    let t0 = Instant::now();
    assert!(sigterm_and_wait(waiter).success());
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert!(sigterm_and_wait(holder).success());
}

#[cfg(target_os = "macos")]
#[test]
fn an_orphaned_child_is_stopped_even_when_the_recorded_owner_pid_is_reused() {
    let fx = Fixture::new("reap");
    fs::create_dir_all(&fx.data).unwrap();
    // The dead node's pid, now reused by some unrelated live process.
    let mut owner = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    // Its child, orphaned when it died: launchd (pid 1) adopted it.
    let out = Command::new("/bin/sh").args(["-c", "/bin/sleep 60 >/dev/null 2>&1 & echo $!"]).output().unwrap();
    let orphan: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    // The shell has exited, but its forked child may not have exec'd sleep yet.
    wait_for("orphan runs /bin/sleep", Duration::from_secs(5), || {
        let ps = Command::new("/bin/ps").args(["-o", "ppid=,comm=", "-p", &orphan.to_string()]).output().ok()?;
        let line = String::from_utf8_lossy(&ps.stdout).split_whitespace().collect::<Vec<_>>().join(" ");
        (line == "1 /bin/sleep").then_some(())
    });
    let status = serde_json::json!({
        "format": 1, "pid": owner.id(), "state": "running", "started_at": 0, "updated_at": 0,
        "updated": "", "data_dir": fx.data, "pile": null, "key": null, "node_id": null, "problem": null,
        "configured_peers": 0, "sync_collections": 0, "maintain_targets": 1, "peers": [],
        "children": [{ "name": "maintain", "state": "running", "pid": orphan, "program": "/bin/sleep",
                       "running_since": 0, "restarts": 0, "last_exit": null, "last_error": null,
                       "next_start_in_secs": null, "log": "" }]
    });
    fs::write(fx.data.join("node.status"), serde_json::to_vec(&status).unwrap()).unwrap();
    let node = fx.start();
    wait_for("orphan stopped", Duration::from_secs(10), || (unsafe { libc_kill(orphan, 0) } != 0).then_some(()));
    assert!(fx.log().contains(&format!("stopped stale maintain child pid {orphan}")), "{}", fx.log());
    assert!(owner.try_wait().unwrap().is_none(), "the process now holding the old owner pid is not touched");
    assert!(sigterm_and_wait(node).success());
    let _ = owner.kill();
    let _ = owner.wait();
}
