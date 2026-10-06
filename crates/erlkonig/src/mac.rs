//! Main-thread menu and the one application-owned native lifetime.

use crate::crown::{self, TransferActivity};
use erlkonig_core::{
    install,
    paths::{self, Layout},
    runtime::Runtime,
};
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSPasteboard, NSPasteboardTypeString,
    NSWorkspace,
};
use objc2_foundation::NSString;
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
use winit::window::WindowId;

struct Items {
    status: MenuItem,
    copy_id: MenuItem,
    chatgpt: MenuItem,
    claude: MenuItem,
    login: CheckMenuItem,
    quit: MenuItem,
}

struct App {
    mtm: MainThreadMarker,
    layout: Layout,
    runtime: Option<Runtime>,
    problem: Option<String>,
    items: Items,
    menu: Option<Menu>,
    tray: Option<TrayIcon>,
    origin: Instant,
    activity: Option<TransferActivity>,
    transfer_bytes: u64,
    animating: bool,
    next_refresh: Instant,
    quitting: Option<Instant>,
    terminate: Arc<AtomicBool>,
}

fn login() -> objc2::rc::Retained<SMAppService> {
    unsafe { SMAppService::mainAppService() }
}

impl App {
    fn alert(&self, title: &str, text: &str, buttons: &[&str]) -> usize {
        let app = NSApplication::sharedApplication(self.mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(text));
        for button in buttons {
            alert.addButtonWithTitle(&NSString::from_str(button));
        }
        (alert.runModal() - NSAlertFirstButtonReturn).max(0) as usize
    }

    fn refresh(&mut self, event_loop: &ActiveEventLoop) {
        if self.terminate.load(Ordering::Relaxed) && self.quitting.is_none() {
            self.quit();
        }
        if let Some(since) = self.quitting {
            if since.elapsed() >= Duration::from_secs(15) {
                // The person's Quit is final. A stuck admitted native call
                // cannot leave the app alive indefinitely. Like Force Quit,
                // this fallback cannot promise to flush an unfinished write.
                eprintln!(
                    "Erlkönig: 15-second shutdown grace elapsed; terminating this application process"
                );
                std::process::exit(1);
            }
            self.items
                .status
                .set_text(if since.elapsed() < Duration::from_secs(5) {
                    "Shutting down…"
                } else {
                    "Finishing admitted work… app exits after 15 seconds"
                });
            if self.runtime.as_ref().is_none_or(Runtime::is_finished) {
                if let Some(mut runtime) = self.runtime.take() {
                    if let Err(error) = runtime.finish() {
                        // Quit is final even when cleanup reports an error.
                        // Never put a modal dialog between joined workers and exit.
                        eprintln!("Erlkönig shutdown: {error:#}");
                    }
                }
                event_loop.exit();
            }
            self.next_refresh = Instant::now() + Duration::from_millis(50);
            return;
        }
        let status = self.runtime.as_ref().map(Runtime::status);
        let text = self.problem.clone().unwrap_or_else(|| {
            status
                .as_ref()
                .map(|s| s.summary())
                .unwrap_or_else(|| "Not running".into())
        });
        self.items.status.set_text(text);
        let usable = status
            .as_ref()
            .is_some_and(|s| s.running && s.mcp_url.is_some());
        self.items.chatgpt.set_enabled(usable);
        self.items.claude.set_enabled(usable);
        self.items
            .copy_id
            .set_enabled(status.as_ref().is_some_and(|s| s.node_id.is_some()));
        self.items.login.set_checked(matches!(
            unsafe { login().status() },
            SMAppServiceStatus::Enabled | SMAppServiceStatus::RequiresApproval
        ));
        let now = self.origin.elapsed();
        let bytes = status.as_ref().map_or(0, |s| s.transfer_bytes);
        let changed = bytes > self.transfer_bytes;
        if changed {
            let began_at = self
                .activity
                .filter(|a| now.saturating_sub(a.last_transfer_at) <= crown::SETTLE_AFTER)
                .map_or(now, |a| a.began_at);
            self.activity = Some(TransferActivity {
                began_at,
                last_transfer_at: now,
            });
        }
        self.transfer_bytes = bytes;
        let reduced =
            unsafe { NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion() };
        let frame = crown::frame(now, self.activity, reduced);
        if changed || self.animating {
            let (rgba, w, h) = frame.rgba();
            if let (Some(tray), Ok(icon)) = (&self.tray, Icon::from_rgba(rgba, w, h)) {
                let _ = tray.set_icon(Some(icon));
            }
        }
        self.animating = frame.animating;
        self.next_refresh = Instant::now()
            + if self.animating {
                Duration::from_millis(33)
            } else {
                Duration::from_millis(250)
            };
    }

    fn quit(&mut self) {
        self.quitting.get_or_insert_with(Instant::now);
        for item in [
            &self.items.chatgpt,
            &self.items.claude,
            &self.items.copy_id,
            &self.items.quit,
        ] {
            item.set_enabled(false);
        }
        self.items.login.set_enabled(false);
        if let Some(runtime) = &self.runtime {
            runtime.begin_shutdown();
        }
    }

    /// AppKit can terminate without returning from run_app. Winit delivers
    /// LoopExiting synchronously from applicationWillTerminate, so drain here
    /// before returning to AppKit, with the same final process-exit deadline.
    fn finish_termination(&mut self) {
        if let Some(mut runtime) = self.runtime.take() {
            runtime.begin_shutdown();
            let since = self.quitting.unwrap_or_else(Instant::now);
            while !runtime.is_finished() {
                if since.elapsed() >= Duration::from_secs(15) {
                    eprintln!("Erlkönig: shutdown grace elapsed during application termination");
                    std::process::exit(1);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if let Err(error) = runtime.finish() {
                eprintln!("Erlkönig shutdown: {error:#}");
            }
        }
    }

    fn connect_chatgpt(&self) {
        let Some(url) = self.runtime.as_ref().and_then(|r| r.status().mcp_url) else {
            return;
        };
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        if !PathBuf::from("/Applications/ChatGPT.app").exists()
            && !home.join("Applications/ChatGPT.app").exists()
        {
            self.alert(
                "Connect ChatGPT",
                "ChatGPT was not found in Applications. Install and open it first.",
                &["OK"],
            );
            return;
        }
        if self.alert("Connect ChatGPT", "Add Erlkönig's authenticated HTTP endpoint to this computer's Codex/ChatGPT config? Its credential stays in your private local config. Keep Erlkönig open while using its tools.", &["Connect", "Cancel"]) != 0 { return; }
        match install::install_chatgpt(&self.layout, &home.join(".codex"), &url) {
            Ok(_) => {
                self.alert("ChatGPT configured", "Restart MCP servers in ChatGPT Settings. Missing or rejected credentials are shown by the client; Erlkönig never falls back to unauthenticated access.", &["OK"]);
            }
            Err(error) => {
                self.alert("Connection setup failed", &error, &["OK"]);
            }
        }
    }

    fn connect_claude(&self) {
        if self.alert(
            "Connect Claude",
            install::CLAUDE_DESKTOP_LIMIT,
            &["Configure Claude Code", "Cancel"],
        ) != 0
        {
            return;
        }
        let Some(url) = self.runtime.as_ref().and_then(|r| r.status().mcp_url) else {
            return;
        };
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        match install::install_claude_code(&self.layout, &home.join(".claude.json"), &url) {
            Ok(_) => {
                self.alert("Claude Code configured", "Open a new Claude Code session, then inspect /mcp. This did not install a Claude chat or Cowork connector.", &["OK"]);
            }
            Err(error) => {
                self.alert("Connection setup failed", &error, &["OK"]);
            }
        }
    }

    fn toggle_login(&self) {
        let service = login();
        let on = unsafe { service.status() };
        if on == SMAppServiceStatus::RequiresApproval {
            if self.alert(
                "Open at login",
                "macOS requires approval in System Settings > General > Login Items & Extensions.",
                &["Open Settings", "Cancel"],
            ) == 0
            {
                unsafe { SMAppService::openSystemSettingsLoginItems() };
            }
            return;
        }
        let result = unsafe {
            if on == SMAppServiceStatus::Enabled {
                service.unregisterAndReturnError()
            } else {
                service.registerAndReturnError()
            }
        };
        if let Err(error) = result {
            self.alert(
                "Open at login",
                &error.localizedDescription().to_string(),
                &["OK"],
            );
        }
    }

    fn on_menu(&mut self, event: MenuEvent, event_loop: &ActiveEventLoop) {
        let id = event.id();
        if id == self.items.quit.id() {
            self.quit();
        } else if id == self.items.copy_id.id() {
            if let Some(id) = self.runtime.as_ref().and_then(|r| r.status().node_id) {
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                if !unsafe {
                    pb.setString_forType(&NSString::from_str(&id), NSPasteboardTypeString)
                } {
                    self.alert("Copy my ID", "The clipboard could not be updated.", &["OK"]);
                }
            }
        } else if id == self.items.chatgpt.id() {
            self.connect_chatgpt();
        } else if id == self.items.claude.id() {
            self.connect_claude();
        } else if id == self.items.login.id() {
            self.toggle_login();
        } else if id == self.items.status.id() {
            let text = self.problem.clone().unwrap_or_else(|| {
                self.runtime
                    .as_ref()
                    .map(|r| {
                        let s = r.status();
                        format!(
                            "{}\n{}\n{}",
                            s.summary(),
                            s.mcp_url.unwrap_or_default(),
                            s.warnings.join("\n")
                        )
                    })
                    .unwrap_or_default()
            });
            self.alert("Erlkönig", &text, &["OK"]);
        }
        self.refresh(event_loop);
    }
}

impl ApplicationHandler<MenuEvent> for App {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause == StartCause::Init {
            let (rgba, w, h) = crown::frame(Duration::ZERO, None, true).rgba();
            let icon = Icon::from_rgba(rgba, w, h).expect("crown pixels");
            match TrayIconBuilder::new()
                .with_menu(Box::new(self.menu.take().unwrap()))
                .with_icon_templated(icon)
                .with_autosave_name("ai.erlkonig.app.status")
                .with_tooltip("Erlkönig")
                .build()
            {
                Ok(tray) => self.tray = Some(tray),
                Err(error) => self.problem = Some(format!("Cannot create menu: {error}")),
            }
            if std::env::current_exe().is_ok_and(|p| paths::is_translocated(&p)) {
                self.problem = Some("Move Erlkönig to Applications, then open it again.".into());
            } else {
                match Runtime::start(&self.layout) {
                    Ok(runtime) => self.runtime = Some(runtime),
                    Err(error) => self.problem = Some(format!("{error:#}")),
                }
            }
        }
        self.refresh(event_loop);
    }
    fn resumed(&mut self, _: &ActiveEventLoop) {}
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: MenuEvent) {
        self.on_menu(event, event_loop);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_refresh));
    }
    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.finish_termination();
    }
}

pub fn main() {
    let mtm = MainThreadMarker::new().expect("Erlkönig starts on the main thread");
    let event_loop = EventLoop::<MenuEvent>::with_user_event()
        .with_activation_policy(ActivationPolicy::Accessory)
        .build()
        .expect("event loop");
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(e);
    }));
    let terminate = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, terminate.clone()).expect("shutdown signal");
    }
    let items = Items {
        status: MenuItem::new("Starting…", true, None),
        copy_id: MenuItem::new("Copy my ID", true, None),
        chatgpt: MenuItem::new("Connect ChatGPT…", true, None),
        claude: MenuItem::new("Connect Claude…", true, None),
        login: CheckMenuItem::new("Open at login", true, false, None),
        quit: MenuItem::new("Quit Erlkönig", true, None),
    };
    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &PredefinedMenuItem::separator(),
        &items.copy_id,
        &items.chatgpt,
        &items.claude,
        &PredefinedMenuItem::separator(),
        &items.login,
        &items.quit,
    ])
    .expect("menu");
    let mut app = App {
        mtm,
        layout: Layout::new(paths::default_data_dir().expect("data directory")),
        runtime: None,
        problem: None,
        items,
        menu: Some(menu),
        tray: None,
        origin: Instant::now(),
        activity: None,
        transfer_bytes: 0,
        animating: false,
        next_refresh: Instant::now(),
        quitting: None,
        terminate,
    };
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("Erlkönig: {error}");
    }
    // Idempotent fallback for event-loop construction/execution errors.
    app.finish_termination();
}
