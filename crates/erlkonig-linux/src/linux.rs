//! Linux shell: a StatusNotifierItem tray (pure-Rust KSNI), an egui status
//! window that also carries every prompt, XDG autostart, and the desktop's
//! session-bus services, around the shared application in `erlkonig_app::app`.

mod autostart;
mod bus;
mod window;

use erlkonig_app::app::{App, Login, Shell};
use erlkonig_app::crown::{self, WorkActivity};
use erlkonig_core::paths::{self, Layout};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::{Duration, Instant};
use tray_icon::menu::MenuEvent;
use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};
use window::{Action, StatusWindow};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;
use zbus::blocking::Connection;

/// Everything that reaches the main loop from other threads.
pub(crate) enum UserEvent {
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    /// Another launch, over the session bus, asked for the window.
    Show,
    /// The desktop's `color-scheme` preference changed.
    Scheme(Option<u32>),
}

/// Crown colour for the panel from the freedesktop `color-scheme` value:
/// 1 prefers dark (light crown), 2 prefers light (dark crown), anything else
/// or no answer gives a light grey that reads on the usual dark panels.
pub(crate) fn crown_colour(scheme: Option<u32>) -> [u8; 4] {
    match scheme {
        Some(1) => [0xf2, 0xf2, 0xf2, 0xff],
        Some(2) => [0x1f, 0x1f, 0x1f, 0xff],
        _ => [0xbd, 0xbd, 0xbd, 0xff],
    }
}

/// Hosts scale the pixmap to the panel; one generous square size suffices.
const ICON_SIDE: u32 = 48;

/// The static crown: the resting shape, or the attention shape.
fn tray_icon(needs_attention: bool, colour: [u8; 4]) -> Option<Icon> {
    let frame = crown::frame(Duration::ZERO, None, true, needs_attention);
    Icon::from_rgba(frame.square_rgba(ICON_SIDE, colour), ICON_SIDE, ICON_SIDE).ok()
}

/// Start `xdg-open` without waiting for it; true if it could be started.
fn xdg_open(target: &OsStr) -> bool {
    match Command::new("xdg-open")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            let _ = std::thread::Builder::new()
                .name("xdg-open".into())
                .spawn(move || child.wait());
            true
        }
        Err(_) => false,
    }
}

struct LinuxShell {
    tray: Option<TrayIcon>,
    colour: [u8; 4],
    /// The attention state of the installed icon; `None` forces a redraw.
    shown: Option<bool>,
    want_window: bool,
    bus: Option<Connection>,
    autostart: Option<PathBuf>,
}

impl Shell for LinuxShell {
    fn alert(&mut self, _: &str, _: &str, _: &[&str]) -> Option<usize> {
        self.want_window = true;
        None
    }

    fn open_url(&self, url: &str) {
        if !self
            .bus
            .as_ref()
            .is_some_and(|bus| bus::open_uri(bus, url).is_ok())
        {
            xdg_open(OsStr::new(url));
        }
    }

    fn reveal(&self, path: &Path) {
        if !self
            .bus
            .as_ref()
            .is_some_and(|bus| bus::file_manager(bus, "ShowItems", path).is_ok())
        {
            xdg_open(path.parent().unwrap_or(path).as_os_str());
        }
    }

    fn show_folder(&self, folder: &Path) -> Result<(), &'static str> {
        if self
            .bus
            .as_ref()
            .is_some_and(|bus| bus::file_manager(bus, "ShowFolders", folder).is_ok())
            || xdg_open(folder.as_os_str())
        {
            Ok(())
        } else {
            Err("The file manager could not open the active storage folder.")
        }
    }

    fn login(&self) -> Login {
        if self.autostart.as_deref().is_some_and(autostart::enabled) {
            Login::On
        } else {
            Login::Off
        }
    }

    fn set_login(&self, on: bool) -> Result<(), String> {
        let dir = self
            .autostart
            .as_ref()
            .ok_or("There is no autostart folder because HOME is not set.")?;
        // Disabling only removes the desktop entry; the image may have moved.
        let executable = if on {
            paths::launcher_executable().map_err(|error| error.to_string())?
        } else {
            PathBuf::new()
        };
        autostart::set(dir, on, &executable).map_err(|error| {
            format!(
                "Cannot change {}: {error}",
                dir.join(autostart::FILE_NAME).display()
            )
        })
    }

    fn open_login_settings(&self) {}

    /// Linux has no bundle registry: integrations come from the CLIs alone.
    fn detect_apps(&self) -> (Option<(PathBuf, String)>, bool) {
        (None, false)
    }

    fn crown(&mut self, _: Option<WorkActivity>, needs_attention: bool) {
        if self.shown == Some(needs_attention) {
            return;
        }
        if let Some(tray) = &self.tray {
            let _ = tray.set_icon(tray_icon(needs_attention, self.colour));
        }
        self.shown = Some(needs_attention);
    }

    /// The crown is a static pixmap; there are no display callbacks to stop.
    fn stop_crown(&mut self) {}

    fn show_window(&mut self) {
        self.want_window = true;
    }
}

struct Linux {
    app: App<LinuxShell>,
    window: Option<StatusWindow>,
}

impl Linux {
    fn open_window(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.raise();
            return;
        }
        match StatusWindow::open(event_loop, self.app.view()) {
            Ok(window) => self.window = Some(window),
            Err(error) => {
                eprintln!("Erlkonig: {error}");
                // Without a window nobody can answer: log each prompt and
                // take its last button, which is always OK or Cancel.
                while let Some(prompt) = self.app.view().prompt {
                    eprintln!("Erlkonig: {}: {}", prompt.title, prompt.text);
                    self.app.answer(prompt.buttons.len().saturating_sub(1));
                }
                self.app.shell.want_window = false;
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for Linux {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause == StartCause::Init {
            let mut builder = TrayIconBuilder::new()
                .with_id(bus::NAME)
                .with_menu(Box::new(self.app.native_menu()))
                .with_tooltip("Erlkonig")
                .with_title("Erlkonig");
            if let Some(icon) = tray_icon(false, self.app.shell.colour) {
                builder = builder.with_icon(icon);
            }
            match builder.build() {
                Ok(tray) => {
                    self.app.shell.tray = Some(tray);
                    self.app.shell.shown = Some(false);
                }
                Err(error) => {
                    self.app.problem = Some(format!("Cannot create menu: {error}"));
                    self.app.shell.want_window = true;
                }
            }
            self.app.start();
        }
        self.app.refresh(event_loop);
    }

    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.app.faculties_window_event(id, &event) { return; }
        let Some(window) = &mut self.window else {
            return;
        };
        if window.id() != id {
            return;
        }
        if window.on_event(&event) {
            self.window = None; // Closing hides; Quit is the menu's.
            return;
        }
        let actions = window.take_actions();
        if actions.is_empty() {
            return;
        }
        for action in actions {
            match action {
                Action::Menu(id) => self.app.on_menu(MenuEvent { id }),
                Action::Answer(index) => self.app.answer(index),
            }
        }
        self.app.refresh(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Menu(event) => {
                self.app.on_menu(event);
                self.app.refresh(event_loop);
            }
            UserEvent::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                ..
            })
            | UserEvent::Show => self.app.shell.want_window = true,
            UserEvent::Tray(_) => {}
            UserEvent::Scheme(scheme) => {
                self.app.shell.colour = crown_colour(scheme);
                self.app.shell.shown = None;
                self.app.next_refresh = Instant::now();
                self.app.refresh(event_loop);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.app.shell.want_window {
            self.app.shell.want_window = false;
            self.open_window(event_loop);
        }
        if let Some(window) = &mut self.window {
            window.set_view(self.app.view());
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.app.next_wake()));
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.window = None;
        self.app.finish_termination();
    }
}

pub fn main() {
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("Erlkonig: cannot reach the graphical session: {error}");
            std::process::exit(1);
        }
    };
    let proxy = event_loop.create_proxy();
    let bus = match bus::claim(proxy.clone()) {
        bus::Claim::Primary(connection) => Some(connection),
        bus::Claim::Shown => {
            eprintln!("Erlkonig is already running; asked it to show its window.");
            return;
        }
        bus::Claim::Unavailable(error) => {
            eprintln!(
                "Erlkonig: no session bus ({error}); a second launch will not find this one."
            );
            None
        }
    };
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));
    let tray_proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = tray_proxy.send_event(UserEvent::Tray(event));
    }));
    let scheme = bus.as_ref().and_then(bus::color_scheme);
    if let Some(connection) = &bus {
        let scheme_proxy = proxy.clone();
        let _ = bus::watch_color_scheme(connection, move |scheme| {
            let _ = scheme_proxy.send_event(UserEvent::Scheme(scheme));
        });
    }
    // Never refuse to run without a tray host: show the window instead.
    let want_window = !bus.as_ref().is_some_and(bus::has_tray_host);
    let terminate = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, terminate.clone()).expect("shutdown signal");
    }
    let shell = LinuxShell {
        tray: None,
        colour: crown_colour(scheme),
        shown: None,
        want_window,
        bus,
        autostart: autostart::dir(),
    };
    let layout = Layout::application(paths::default_data_dir().expect("data directory"))
        .expect("node bootstrap location");
    let mut linux = Linux {
        app: App::new(shell, layout, terminate),
        window: None,
    };
    if let Err(error) = event_loop.run_app(&mut linux) {
        eprintln!("Erlkonig: {error}");
    }
    // Idempotent fallback for event-loop construction/execution errors.
    linux.window = None;
    linux.app.finish_termination();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_color_scheme_picks_a_crown_that_reads_on_the_panel() {
        let light = crown_colour(Some(1));
        let dark = crown_colour(Some(2));
        let grey = crown_colour(None);
        assert_eq!(crown_colour(Some(0)), grey);
        assert_eq!(crown_colour(Some(7)), grey);
        let luma = |c: [u8; 4]| u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2]);
        assert!(luma(light) > luma(grey) && luma(grey) > luma(dark));
        assert!(luma(dark) < 3 * 64, "prefer light: a dark crown");
        assert!(luma(light) > 3 * 224, "prefer dark: a light crown");
        assert!([light, dark, grey].iter().all(|c| c[3] == 0xff));
    }

    #[test]
    fn the_tray_icon_is_the_static_crown_and_attention_changes_it() {
        let colour = crown_colour(None);
        let rest = crown::frame(Duration::ZERO, None, true, false);
        let attention = crown::frame(Duration::ZERO, None, true, true);
        assert!(tray_icon(false, colour).is_some());
        assert!(!rest.animating && !attention.animating);
        assert_ne!(
            rest.square_rgba(ICON_SIDE, colour),
            attention.square_rgba(ICON_SIDE, colour)
        );
    }
}
