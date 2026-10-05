//! Child supervision with exponential backoff.
//!
//! Each child runs in its own process group with a scrubbed environment; its
//! stdout and stderr go line by line to `logs/<name>.log`, and sync's
//! `reconciled with peer` lines are kept per peer for the status file.

use crate::envscrub;
use crate::logs::RotatingLog;
use crate::procs;
use crate::status::{ChildStatus, NodeStatus, PeerStatus, parse_reconciled};
use crate::words::{now_unix, rfc3339};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSpec {
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    /// Delay after the first failure.
    pub initial: Duration,
    /// Ceiling for the doubling delay.
    pub max: Duration,
    /// A child that ran at least this long counts as healthy again.
    pub healthy_after: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            healthy_after: Duration::from_secs(60),
        }
    }
}

impl Backoff {
    /// The wait before the next start after `failures` consecutive failures.
    pub fn delay(&self, failures: u32) -> Duration {
        if failures == 0 {
            return Duration::ZERO;
        }
        let factor = 1u32.checked_shl(failures - 1).unwrap_or(u32::MAX);
        self.initial.saturating_mul(factor).min(self.max)
    }
}

type Shared<T> = Arc<Mutex<T>>;

struct Slot {
    spec: ChildSpec,
    program_real: PathBuf,
    child: Option<Child>,
    started: Option<(Instant, u64)>,
    failures: u32,
    restarts: u32,
    spawned_once: bool,
    next_start: Instant,
    last_exit: Option<String>,
    last_error: Option<String>,
    last_line: Shared<Option<String>>,
    log: Shared<RotatingLog>,
    log_path: PathBuf,
}

pub struct Supervisor {
    slots: Vec<Slot>,
    backoff: Backoff,
    log_dir: PathBuf,
    peers: Shared<BTreeMap<String, PeerStatus>>,
    grace: Duration,
}

fn describe_exit(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exited with status {code}"),
        (None, Some(sig)) => format!("killed by signal {sig}"),
        _ => "exited".to_string(),
    }
}

fn pump(
    stream: impl Read + Send + 'static,
    tag: &'static str,
    log: Shared<RotatingLog>,
    last_line: Shared<Option<String>>,
    peers: Shared<BTreeMap<String, PeerStatus>>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
            if line.is_empty() {
                continue;
            }
            let now = now_unix();
            if let Ok(mut log) = log.lock() {
                let _ = log.line(&format!("{} {tag} {line}", rfc3339(now)));
            }
            if let (Some((peer, collections)), Ok(mut peers)) = (parse_reconciled(&line), peers.lock()) {
                peers.insert(peer.clone(), PeerStatus { peer, collections, at: now, line: line.clone() });
            }
            if let Ok(mut last) = last_line.lock() {
                *last = Some(line.chars().take(400).collect());
            }
        }
    });
}

impl Supervisor {
    pub fn new(log_dir: impl Into<PathBuf>, backoff: Backoff) -> Self {
        Supervisor {
            slots: Vec::new(),
            backoff,
            log_dir: log_dir.into(),
            peers: Arc::new(Mutex::new(BTreeMap::new())),
            grace: Duration::from_secs(5),
        }
    }

    pub fn specs(&self) -> Vec<ChildSpec> {
        self.slots.iter().map(|s| s.spec.clone()).collect()
    }

    fn slot_for(&self, spec: ChildSpec) -> Slot {
        let log_path = self.log_dir.join(format!("{}.log", spec.name));
        let program_real = fs::canonicalize(&spec.program).unwrap_or_else(|_| spec.program.clone());
        Slot {
            program_real,
            child: None,
            started: None,
            failures: 0,
            restarts: 0,
            spawned_once: false,
            next_start: Instant::now(),
            last_exit: None,
            last_error: None,
            last_line: Arc::new(Mutex::new(None)),
            log: Arc::new(Mutex::new(RotatingLog::new(
                &log_path,
                RotatingLog::DEFAULT_MAX,
                RotatingLog::DEFAULT_KEEP,
            ))),
            log_path,
            spec,
        }
    }

    /// Makes the supervised set equal to `specs`: unchanged children keep
    /// running, changed or removed ones are stopped, new ones start on the
    /// next tick.
    pub fn set_specs(&mut self, specs: Vec<ChildSpec>) {
        let mut old: Vec<Slot> = std::mem::take(&mut self.slots);
        let mut next = Vec::with_capacity(specs.len());
        for spec in specs {
            if let Some(i) = old.iter().position(|s| s.spec == spec) {
                next.push(old.swap_remove(i));
            } else {
                next.push(self.slot_for(spec));
            }
        }
        for mut slot in old {
            stop_slot(&mut slot, self.grace);
        }
        self.slots = next;
    }

    /// Reaps exited children and starts the ones whose backoff has elapsed.
    pub fn tick(&mut self) {
        let now = Instant::now();
        for i in 0..self.slots.len() {
            let backoff = self.backoff;
            let slot = &mut self.slots[i];
            if let Some(child) = slot.child.as_mut() {
                match child.try_wait() {
                    Ok(None) => continue,
                    Ok(Some(status)) => {
                        let ran = slot.started.map(|(t, _)| now - t).unwrap_or_default();
                        if ran >= backoff.healthy_after {
                            slot.failures = 0;
                        }
                        slot.failures += 1;
                        let what = describe_exit(status);
                        let last = slot.last_line.lock().ok().and_then(|l| l.clone());
                        slot.last_error = Some(match last {
                            Some(line) => format!("{what}; last output: {line}"),
                            None => what.clone(),
                        });
                        let delay = backoff.delay(slot.failures);
                        let note = format!(
                            "{} {} {what}; next start in {delay:?}",
                            rfc3339(now_unix()),
                            slot.spec.name
                        );
                        if let Ok(mut log) = slot.log.lock() {
                            let _ = log.line(&note);
                        }
                        slot.last_exit = Some(what);
                        slot.child = None;
                        slot.started = None;
                        slot.next_start = now + delay;
                    }
                    Err(e) => {
                        slot.last_error = Some(format!("cannot wait for child: {e}"));
                        continue;
                    }
                }
            }
            if slot.child.is_none() && now >= slot.next_start {
                self.spawn(i);
            }
        }
    }

    fn spawn(&mut self, i: usize) {
        let peers = self.peers.clone();
        let backoff = self.backoff;
        let slot = &mut self.slots[i];
        let mut cmd = Command::new(&slot.spec.program);
        cmd.args(&slot.spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        envscrub::apply(&mut cmd);
        if let Some(cwd) = &slot.spec.cwd {
            cmd.current_dir(cwd);
        }
        if slot.spawned_once {
            slot.restarts += 1;
        }
        slot.spawned_once = true;
        match cmd.spawn() {
            Ok(mut child) => {
                if let Some(out) = child.stdout.take() {
                    pump(out, "out", slot.log.clone(), slot.last_line.clone(), peers.clone());
                }
                if let Some(err) = child.stderr.take() {
                    pump(err, "err", slot.log.clone(), slot.last_line.clone(), peers);
                }
                let note = format!(
                    "{} started {} pid {}: {} {}",
                    rfc3339(now_unix()),
                    slot.spec.name,
                    child.id(),
                    slot.spec.program.display(),
                    slot.spec.args.iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
                );
                if let Ok(mut log) = slot.log.lock() {
                    let _ = log.line(&note);
                }
                slot.started = Some((Instant::now(), now_unix()));
                slot.child = Some(child);
            }
            Err(e) => {
                slot.failures += 1;
                slot.last_error = Some(format!("cannot start {}: {e}", slot.spec.program.display()));
                slot.next_start = Instant::now() + backoff.delay(slot.failures);
            }
        }
    }

    /// Stops every child: SIGTERM to its group, SIGKILL after the grace period.
    pub fn stop_all(&mut self) {
        for slot in &mut self.slots {
            stop_slot(slot, self.grace);
        }
    }

    pub fn statuses(&self) -> Vec<ChildStatus> {
        let now = Instant::now();
        self.slots
            .iter()
            .map(|s| ChildStatus {
                name: s.spec.name.clone(),
                state: if s.child.is_some() { "running" } else { "waiting" }.into(),
                pid: s.child.as_ref().map(Child::id),
                program: s.program_real.clone(),
                running_since: s.started.map(|(_, at)| at),
                restarts: s.restarts,
                last_exit: s.last_exit.clone(),
                last_error: s.last_error.clone(),
                next_start_in_secs: s
                    .child
                    .is_none()
                    .then(|| s.next_start.saturating_duration_since(now).as_secs()),
                log: s.log_path.clone(),
            })
            .collect()
    }

    pub fn peers(&self) -> Vec<PeerStatus> {
        self.peers.lock().map(|p| p.values().cloned().collect()).unwrap_or_default()
    }
}

fn stop_slot(slot: &mut Slot, grace: Duration) {
    let Some(mut child) = slot.child.take() else { return };
    procs::signal_group(child.id(), libc::SIGTERM);
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                slot.last_exit = Some(describe_exit(status));
                break;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            _ => {
                procs::signal_group(child.id(), libc::SIGKILL);
                let _ = child.wait();
                slot.last_exit = Some("killed after the grace period".into());
                break;
            }
        }
    }
    if let Ok(mut log) = slot.log.lock() {
        let _ = log.line(&format!("{} stopped {}", rfc3339(now_unix()), slot.spec.name));
    }
}

/// Stops children a crashed predecessor left running. Only pids recorded in
/// its status file are touched, and only when the kernel says that pid still
/// runs the very program that was recorded.
pub fn reap_stale(previous: &NodeStatus, grace: Duration) -> Vec<String> {
    let mut notes = Vec::new();
    if procs::alive(previous.pid) && previous.pid != std::process::id() {
        return notes; // its owner is alive; not ours to touch
    }
    for c in &previous.children {
        let Some(pid) = c.pid else { continue };
        if pid == std::process::id() || !procs::alive(pid) {
            continue;
        }
        let running: Option<PathBuf> = procs::exe_path(pid).and_then(|p| fs::canonicalize(p).ok());
        let recorded = fs::canonicalize(&c.program).unwrap_or_else(|_| c.program.clone());
        if running.as_deref() != Some(recorded.as_path()) {
            continue;
        }
        procs::signal_group(pid, libc::SIGTERM);
        let deadline = Instant::now() + grace;
        while procs::alive(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        if procs::alive(pid) {
            procs::signal_group(pid, libc::SIGKILL);
        }
        notes.push(format!("stopped stale {} child pid {pid} ({})", c.name, recorded.display()));
    }
    notes
}

/// `program` exists and is executable.
pub fn is_executable(program: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(program).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let b = Backoff::default();
        assert_eq!(b.delay(0), Duration::ZERO);
        assert_eq!(b.delay(1), Duration::from_secs(1));
        assert_eq!(b.delay(2), Duration::from_secs(2));
        assert_eq!(b.delay(4), Duration::from_secs(8));
        assert_eq!(b.delay(7), Duration::from_secs(60));
        assert_eq!(b.delay(200), Duration::from_secs(60));
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("erlkonig-sup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn sh(name: &str, script: &str) -> ChildSpec {
        ChildSpec {
            name: name.into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: None,
        }
    }

    #[test]
    fn a_failing_child_is_restarted_with_backoff_and_logged() {
        let d = scratch("fail");
        let fast = Backoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(400),
            healthy_after: Duration::from_secs(60),
        };
        let mut sup = Supervisor::new(&d, fast);
        sup.set_specs(vec![sh("flaky", "echo reconciled with peer abc: 2 collections; exit 3")]);
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(1500) {
            sup.tick();
            thread::sleep(Duration::from_millis(20));
        }
        let st = &sup.statuses()[0];
        // 100 + 200 + 400 + 400 ms of backoff fit in 1.5 s: 3 or 4 restarts.
        assert!((2..=5).contains(&st.restarts), "restarts {}", st.restarts);
        assert_eq!(st.last_exit.as_deref(), Some("exited with status 3"));
        assert!(st.last_error.as_deref().unwrap().contains("reconciled with peer"));
        let peers = sup.peers();
        assert_eq!(peers.len(), 1);
        assert_eq!((peers[0].peer.as_str(), peers[0].collections), ("abc", 2));
        let log = fs::read_to_string(d.join("flaky.log")).unwrap();
        assert!(log.contains("out reconciled with peer abc: 2 collections"), "{log}");
        assert!(log.contains("exited with status 3; next start in"), "{log}");
        sup.stop_all();
    }

    #[test]
    fn children_get_a_scrubbed_environment_and_stop_on_sigterm() {
        let d = scratch("env");
        let envfile = d.join("env.txt");
        // SAFETY: tests in this module do not read these variables concurrently.
        unsafe {
            std::env::set_var("TRIBLESPACE_KEY", "/leak");
            std::env::set_var("PERSONA", "leak");
        }
        let mut sup = Supervisor::new(&d, Backoff::default());
        sup.set_specs(vec![sh(
            "env",
            &format!("env > '{}'; exec sleep 30", envfile.display()),
        )]);
        sup.tick();
        let t0 = Instant::now();
        while !envfile.exists() && t0.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_millis(100));
        let env = fs::read_to_string(&envfile).unwrap();
        assert!(!env.contains("TRIBLESPACE_KEY") && !env.contains("PERSONA"), "{env}");
        assert!(env.contains("PATH="));
        let pid = sup.statuses()[0].pid.unwrap();
        let t1 = Instant::now();
        sup.stop_all();
        assert!(t1.elapsed() < Duration::from_secs(2), "sleep should die on SIGTERM");
        assert!(!procs::alive(pid));
    }

    #[test]
    fn unchanged_specs_keep_their_child() {
        let d = scratch("keep");
        let mut sup = Supervisor::new(&d, Backoff::default());
        let a = sh("a", "exec sleep 30");
        sup.set_specs(vec![a.clone()]);
        sup.tick();
        let pid = sup.statuses()[0].pid.unwrap();
        sup.set_specs(vec![a.clone(), sh("b", "exec sleep 30")]);
        sup.tick();
        assert_eq!(sup.statuses()[0].pid, Some(pid));
        sup.set_specs(vec![sh("b", "exec sleep 30")]);
        assert!(!procs::alive(pid) || {
            thread::sleep(Duration::from_millis(100));
            !procs::alive(pid)
        });
        assert_eq!(sup.statuses().len(), 1);
        sup.stop_all();
    }
}
