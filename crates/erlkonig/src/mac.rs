//! Main-thread menu and the one application-owned native lifetime.

use crate::crown::{self, TransferActivity};
use erlkonig_core::{
    config, install,
    node_store::ManagedPile,
    paths::{self, Layout},
    runtime::Runtime,
};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSCellImagePosition, NSImageScaling,
    NSModalResponseOK, NSOpenPanel, NSPasteboard, NSPasteboardTypeString, NSTextField, NSView,
    NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
use winit::window::WindowId;

struct Items {
    status: MenuItem,
    preview: MenuItem,
    copy_id: MenuItem,
    chatgpt: MenuItem,
    claude: MenuItem,
    login: CheckMenuItem,
    piles: Submenu,
    add_pile: MenuItem,
    quit: MenuItem,
}

struct PileItems {
    id: String,
    submenu: Submenu,
    sync: CheckMenuItem,
    maintenance: CheckMenuItem,
    edit: MenuItem,
    details: MenuItem,
    remove: MenuItem,
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
    preview_started: Option<Duration>,
    transfer_bytes: u64,
    animating: bool,
    next_refresh: Instant,
    quitting: Option<Instant>,
    terminate: Arc<AtomicBool>,
    pile_items: Vec<PileItems>,
    config_generation: u64,
}

fn login() -> objc2::rc::Retained<SMAppService> {
    unsafe { SMAppService::mainAppService() }
}

fn set_crown(tray: &TrayIcon, frame: &crown::Frame, mtm: MainThreadMarker) {
    if let Some(button) = tray.ns_status_item().and_then(|item| item.button(mtm)) {
        button.setImageScaling(NSImageScaling::ScaleNone);
        button.setImagePosition(NSCellImagePosition::ImageOnly);
        button.setImage(Some(&frame.template_image()));
    }
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
        if let Some(status) = &status {
            if status.config_generation != self.config_generation {
                self.config_generation = status.config_generation;
                self.rebuild_piles(status);
            }
            for items in &self.pile_items {
                if let Some(pile) = status.managed.iter().find(|pile| pile.id == items.id) {
                    items.sync.set_checked(pile.sync_enabled);
                    items.maintenance.set_checked(pile.maintenance_enabled);
                    let state = if pile.problem.is_some() {
                        "problem"
                    } else if pile.sync_running && pile.maintenance_running {
                        "sync + maintenance"
                    } else if pile.sync_running {
                        "sync"
                    } else if pile.maintenance_running {
                        "local maintenance"
                    } else if pile.sync_enabled || pile.maintenance_enabled {
                        "starting"
                    } else {
                        "off"
                    };
                    items.submenu.set_text(format!("{} · {state}", pile.label));
                }
            }
        }
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
        // Native image/menu changes can wake the loop before WaitUntil expires.
        // Keep processing status and real activity, but do not let those wakeups
        // drive more images or move the existing frame deadline into the future.
        if Instant::now() >= self.next_refresh {
            let reduced = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
            let frame =
                crown::frame_with_preview(now, self.activity, self.preview_started, reduced);
            if self.animating || frame.animating {
                if let Some(tray) = &self.tray {
                    set_crown(tray, &frame, self.mtm);
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
    }

    fn quit(&mut self) {
        self.quitting.get_or_insert_with(Instant::now);
        for item in [
            &self.items.chatgpt,
            &self.items.claude,
            &self.items.copy_id,
            &self.items.preview,
            &self.items.quit,
        ] {
            item.set_enabled(false);
        }
        self.items.login.set_enabled(false);
        self.items.piles.set_enabled(false);
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

    fn rebuild_piles(&mut self, status: &erlkonig_core::status::Status) {
        while self.items.piles.remove_at(0).is_some() {}
        self.pile_items.clear();
        let _ = self.items.piles.append(&self.items.add_pile);
        let _ = self.items.piles.append(&PredefinedMenuItem::separator());
        for pile in &status.managed {
            let submenu = Submenu::new(
                format!(
                    "{}{}",
                    pile.label,
                    if pile.problem.is_some() { " ⚠" } else { "" }
                ),
                true,
            );
            let items = PileItems {
                id: pile.id.clone(),
                submenu: submenu.clone(),
                sync: CheckMenuItem::new("Sync", true, pile.sync_enabled, None),
                maintenance: CheckMenuItem::new(
                    "Local maintenance",
                    true,
                    pile.maintenance_enabled,
                    None,
                ),
                edit: MenuItem::new("Edit configuration…", true, None),
                details: MenuItem::new("Status and paths…", true, None),
                remove: MenuItem::new("Remove from Erlkönig…", true, None),
            };
            let _ = submenu.append_items(&[
                &items.sync,
                &items.maintenance,
                &items.edit,
                &items.details,
                &PredefinedMenuItem::separator(),
                &items.remove,
            ]);
            let _ = self.items.piles.append(&submenu);
            self.pile_items.push(items);
        }
        if status.managed.is_empty() {
            let _ = self.items.piles.append(&MenuItem::new(
                "No managed piles — services are off",
                false,
                None,
            ));
        }
    }

    fn choose_file(&self, title: &str) -> Option<PathBuf> {
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
        let panel = NSOpenPanel::openPanel(self.mtm);
        panel.setTitle(Some(&NSString::from_str(title)));
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(false);
        panel.setCanCreateDirectories(false);
        if panel.runModal() != NSModalResponseOK {
            return None;
        }
        panel
            .URL()?
            .path()
            .map(|path| PathBuf::from(path.to_string()))
    }

    fn edit_fields(&self, pile: &mut ManagedPile) -> Result<bool, String> {
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&NSString::from_str("Managed pile configuration"));
        alert.setInformativeText(&NSString::from_str("Enter exact IDs and collection handles, separated by spaces or commas. Adding a pile leaves Sync and Local maintenance off. MCP continues to use the node pile only."));
        alert.addButtonWithTitle(&NSString::from_str("Save"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let view = NSView::initWithFrame(
            NSView::alloc(self.mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(600.0, 244.0)),
        );
        let values = [
            pile.label.clone(),
            pile.peers
                .iter()
                .map(hex::encode)
                .collect::<Vec<_>>()
                .join(" "),
            pile.sync_collections
                .iter()
                .map(|h| format!("blake3:{}", hex::encode(h.raw)))
                .collect::<Vec<_>>()
                .join(" "),
            pile.maintain_targets
                .iter()
                .map(|h| format!("blake3:{}", hex::encode(h.raw)))
                .collect::<Vec<_>>()
                .join(" "),
        ];
        let mut fields = Vec::new();
        for (i, (name, value)) in [
            "Name",
            "Peer IDs (64 hex)",
            "Sync collections",
            "Maintenance targets",
        ]
        .iter()
        .zip(values)
        .enumerate()
        {
            let y = 222.0 - i as f64 * 60.0;
            let label = NSTextField::labelWithString(&NSString::from_str(name), self.mtm);
            label.setFrame(NSRect::new(NSPoint::new(0.0, y), NSSize::new(600.0, 20.0)));
            view.addSubview(&label);
            let field = NSTextField::textFieldWithString(&NSString::from_str(&value), self.mtm);
            field.setFrame(NSRect::new(
                NSPoint::new(0.0, y - 26.0),
                NSSize::new(600.0, 24.0),
            ));
            view.addSubview(&field);
            fields.push(field);
        }
        alert.setAccessoryView(Some(&view));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return Ok(false);
        }
        let split = |s: String| {
            s.split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        pile.label = fields[0].stringValue().to_string();
        if pile.label.trim().is_empty() {
            return Err("A name is required.".into());
        }
        pile.peers = split(fields[1].stringValue().to_string())
            .iter()
            .map(|peer| {
                if !config::is_node_id(peer) {
                    return Err("Peer IDs must have 64 hexadecimal characters.".to_owned());
                }
                hex::decode(peer)
                    .map_err(|e| e.to_string())?
                    .try_into()
                    .map_err(|_| "Invalid peer ID".to_owned())
            })
            .collect::<Result<_, _>>()?;
        pile.sync_collections = split(fields[2].stringValue().to_string())
            .iter()
            .map(|h| config::parse_handle(h).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        pile.maintain_targets = split(fields[3].stringValue().to_string())
            .iter()
            .map(|h| config::parse_handle(h).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        Ok(true)
    }

    fn add_pile(&self) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        let result = (|| {
            let expected = runtime.config().map_err(|e| e.to_string())?;
            let Some(path) = self.choose_file("Choose the existing pile to manage") else {
                return Ok(());
            };
            let Some(key) = self.choose_file("Choose its existing signing-key file") else {
                return Ok(());
            };
            let label = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut pile = ManagedPile::new(label, path, key);
            if self.edit_fields(&mut pile)? {
                runtime
                    .put_managed(&expected, &pile)
                    .map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            self.alert("Configuration not saved", &error, &["OK"]);
        }
    }

    fn pile_action(&self, selected: &str, action: &str) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        if action == "details" {
            if let Some(pile) = runtime.status().managed.iter().find(|p| p.id == selected) {
                let text = format!(
                    "{}\nPile: {}\nKey file: {}\nSigning identity: {}\nSync: {}\nLocal maintenance: {}\nTransferred: {} bytes\nMaintenance publications: {}\n{}",
                    pile.label,
                    pile.pile,
                    pile.key,
                    pile.node_id.as_deref().unwrap_or("not loaded while off"),
                    if pile.sync_running {
                        "running"
                    } else if pile.sync_enabled {
                        "requested — not running"
                    } else {
                        "off"
                    },
                    if pile.maintenance_running {
                        "running"
                    } else if pile.maintenance_enabled {
                        "requested — not running"
                    } else {
                        "off"
                    },
                    pile.transfer_bytes,
                    pile.maintenance_publications,
                    pile.problem.as_deref().unwrap_or("No reported problem")
                );
                self.alert("Managed pile status", &text, &["OK"]);
            }
            return;
        }
        let result = (|| {
            let expected = runtime.config().map_err(|e| e.to_string())?;
            let id = expected
                .managed_ids()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|id| id.to_string() == selected)
                .ok_or("This entry is no longer configured.")?;
            if action == "remove" {
                if self.alert("Remove managed pile?", "This stops its services and removes the entry from this app. The pile and signing-key files are never deleted.", &["Remove", "Cancel"]) == 0 {
                    runtime.remove_managed(&expected, id).map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
            let mut pile = expected
                .managed(id)
                .map_err(|e| e.to_string())?
                .ok_or("This entry was removed.")?;
            match action {
                "sync" => pile.sync_enabled = !pile.sync_enabled,
                "maintenance" => pile.maintenance_enabled = !pile.maintenance_enabled,
                "edit" => {
                    if !self.edit_fields(&mut pile)? {
                        return Ok(());
                    }
                }
                _ => return Ok(()),
            }
            runtime
                .put_managed(&expected, &pile)
                .map_err(|e| e.to_string())
        })();
        if let Err(error) = result {
            self.alert("Configuration not saved", &error, &["OK"]);
        }
    }

    fn on_menu(&mut self, event: MenuEvent, event_loop: &ActiveEventLoop) {
        let id = event.id();
        if id == self.items.quit.id() {
            self.quit();
        } else if id == self.items.preview.id() {
            if NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion() {
                self.alert(
                    "Sync animation preview",
                    "Reduce Motion is enabled, so the crown stays still. This preview does not start sync or transfer any data.",
                    &["OK"],
                );
            }
            self.preview_started = Some(self.origin.elapsed());
            self.animating = true; // Paint the first frame and schedule its attack.
            self.next_refresh = Instant::now();
        } else if id == self.items.add_pile.id() {
            self.add_pile();
        } else if let Some((entry, action)) = self.pile_items.iter().find_map(|p| {
            [
                (p.sync.id(), "sync"),
                (p.maintenance.id(), "maintenance"),
                (p.edit.id(), "edit"),
                (p.details.id(), "details"),
                (p.remove.id(), "remove"),
            ]
            .into_iter()
            .find(|(item, _)| id == *item)
            .map(|(_, action)| (p.id.clone(), action))
        }) {
            self.pile_action(&entry, action);
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
                            "{}\n{}\n{}\n{}",
                            s.summary(),
                            s.mcp_url.unwrap_or_default(),
                            s.warnings.join("\n"),
                            s.metrics_problem.unwrap_or_default()
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
            match TrayIconBuilder::new()
                .with_menu(Box::new(self.menu.take().unwrap()))
                .with_autosave_name("ai.erlkonig.app.status")
                .build()
            {
                Ok(tray) => {
                    set_crown(&tray, &crown::frame(Duration::ZERO, None, true), self.mtm);
                    // tray-icon also fits its click-target view when setting
                    // the tooltip. Do this after installing our native image,
                    // so its hit area includes the full, wider crown.
                    let _ = tray.set_tooltip(Some("Erlkönig"));
                    self.tray = Some(tray);
                }
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
        preview: MenuItem::new("Preview sync animation", true, None),
        copy_id: MenuItem::new("Copy my ID", true, None),
        chatgpt: MenuItem::new("Connect ChatGPT…", true, None),
        claude: MenuItem::new("Connect Claude…", true, None),
        login: CheckMenuItem::new("Open at login", true, false, None),
        piles: Submenu::new("Managed Piles", true),
        add_pile: MenuItem::new("Add existing pile…", true, None),
        quit: MenuItem::new("Quit Erlkönig", true, None),
    };
    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &items.preview,
        &items.piles,
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
        preview_started: None,
        transfer_bytes: 0,
        animating: false,
        next_refresh: Instant::now(),
        quitting: None,
        terminate,
        pile_items: Vec::new(),
        config_generation: u64::MAX,
    };
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("Erlkönig: {error}");
    }
    // Idempotent fallback for event-loop construction/execution errors.
    app.finish_termination();
}
