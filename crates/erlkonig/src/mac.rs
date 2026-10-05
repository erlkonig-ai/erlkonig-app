//! The macOS menu-bar UI: tray-icon + muda on winit's main-thread loop, and
//! AppKit / ServiceManagement through objc2.

use crate::icon;
use erlkonig_core::config::NodeConfig;
use erlkonig_core::paths::{self, Layout, ensure_private_dir, ensure_symlink};
use erlkonig_core::status::{self, NodeStatus};
use erlkonig_core::words::now_unix;
use erlkonig_core::{init, install};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSPasteboard, NSPasteboardTypeString, NSWorkspace};
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
use winit::window::WindowId;

const AGENT_PLIST: &str = "ai.erlkonig.node.plist";
const ASKED_MARKER: &str = ".asked-background";
const REFRESH: Duration = Duration::from_secs(2);

enum UserEvent {
    Menu(MenuEvent),
}

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
    exe_dir: PathBuf,
    translocated: bool,
    setup_problem: Option<String>,
    items: Items,
    menu: Option<Menu>,
    tray: Option<TrayIcon>,
    flash: Option<(String, Instant)>,
    next_refresh: Instant,
}

fn agent() -> objc2::rc::Retained<SMAppService> {
    unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(AGENT_PLIST)) }
}

fn agent_status() -> SMAppServiceStatus {
    unsafe { agent().status() }
}

impl App {
    fn alert(&self, title: &str, text: &str, buttons: &[&str]) -> usize {
        let app = NSApplication::sharedApplication(self.mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(text));
        for b in buttons {
            alert.addButtonWithTitle(&NSString::from_str(b));
        }
        let response = alert.runModal();
        (response - NSAlertFirstButtonReturn).max(0) as usize
    }

    fn tool(&self, name: &str) -> PathBuf {
        self.exe_dir.join(name)
    }

    /// D, the D/bin link into this bundle, and the first-start question.
    fn setup(&mut self) {
        if let Err(e) = ensure_private_dir(&self.layout.dir) {
            self.setup_problem = Some(format!("Cannot create {}: {e}", self.layout.dir.display()));
            return;
        }
        if self.translocated {
            self.alert(
                "Move Erlkönig to Applications",
                "macOS is running Erlkönig from a temporary copy. Quit it, move Erlkönig.app into your Applications folder, and open it from there.",
                &["OK"],
            );
            return;
        }
        if self.tool("erlkonig-node").is_file()
            && let Err(e) = ensure_symlink(&self.layout.bin_link(), &self.exe_dir)
        {
            self.setup_problem = Some(format!("Cannot link the tools: {e}"));
        }
        let marker = self.layout.dir.join(ASKED_MARKER);
        if !marker.exists() && agent_status() != SMAppServiceStatus::Enabled {
            let _ = std::fs::write(&marker, b"");
            let choice = self.alert(
                "Run Erlkönig in the background?",
                "Erlkönig keeps your pile in sync and maintained while you work, also when this menu is closed. \
You can change this later with Open at login.",
                &["Run in the background", "Not now"],
            );
            if choice == 0 {
                self.set_login(true);
            }
        }
    }

    fn set_login(&mut self, on: bool) {
        let service = agent();
        let result = unsafe {
            if on { service.registerAndReturnError() } else { service.unregisterAndReturnError() }
        };
        if let Err(err) = result {
            let text = err.localizedDescription().to_string();
            self.alert("Open at login", &format!("macOS refused: {text}"), &["OK"]);
        }
        if on && agent_status() == SMAppServiceStatus::RequiresApproval {
            let choice = self.alert(
                "Allow Erlkönig in Login Items",
                "macOS needs your approval before Erlkönig can run in the background. Turn on Erlkönig under System Settings > General > Login Items & Extensions.",
                &["Open System Settings", "Later"],
            );
            if choice == 0 {
                unsafe { SMAppService::openSystemSettingsLoginItems() };
            }
        }
    }

    fn refresh(&mut self) {
        let now = now_unix();
        let text = if self.translocated {
            "Move Erlkönig to Applications to finish setup".to_string()
        } else if let Some(p) = &self.setup_problem {
            p.clone()
        } else if let Some((msg, _)) = self.flash.as_ref().filter(|(_, until)| Instant::now() < *until) {
            msg.clone()
        } else {
            let agent = agent_status();
            let summary = status::summary_at(&self.layout.status(), now);
            if agent == SMAppServiceStatus::RequiresApproval {
                "Waiting for approval in System Settings > Login Items".to_string()
            } else {
                summary
            }
        };
        self.items.status.set_text(text);
        self.items.login.set_checked(agent_status() == SMAppServiceStatus::Enabled);
        let usable = !self.translocated;
        self.items.chatgpt.set_enabled(usable);
        self.items.claude.set_enabled(usable);
        self.items.login.set_enabled(usable);
        self.next_refresh = Instant::now() + REFRESH;
    }

    fn node_id(&self) -> Result<String, String> {
        if let Some(id) = NodeStatus::read(&self.layout.status()).ok().and_then(|s| s.node_id) {
            return Ok(id);
        }
        let r = NodeConfig::load(&self.layout.config())?.resolve(&self.layout.dir);
        let trible = self.tool("trible");
        init::ensure_pile(&r.pile, !r.pile_named)?;
        init::ensure_key(&trible, &r.pile, &r.key, !r.key_named)?;
        init::node_id(&trible, &r.key)
    }

    fn copy_id(&mut self) {
        match self.node_id() {
            Ok(id) => {
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
                let ok = unsafe { pb.setString_forType(&NSString::from_str(&id), NSPasteboardTypeString) };
                let msg = if ok {
                    format!("Copied your ID {}…{}", &id[..6], &id[id.len() - 6..])
                } else {
                    "Could not copy to the clipboard".to_string()
                };
                self.flash = Some((msg, Instant::now() + Duration::from_secs(4)));
            }
            Err(e) => {
                self.alert("Copy my ID", &e, &["OK"]);
            }
        }
    }

    fn plugin_src(&self) -> PathBuf {
        self.exe_dir.join("../Resources/plugin")
    }

    fn install_chatgpt(&mut self) {
        let Some(codex) = install::find_codex() else {
            self.alert(
                "Install in ChatGPT",
                "ChatGPT was not found in Applications. Install the ChatGPT app, open it once, then try again.",
                &["OK"],
            );
            return;
        };
        match install::install_chatgpt(&self.layout, &self.plugin_src(), &codex) {
            Ok(report) => {
                self.alert("Install in ChatGPT", &report.message, &["OK"]);
            }
            Err(e) => {
                self.alert("Install in ChatGPT failed", &e, &["OK"]);
            }
        }
    }

    fn install_claude(&mut self) {
        let zip = match install::build_claude_zip(&self.layout, &self.plugin_src()) {
            Ok((zip, _)) => zip,
            Err(e) => {
                self.alert("Install in Claude failed", &e, &["OK"]);
                return;
            }
        };
        reveal(&zip);
        let steps = install::claude_upload_steps(&zip);
        match install::find_claude() {
            Some(claude) => {
                let choice = self.alert(
                    "Install in Claude",
                    &format!("{steps}\n\nClaude Code is installed on this computer too; Erlkönig can add the plugin there directly."),
                    &["Done", "Also install in Claude Code"],
                );
                if choice == 1 {
                    match install::install_claude_code(&self.layout, &self.plugin_src(), &claude) {
                        Ok(r) => self.alert("Claude Code", &r.message, &["OK"]),
                        Err(e) => self.alert("Claude Code install failed", &e, &["OK"]),
                    };
                }
            }
            None => {
                self.alert("Install in Claude", &steps, &["Done"]);
            }
        }
    }

    fn on_menu(&mut self, event: MenuEvent, event_loop: &ActiveEventLoop) {
        let id = event.id();
        if id == self.items.quit.id() {
            event_loop.exit();
        } else if id == self.items.copy_id.id() {
            self.copy_id();
        } else if id == self.items.chatgpt.id() {
            self.install_chatgpt();
        } else if id == self.items.claude.id() {
            self.install_claude();
        } else if id == self.items.login.id() {
            let on = agent_status() != SMAppServiceStatus::Enabled;
            self.set_login(on);
        }
        self.refresh();
    }
}

fn reveal(path: &Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let urls = NSArray::from_retained_slice(&[url]);
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
}

impl ApplicationHandler<UserEvent> for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        match cause {
            StartCause::Init => {
                // macOS wants the status item created once the loop runs.
                let tray = match icon::menubar_rgba().and_then(|(rgba, w, h)| {
                    Icon::from_rgba(rgba, w, h).map_err(|e| e.to_string())
                }) {
                    Ok(icon) => TrayIconBuilder::new()
                        .with_menu(Box::new(self.menu.take().expect("menu built in main")))
                        .with_icon_templated(icon)
                        .with_autosave_name("ai.erlkonig.app.status")
                        .with_tooltip("Erlkönig")
                        .build(),
                    Err(e) => {
                        eprintln!("erlkonig: icon: {e}");
                        TrayIconBuilder::new()
                            .with_menu(Box::new(self.menu.take().expect("menu built in main")))
                            .with_title("E")
                            .build()
                    }
                };
                match tray {
                    Ok(t) => self.tray = Some(t),
                    Err(e) => eprintln!("erlkonig: cannot create the menu-bar item: {e}"),
                }
                self.setup();
                self.refresh();
            }
            StartCause::ResumeTimeReached { .. } => self.refresh(),
            _ => {}
        }
    }

    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Menu(e) => self.on_menu(e, event_loop),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_refresh));
    }
}

pub fn main() {
    let mtm = MainThreadMarker::new().expect("erlkonig must start on the main thread");
    let exe = std::env::current_exe().expect("current executable");
    let translocated = paths::is_translocated(&exe);
    let exe_dir = std::fs::canonicalize(&exe)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .expect("executable directory");
    let layout = Layout::new(paths::data_dir().expect("data directory"));

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_activation_policy(ActivationPolicy::Accessory)
        .build()
        .expect("event loop");
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(UserEvent::Menu(e));
    }));

    let items = Items {
        status: MenuItem::new("Starting…", false, None),
        copy_id: MenuItem::new("Copy my ID", true, None),
        chatgpt: MenuItem::new("Install in ChatGPT", true, None),
        claude: MenuItem::new("Install in Claude", true, None),
        login: CheckMenuItem::new("Open at login", true, false, None),
        quit: MenuItem::new("Quit Erlkönig Menu", true, None),
    };
    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &PredefinedMenuItem::separator(),
        &items.copy_id,
        &PredefinedMenuItem::separator(),
        &items.chatgpt,
        &items.claude,
        &PredefinedMenuItem::separator(),
        &items.login,
        &PredefinedMenuItem::separator(),
        &items.quit,
    ])
    .expect("menu");

    let mut app = App {
        mtm,
        layout,
        exe_dir,
        translocated,
        setup_problem: None,
        items,
        menu: Some(menu),
        tray: None,
        flash: None,
        next_refresh: Instant::now() + REFRESH,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("erlkonig: {e}");
    }
}
