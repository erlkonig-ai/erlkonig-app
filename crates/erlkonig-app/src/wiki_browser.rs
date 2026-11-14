//! One app-owned GUI child, using the same executable and existing node binding.
//! The inherited pipe carries paths/public identity once, then only lifetime.
mod window;

use erlkonig_core::config::NodeBinding;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};

const HANDOFF_LIMIT: usize = 8192;

fn close_window(stop: &AtomicBool, window: &OnceLock<egui::Context>) {
    stop.store(true, Ordering::Release);
    if let Some(ctx) = window.get() {
        // Like NotebookConfig's ctrl-c handler, wake the event loop directly.
        // An idle/hidden window must not need another card draw to see EOF.
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
    }
}

fn attach_window(stop: &AtomicBool, window: &OnceLock<egui::Context>, ctx: &egui::Context) {
    let _ = window.set(ctx.clone());
    // Covers EOF before the first frame, including the attach/EOF race.
    if stop.load(Ordering::Acquire) {
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
    }
}

pub(crate) struct BrowserChild {
    child: Child,
    lifetime: Option<ChildStdin>,
}

impl BrowserChild {
    pub(crate) fn open(binding: &NodeBinding) -> Result<Self, String> {
        let mut handoff = serde_json::to_vec(binding)
            .map_err(|_| "Cannot prepare the Wiki browser.".to_owned())?;
        if handoff.len() >= HANDOFF_LIMIT {
            return Err("Node paths are too long for the Wiki browser.".into());
        }
        handoff.push(b'\n');
        let executable = std::env::current_exe()
            .map_err(|_| "Cannot locate this Erlkonig executable.".to_owned())?;
        let mut child = Command::new(executable)
            .arg("browse-wiki")
            .env_remove("GORBIE_EDITOR")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| "Cannot open the Wiki browser.".to_owned())?;
        let mut lifetime = child.stdin.take().expect("piped child stdin");
        if lifetime.write_all(&handoff).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("The Wiki browser did not accept its node binding.".into());
        }
        Ok(Self {
            child,
            lifetime: Some(lifetime),
        })
    }

    pub(crate) fn stop(&mut self) {
        self.lifetime.take();
    }

    /// Poll/reap on the existing app tick. The app's existing final shutdown
    /// fallback owns force termination; this child adds no shorter deadline.
    pub(crate) fn finished(&mut self) -> bool {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return true;
        }
        false
    }
}

impl Drop for BrowserChild {
    fn drop(&mut self) {
        self.lifetime.take();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn read_handoff(input: &mut impl Read) -> Result<NodeBinding, String> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0];
        if input
            .read(&mut byte)
            .map_err(|_| "Erlkonig is unavailable.".to_owned())?
            == 0
        {
            return Err("Erlkonig is unavailable. Open the Wiki browser from its menu.".into());
        }
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() == HANDOFF_LIMIT {
            return Err("Invalid Wiki browser handoff.".into());
        }
        bytes.push(byte[0]);
    }
    serde_json::from_slice(&bytes).map_err(|_| "Invalid Wiki browser handoff.".into())
}

/// Must be dispatched before the node CLI, native menu or Runtime startup.
/// No default paths, ambient Faculty variables, token or locator fallback.
pub fn run(args: &[std::ffi::OsString]) -> Result<bool, String> {
    if args.first().is_none_or(|arg| arg != "browse-wiki") {
        return Ok(false);
    }
    if args.len() != 1 {
        return Err("The Wiki browser is opened from Erlkonig's menu.".into());
    }
    let binding = read_handoff(&mut std::io::stdin())?;
    let stop = Arc::new(AtomicBool::new(false));
    let window = Arc::new(OnceLock::new());
    let watching = stop.clone();
    let closing = window.clone();
    let monitor = std::thread::Builder::new()
        .name("wiki-parent".into())
        .spawn(move || {
            let mut input = std::io::stdin();
            while !watching.load(Ordering::Relaxed) {
                let mut descriptor = libc::pollfd {
                    fd: libc::STDIN_FILENO,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // stdin is the dedicated inherited lifetime pipe. Poll bounds the
                // join when the user closes the window while its parent stays open.
                let ready = unsafe { libc::poll(&mut descriptor, 1, 100) };
                if ready > 0 {
                    let mut byte = [0];
                    if !matches!(input.read(&mut byte), Ok(1)) {
                        close_window(&watching, &closing);
                        break;
                    }
                }
            }
        })
        .map_err(|_| "Cannot observe the parent application's lifetime.".to_owned())?;
    let result = window::run(binding, stop.clone(), window);
    stop.store(true, Ordering::Relaxed);
    let _ = monitor.join();
    result.map(|()| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn parent_eof_wakes_and_closes_an_attached_or_later_window() {
        for early in [false, true] {
            let stop = AtomicBool::new(false);
            let window = OnceLock::new();
            let ctx = egui::Context::default();
            let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let observed = wakes.clone();
            ctx.set_request_repaint_callback(move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
            });
            if early { close_window(&stop, &window); }
            attach_window(&stop, &window, &ctx);
            if !early { close_window(&stop, &window); }
            assert!(stop.load(Ordering::Acquire));
            assert!(wakes.load(Ordering::Relaxed) > 0);
            let output = ctx.run(egui::RawInput::default(), |_| {});
            assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close)));
        }
    }
    #[test]
    fn bounded_handoff_never_has_a_default_binding() {
        assert!(read_handoff(&mut &b""[..]).is_err());
        assert!(read_handoff(&mut &b"{}\n"[..]).is_err());
        assert!(read_handoff(&mut &vec![b'x'; HANDOFF_LIMIT + 2][..]).is_err());
        let binding = NodeBinding {
            pile: "/existing/data.pile".into(),
            key: "/existing/data.key".into(),
            signer: [7; 32],
        };
        let mut bytes = serde_json::to_vec(&binding).unwrap();
        bytes.push(b'\n');
        assert_eq!(read_handoff(&mut &bytes[..]).unwrap(), binding);
    }
    #[test]
    fn parent_pipe_closure_exits_and_reaps_the_owned_child() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "cat >/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let lifetime = child.stdin.take();
        let mut browser = BrowserChild { child, lifetime };
        browser.stop();
        let deadline = Instant::now() + Duration::from_secs(4);
        while !browser.finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(browser.finished());
        assert!(browser.child.try_wait().unwrap().is_some());
    }
}
