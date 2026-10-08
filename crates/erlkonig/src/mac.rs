//! Main-thread menu and the one application-owned native lifetime.

use crate::crown::{self, TransferActivity};
use crate::crown_display::DisplayCrown;
use erlkonig_core::{
    config, install,
    node_store::ManagedPile,
    paths::{self, Layout},
    runtime::Runtime,
    workspace::{self, Discovery, Workspace},
};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSModalResponseOK, NSOpenPanel, NSPasteboard,
    NSPasteboardTypeString, NSPopUpButton, NSTextField, NSView, NSWorkspace,
};
use objc2_foundation::{NSBundle, NSPoint, NSRect, NSSize, NSString, NSURL};
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
    preview: CheckMenuItem,
    copy_id: MenuItem,
    integrations: Submenu,
    no_clients: MenuItem,
    chatgpt: CheckMenuItem,
    openai_manual: MenuItem,
    claude: CheckMenuItem,
    cowork: MenuItem,
    refresh_plugins: MenuItem,
    login: CheckMenuItem,
    piles: Submenu,
    add_pile: MenuItem,
    workspaces: Submenu,
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
    workspace: MenuItem,
}

struct WorkspaceItems {
    id: String,
    edit: MenuItem,
    details: MenuItem,
    copy_id: MenuItem,
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
    crown: Option<DisplayCrown>,
    transfer_bytes: u64,
    next_refresh: Instant,
    quitting: Option<Instant>,
    terminate: Arc<AtomicBool>,
    pile_items: Vec<PileItems>,
    workspace_items: Vec<WorkspaceItems>,
    config_generation: u64,
    plugin_states: [Option<Result<install::Inventory, String>>; 2],
    clients: Option<install::ClientAvailability>,
    plugin_task: Option<PluginTask>,
}

enum PluginResult {
    Inventory {
        clients: install::ClientAvailability,
        states: [Option<Result<install::Inventory, String>>; 2],
    },
    Changed(install::Host, Result<install::Inventory, String>),
    Archive(Result<PathBuf, String>),
}

struct PluginTask {
    cancel: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<PluginResult>,
}

fn host_index(host: install::Host) -> usize {
    match host {
        install::Host::OpenAi => 0,
        install::Host::ClaudeCode => 1,
    }
}

fn login() -> objc2::rc::Retained<SMAppService> {
    unsafe { SMAppService::mainAppService() }
}

/// Public LaunchServices lookup, not a running-process test. Reading bundle
/// metadata does not launch/load its executable or imply account entitlement.
fn installed_app(bundle_id: &str, fallback_name: &str) -> Option<(PathBuf, String)> {
    let url = NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
    let path = PathBuf::from(url.path()?.to_string());
    if !path.is_absolute() || !path.is_dir() {
        return None; // LaunchServices can retain an entry after an app is removed.
    }
    let bundle = NSBundle::bundleWithURL(&url)?;
    let name = ["CFBundleDisplayName", "CFBundleName"]
        .into_iter()
        .find_map(|key| {
            bundle
                .objectForInfoDictionaryKey(&NSString::from_str(key))?
                .downcast::<NSString>()
                .ok()
                .map(|name| name.to_string())
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| fallback_name.to_owned());
    Some((path, name))
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
            if self.runtime.as_ref().is_none_or(Runtime::is_finished)
                && self
                    .plugin_task
                    .as_ref()
                    .is_none_or(|task| task.thread.is_finished())
            {
                if let Some(task) = self.plugin_task.take() {
                    let _ = task.thread.join();
                }
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
        // Status/config polling is independent of the native display clock.
        // Image-caused events neither redraw nor slide this deadline forward.
        if Instant::now() < self.next_refresh {
            return;
        }
        self.finish_plugin_task();
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
                self.rebuild_workspaces();
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
        self.update_plugin_items();
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
        if let Some(crown) = &self.crown {
            crown.update_activity(self.activity);
        }
        self.next_refresh = Instant::now() + Duration::from_millis(250);
    }

    fn quit(&mut self) {
        self.quitting.get_or_insert_with(Instant::now);
        self.crown.take(); // Invalidate display callbacks before service teardown.
        if let Some(task) = &self.plugin_task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        self.items.chatgpt.set_enabled(false);
        self.items.claude.set_enabled(false);
        for item in [
            &self.items.openai_manual,
            &self.items.cowork,
            &self.items.refresh_plugins,
            &self.items.copy_id,
            &self.items.quit,
        ] {
            item.set_enabled(false);
        }
        self.items.login.set_enabled(false);
        self.items.preview.set_enabled(false);
        self.items.piles.set_enabled(false);
        self.items.workspaces.set_enabled(false);
        self.items.integrations.set_enabled(false);
        if let Some(runtime) = &self.runtime {
            runtime.begin_shutdown();
        }
    }

    /// AppKit can terminate without returning from run_app. Winit delivers
    /// LoopExiting synchronously from applicationWillTerminate, so drain here
    /// before returning to AppKit, with the same final process-exit deadline.
    fn finish_termination(&mut self) {
        self.crown.take();
        if let Some(task) = &self.plugin_task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        // AppKit may exit without going through quit(). Stop admission before
        // waiting for any child cleanup, even if that cleanup cannot finish.
        if let Some(runtime) = &self.runtime {
            runtime.begin_shutdown();
        }
        let since = self.quitting.unwrap_or_else(Instant::now);
        while self
            .plugin_task
            .as_ref()
            .is_some_and(|task| !task.thread.is_finished())
        {
            if since.elapsed() >= Duration::from_secs(15) {
                std::process::exit(1);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if let Some(task) = self.plugin_task.take() {
            let _ = task.thread.join();
        }
        if let Some(mut runtime) = self.runtime.take() {
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

    fn start_plugin_task(
        &mut self,
        work: impl FnOnce(Arc<AtomicBool>) -> PluginResult + Send + 'static,
    ) {
        if self.plugin_task.is_some() || self.quitting.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        match std::thread::Builder::new()
            .name("plugin-lifecycle".into())
            .spawn(move || work(worker_cancel))
        {
            Ok(thread) => self.plugin_task = Some(PluginTask { cancel, thread }),
            Err(error) => {
                self.alert("Cannot start plugin operation", &error.to_string(), &["OK"]);
            }
        }
        self.update_plugin_items();
    }

    fn check_plugins(&mut self) {
        if self.plugin_task.is_some() || self.quitting.is_some() {
            return;
        }
        // The current OpenAI app is branded ChatGPT but retains this bundle ID.
        // Use its public display name, not its path or ID, for the menu label.
        let openai = installed_app("com.openai.codex", "OpenAI");
        let openai_apps = openai
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let mut clients = install::ClientAvailability {
            openai_app: openai.map(|(_, name)| name),
            claude_desktop: installed_app("com.anthropic.claudefordesktop", "Claude").is_some(),
            ..Default::default()
        };
        let layout = self.layout.clone();
        self.start_plugin_task(move |cancel| {
            clients.openai_cli =
                install::find_client_with_apps(install::Host::OpenAi, &openai_apps);
            clients.claude_cli = install::find_client_with_apps(install::Host::ClaudeCode, &[]);
            let states = [install::Host::OpenAi, install::Host::ClaudeCode].map(|host| {
                clients
                    .cli(host)
                    .map(|program| install::inventory_with_client(host, program, &layout, &cancel))
            });
            PluginResult::Inventory { clients, states }
        });
        if self.plugin_task.is_none() {
            self.rebuild_integrations();
        }
    }

    fn rebuild_integrations(&self) {
        let menu = &self.items.integrations;
        while menu.remove_at(0).is_some() {}
        match &self.clients {
            Some(clients) => {
                if clients.openai_label().is_some() {
                    if clients.openai_cli.is_some() {
                        let _ = menu.append(&self.items.chatgpt);
                    } else {
                        let _ = menu.append(&self.items.openai_manual);
                    }
                }
                if clients.claude_cli.is_some() {
                    let _ = menu.append(&self.items.claude);
                }
                if clients.claude_desktop {
                    let _ = menu.append(&self.items.cowork);
                }
                if !clients.any() {
                    self.items
                        .no_clients
                        .set_text("No supported clients detected");
                    let _ = menu.append(&self.items.no_clients);
                }
            }
            None => {
                self.items
                    .no_clients
                    .set_text(if self.plugin_task.is_some() {
                        "Checking clients…"
                    } else {
                        "Client detection unavailable"
                    });
                let _ = menu.append(&self.items.no_clients);
            }
        }
        let _ = menu.append_items(&[
            &PredefinedMenuItem::separator(),
            &self.items.refresh_plugins,
        ]);
    }

    fn update_plugin_items(&self) {
        for (host, item) in [
            (install::Host::OpenAi, &self.items.chatgpt),
            (install::Host::ClaudeCode, &self.items.claude),
        ] {
            let state = self.plugin_states[host_index(host)].as_ref();
            let (checked, actionable, suffix) = install::inventory_menu_state(state);
            if let Some(checked) = checked {
                item.set_checked(checked);
            }
            // On an error retain the last-known check, disable the control and
            // explicitly mark uncertainty instead of claiming uninstallation.
            let label = (host == install::Host::OpenAi)
                .then(|| {
                    self.clients
                        .as_ref()
                        .and_then(|clients| clients.openai_label())
                })
                .flatten()
                .unwrap_or_else(|| host.label().into());
            item.set_text(format!("{label} · {suffix}"));
            item.set_enabled(self.plugin_task.is_none() && actionable);
        }
        if let Some(label) = self
            .clients
            .as_ref()
            .and_then(|clients| clients.openai_label())
        {
            self.items
                .openai_manual
                .set_text(format!("{label} · CLI unavailable…"));
        }
        self.items
            .openai_manual
            .set_enabled(self.plugin_task.is_none());
        self.items
            .refresh_plugins
            .set_text(if self.plugin_task.is_some() {
                "Checking / updating plugins…"
            } else {
                "Refresh plugin status…"
            });
        self.items
            .refresh_plugins
            .set_enabled(self.plugin_task.is_none());
        self.items.cowork.set_enabled(self.plugin_task.is_none());
    }

    fn finish_plugin_task(&mut self) {
        if !self
            .plugin_task
            .as_ref()
            .is_some_and(|task| task.thread.is_finished())
        {
            return;
        }
        let Some(task) = self.plugin_task.take() else {
            return;
        };
        match task.thread.join() {
            Ok(PluginResult::Inventory { clients, states }) => {
                self.clients = Some(clients);
                self.plugin_states = states;
                self.rebuild_integrations();
            }
            Ok(PluginResult::Changed(host, result)) => {
                let text = match &result {
                    Ok(state) if state.installed => {
                        "The local client reports the plugin installed. Its enabled state is shown in the menu. Restart/reload the client to use the current package; installation is not proof of an active MCP connection."
                    }
                    Ok(_) => {
                        "The local client confirms the plugin was removed. Other plugins, marketplace registration and unrelated settings were left alone. A separate Claude account installation, if any, is unchanged."
                    }
                    Err(error) => error,
                };
                self.alert(host.label(), text, &["OK"]);
                self.plugin_states[host_index(host)] = Some(result);
            }
            Ok(PluginResult::Archive(result)) => match result {
                Ok(path) => {
                    if self.alert(
                        "Manage Cowork plugin",
                        &format!("{}\n\nArchive: {}", install::COWORK_HELP, path.display()),
                        &["Open account plugins", "Cancel"],
                    ) == 0
                    {
                        let workspace = NSWorkspace::sharedWorkspace();
                        if let Some(url) =
                            NSURL::URLWithString(&NSString::from_str(install::COWORK_URL))
                        {
                            workspace.openURL(&url);
                        }
                        workspace.selectFile_inFileViewerRootedAtPath(
                            Some(&NSString::from_str(&path.to_string_lossy())),
                            &NSString::from_str(""),
                        );
                    }
                }
                Err(error) => {
                    self.alert("Cowork package unavailable", &error, &["OK"]);
                }
            },
            Err(_) => {
                self.plugin_states = [
                    Some(Err("Plugin worker failed".into())),
                    Some(Err("Plugin worker failed".into())),
                ];
                self.rebuild_integrations();
            }
        }
        self.update_plugin_items();
    }

    fn plugin_action(&mut self, host: install::Host) {
        if self.plugin_task.is_some() || self.quitting.is_some() {
            return;
        }
        // AppKit toggles a check item immediately; restore observed state until
        // the client has actually confirmed the requested mutation.
        self.update_plugin_items();
        let Some(Ok(state)) = self.plugin_states[host_index(host)].clone() else {
            return;
        };
        let Some(program) = self
            .clients
            .as_ref()
            .and_then(|clients| clients.cli(host))
            .map(PathBuf::from)
        else {
            return;
        };
        if state.account_managed && !state.installed {
            self.manage_cowork();
            return;
        }
        let scope = match host {
            install::Host::OpenAi => {
                "This is the local OpenAI package for supported ChatGPT Work/Codex clients on this Mac. Status is the installed Codex CLI's inventory, not proof of ChatGPT UI refresh."
            }
            install::Host::ClaudeCode => {
                "This changes only this Mac's user-scope Claude Code plugin. It does not install or remove a Cowork account plugin."
            }
        };
        let text = format!(
            "{scope}\n\nKeep Erlkönig open while using its tools. After moving/updating the app, Refresh package reinstalls the current executable path; enable/reload it in the client if needed.\n\n{}",
            install::legacy_config_notice(host)
                .unwrap_or("No credentials are copied to the plugin or client settings.")
        );
        let wanted = if state.installed {
            match self.alert(
                host.label(),
                &text,
                &["Remove plugin", "Refresh package", "Cancel"],
            ) {
                0 => false,
                1 => true,
                _ => return,
            }
        } else {
            if self.alert(host.label(), &text, &["Install plugin", "Cancel"]) != 0 {
                return;
            }
            true
        };
        let url = self.runtime.as_ref().and_then(|r| r.status().mcp_url);
        if wanted && url.is_none() {
            self.alert(
                "Erlkönig is not running",
                "Start the app's services before installing the plugin.",
                &["OK"],
            );
            return;
        }
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                self.alert("Plugin operation unavailable", &error.to_string(), &["OK"]);
                return;
            }
        };
        let layout = self.layout.clone();
        self.start_plugin_task(move |cancel| {
            PluginResult::Changed(
                host,
                install::set_installed_with_client(
                    host,
                    &program,
                    wanted,
                    &layout,
                    &executable,
                    url.as_deref().unwrap_or(""),
                    &cancel,
                ),
            )
        });
    }

    fn manage_cowork(&mut self) {
        let Some(url) = self.runtime.as_ref().and_then(|r| r.status().mcp_url) else {
            if self.alert(
                "Manage Cowork plugin",
                install::COWORK_HELP,
                &["Open account plugins", "Cancel"],
            ) == 0
            {
                if let Some(url) = NSURL::URLWithString(&NSString::from_str(install::COWORK_URL)) {
                    NSWorkspace::sharedWorkspace().openURL(&url);
                }
            }
            return;
        };
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(_) => return,
        };
        let layout = self.layout.clone();
        self.start_plugin_task(move |cancel| {
            PluginResult::Archive(install::cowork_archive(&layout, &executable, &url, &cancel))
        });
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
                workspace: MenuItem::new("Create query workspace…", true, None),
            };
            let _ = submenu.append_items(&[
                &items.sync,
                &items.maintenance,
                &items.edit,
                &items.details,
                &items.workspace,
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
        alert.setInformativeText(&NSString::from_str("Enter exact IDs and collection handles, separated by spaces or commas. Adding a pile leaves Sync and Local maintenance off. Query workspaces are configured separately; each MCP data request must name an exact workspace ID."));
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
            if action == "workspace" {
                let discovery = Discovery::open(&pile).map_err(|e| format!("{e:#}"))?;
                let mut workspace = Workspace::new(&pile.label, &pile, discovery.signer());
                if self.edit_workspace(&mut workspace, &discovery)? {
                    runtime.put_workspace(&expected, &workspace).map_err(|e| format!("{e:#}"))?;
                }
                return Ok(());
            }
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

    fn rebuild_workspaces(&mut self) {
        while self.items.workspaces.remove_at(0).is_some() {}
        self.workspace_items.clear();
        let _ = self.items.workspaces.append(&MenuItem::new(
            "Each tool request chooses an exact workspace", false, None,
        ));
        let _ = self.items.workspaces.append(&PredefinedMenuItem::separator());
        let result = (|| {
            let runtime = self.runtime.as_ref().ok_or("Not running".to_owned())?;
            let config = runtime.config().map_err(|e| format!("{e:#}"))?;
            for id in config.workspace_ids().map_err(|e| format!("{e:#}"))? {
                let exact_id = format!("{id:X}");
                let short_id = &exact_id[..8];
                let (label, editable) = match config.workspace_revision(id) {
                    Ok(Some(workspace)) => {
                        let available = matches!(config.workspace(id), Ok(Some(_)));
                        (format!("{}{}", workspace.label, if available { "" } else { " · binding unavailable" }), true)
                    }
                    result => {
                        let reason = match result {
                            Err(error) => format!("{error:#}"),
                            _ => "enrollment changed or removed".to_owned(),
                        };
                        (format!("Unavailable: {reason}"), false)
                    }
                };
                let submenu = Submenu::new(format!("{label} · {short_id}"), true);
                let items = WorkspaceItems {
                    id: format!("{id:X}"),
                    edit: MenuItem::new("Discover and edit roots…", editable, None),
                    details: MenuItem::new("Exact binding and roots…", editable, None),
                    copy_id: MenuItem::new("Copy exact workspace ID", true, None),
                    remove: MenuItem::new("Remove workspace revision…", true, None),
                };
                let _ = submenu.append_items(&[&items.edit, &items.details, &items.copy_id, &items.remove]);
                let _ = self.items.workspaces.append(&submenu);
                self.workspace_items.push(items);
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            let _ = self.items.workspaces.append(&MenuItem::new(format!("Unavailable: {error}"), false, None));
        }
        let _ = self.items.workspaces.append(&PredefinedMenuItem::separator());
        let _ = self.items.workspaces.append(&MenuItem::new(
            "Create from Managed Piles → a pile → Create query workspace…", false, None,
        ));
    }

    /// One native popup; its values are display labels, its index selects the
    /// exact object held by this dialog. Names never resolve root identity.
    fn choose_index(&self, title: &str, detail: &str, choices: &[String], initial: usize, buttons: &[&str]) -> (usize, Option<usize>) {
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(detail));
        for button in buttons {
            alert.addButtonWithTitle(&NSString::from_str(button));
        }
        let popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(780.0, 32.0)), false,
        );
        for choice in choices {
            popup.addItemWithTitle(&NSString::from_str(choice));
        }
        if initial < choices.len() {
            popup.selectItemAtIndex(initial as isize);
        }
        alert.setAccessoryView(Some(&popup));
        let button = (alert.runModal() - NSAlertFirstButtonReturn).max(0) as usize;
        let index = usize::try_from(popup.indexOfSelectedItem()).ok();
        (button, index)
    }

    fn text_input(&self, title: &str, detail: &str, initial: &str) -> Option<String> {
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(detail));
        alert.addButtonWithTitle(&NSString::from_str("Use exact handle"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let field = NSTextField::textFieldWithString(&NSString::from_str(initial), self.mtm);
        field.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(780.0, 26.0)));
        alert.setAccessoryView(Some(&field));
        (alert.runModal() == NSAlertFirstButtonReturn).then(|| field.stringValue().to_string())
    }

    fn edit_workspace(&self, workspace: &mut Workspace, discovery: &Discovery) -> Result<bool, String> {
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
        let (roots, unavailable) = discovery.roots().map_err(|e| format!("{e:#}"))?;
        let roles = workspace::roles();
        loop {
            let selected = roles.iter().filter_map(|(role, name)| {
                workspace.routes.get(role).map(|root| {
                    let access = discovery.probe(*root).map(|root| format!("READ {} / WRITE {}", root.read, root.write))
                        .unwrap_or_else(|_| "unavailable locally".into());
                    format!("{name} ({access})")
                })
            }).collect::<Vec<_>>().join(", ");
            let detail = format!(
                "Pile: {}\nSigning identity: {}\n\n{}\n\nOmitted roles stay unconfigured. Saving creates a new exact request ID; previous IDs stop working. Sync and maintenance are not changed.\n\nDiscovery lists local indexed collection records only ({} unavailable). Descriptor-only roots require an exact handle.",
                workspace.pile.display(), hex::encode(workspace.signer),
                if selected.is_empty() { "No roles selected." } else { &selected }, unavailable.len(),
            );
            let alert = NSAlert::new(self.mtm);
            alert.setMessageText(&NSString::from_str("Query workspace"));
            alert.setInformativeText(&NSString::from_str(&detail));
            for title in ["Save workspace", "Choose a role…", "Cancel"] {
                alert.addButtonWithTitle(&NSString::from_str(title));
            }
            let name = NSTextField::textFieldWithString(&NSString::from_str(&workspace.label), self.mtm);
            name.setPlaceholderString(Some(&NSString::from_str("Workspace name")));
            name.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(780.0, 26.0)));
            alert.setAccessoryView(Some(&name));
            let action = alert.runModal() - NSAlertFirstButtonReturn;
            workspace.label = name.stringValue().to_string();
            match action {
                0 => {
                    if workspace.label.trim().is_empty() {
                        self.alert("Name required", "Give this workspace a name before saving.", &["OK"]);
                        continue;
                    }
                    for (role, root) in &workspace.routes {
                        discovery.select(*role, *root).map_err(|e| format!("{e:#}"))?;
                    }
                    return Ok(true);
                }
                1 => {}
                _ => return Ok(false),
            }
            let role_names = roles.iter().map(|(role, name)| format!("{name} · {}", if workspace.routes.contains_key(role) { "selected" } else { "unconfigured" })).collect::<Vec<_>>();
            let (button, index) = self.choose_index("Choose faculty role", "Choose which role to configure. Nothing is saved until Save workspace.", &role_names, 0, &["Choose root…", "Cancel"]);
            let Some((role, role_name)) = index.filter(|_| button == 0).and_then(|i| roles.get(i)) else { continue; };
            let mut candidates = roots.iter().filter(|root| root.name == *role_name).cloned().collect::<Vec<_>>();
            if let Some(root) = workspace.routes.get(role) {
                if !candidates.iter().any(|candidate| candidate.handle == *root) {
                    if let Ok(candidate) = discovery.probe(*root) {
                        candidates.push(candidate); // Previously selected exact descriptor-only root.
                    }
                }
            }
            let initial = workspace.routes.get(role).and_then(|root| candidates.iter().position(|candidate| candidate.handle == *root)).map_or(0, |index| index + 1);
            let mut choices = vec!["Unconfigured — do not route this role".to_owned()];
            choices.extend(candidates.iter().map(|root| root.description()));
            let (button, index) = self.choose_index(&format!("Exact root for {role_name}"),
                "Same-name roots are different identities. READ and WRITE show this workspace signer's local admission. Exact handle also permits locally present descriptor-only roots, which discovery cannot enumerate.",
                &choices, initial, &["Select", "Enter exact handle…", "Cancel"]);
            let handle = match button {
                0 => match index {
                    Some(0) => { workspace.routes.remove(role); continue; }
                    Some(index) => candidates.get(index - 1).map(|root| Ok(root.handle)),
                    None => None,
                },
                1 => self.text_input("Exact collection handle", "Paste the complete blake3: descriptor handle. It must be present locally and carry this role's name; no download or fallback will occur.", "")
                    .map(|text| config::parse_handle(text.trim()).map_err(|e| e.to_string())),
                _ => None,
            };
            let Some(handle) = handle else { continue; };
            let selection = handle.and_then(|handle| discovery.select(*role, handle).map_err(|e| format!("{e:#}")));
            match selection {
                Ok(root) => { workspace.routes.insert(*role, root.handle); }
                Err(error) => { self.alert("Root not selected", &error, &["OK"]); }
            }
        }
    }

    fn workspace_action(&self, selected: &str, action: &str) {
        let Some(runtime) = &self.runtime else { return; };
        let result = (|| {
            let expected = runtime.config().map_err(|e| format!("{e:#}"))?;
            let id = expected.workspace_ids().map_err(|e| format!("{e:#}"))?.into_iter()
                .find(|id| format!("{id:X}") == selected).ok_or("This workspace revision is no longer available.")?;
            // Copy/withdraw need only the selected native revision. A malformed
            // binding must not make its own removal inaccessible.
            if action == "copy" {
                let pasteboard = NSPasteboard::generalPasteboard();
                pasteboard.clearContents();
                if !unsafe { pasteboard.setString_forType(&NSString::from_str(selected), NSPasteboardTypeString) } {
                    return Err("The clipboard could not be updated.".into());
                }
                return Ok(());
            }
            if action == "remove" {
                if self.alert("Remove query workspace revision?", &format!("Withdraw {id:X}? Its exact request ID will stop working. Other concurrent revisions remain separately visible. No data, enrollment, sync or maintenance configuration is deleted."), &["Remove", "Cancel"]) == 0 {
                    runtime.remove_workspace_revision(&expected, id).map_err(|e| format!("{e:#}"))?;
                }
                return Ok(());
            }
            let mut workspace = expected.workspace_revision(id).map_err(|e| format!("{e:#}"))?
                .ok_or("This workspace revision is no longer available.")?;
            match action {
                "details" => {
                    let roles = workspace::roles();
                    let routes = workspace.routes.iter().map(|(role, root)| format!("{}: blake3:{}", roles.iter().find(|(id, _)| id == role).map(|(_, name)| *name).unwrap_or("unknown role"), hex::encode(root.raw))).collect::<Vec<_>>().join("\n");
                    self.alert("Exact workspace binding", &format!("{}\nWorkspace ID: {id:X}\nManaged pile ID: {:X}\nPile: {}\nKey file: {}\nPublic signer: {}\n\n{}\n\nThese are query routes, not sync or maintenance selections.", workspace.label, workspace.managed_id, workspace.pile.display(), workspace.key.display(), hex::encode(workspace.signer), routes), &["OK"]);
                }
                "edit" => {
                    let managed = expected.managed(workspace.managed_id).map_err(|e| format!("{e:#}"))?.ok_or("Managed pile no longer enrolled.")?;
                    if workspace.pile != managed.pile || workspace.key != managed.key {
                        if self.alert("Managed binding changed", &format!("The enrolled pile now uses:\nPile: {}\nKey: {}\n\nCreate a new workspace revision for these paths? The previous request ID will not be reused.", managed.pile.display(), managed.key.display()), &["Use new binding", "Cancel"]) != 0 { return Ok(()); }
                        workspace.pile = managed.pile.clone();
                        workspace.key = managed.key.clone();
                    }
                    let discovery = Discovery::open(&managed).map_err(|e| format!("{e:#}"))?;
                    if discovery.signer() != workspace.signer {
                        if self.alert("Signing identity changed", &format!("The selected key file now names {} instead of {}. Create a new workspace revision for this identity? The old revision cannot use the replacement key.", hex::encode(discovery.signer()), hex::encode(workspace.signer)), &["Use new identity", "Cancel"]) != 0 { return Ok(()); }
                        workspace.signer = discovery.signer();
                    }
                    if self.edit_workspace(&mut workspace, &discovery)? {
                        runtime.put_workspace(&expected, &workspace).map_err(|e| format!("{e:#}"))?;
                    }
                }
                _ => {}
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result { self.alert("Workspace unavailable", &error, &["OK"]); }
    }

    fn on_menu(&mut self, event: MenuEvent, event_loop: &ActiveEventLoop) {
        let id = event.id();
        if id == self.items.quit.id() {
            self.quit();
        } else if id == self.items.preview.id() {
            let enabled = self.items.preview.is_checked();
            if let Some(crown) = &self.crown {
                crown.set_preview(enabled);
                self.items.preview.set_checked(crown.preview_enabled());
            }
            if enabled && NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion() {
                self.alert(
                    "Sync animation preview",
                    "Preview is on, but Reduce Motion keeps the crown still. Uncheck Preview sync animation to stop the preview. No sync or data transfer is started.",
                    &["OK"],
                );
            }
        } else if id == self.items.add_pile.id() {
            self.add_pile();
        } else if let Some((entry, action)) = self.pile_items.iter().find_map(|p| {
            [
                (p.sync.id(), "sync"),
                (p.maintenance.id(), "maintenance"),
                (p.edit.id(), "edit"),
                (p.details.id(), "details"),
                (p.remove.id(), "remove"),
                (p.workspace.id(), "workspace"),
            ]
            .into_iter()
            .find(|(item, _)| id == *item)
            .map(|(_, action)| (p.id.clone(), action))
        }) {
            self.pile_action(&entry, action);
        } else if let Some((entry, action)) = self.workspace_items.iter().find_map(|workspace| {
            [(workspace.edit.id(), "edit"), (workspace.details.id(), "details"),
                (workspace.copy_id.id(), "copy"), (workspace.remove.id(), "remove")]
                .into_iter().find(|(item, _)| id == *item)
                .map(|(_, action)| (workspace.id.clone(), action))
        }) {
            self.workspace_action(&entry, action);
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
            self.plugin_action(install::Host::OpenAi);
        } else if id == self.items.openai_manual.id() {
            self.alert(
                "OpenAI plugin management unavailable",
                "The desktop app is installed, but a supported Codex management CLI was not detected. Use the app's plugin settings, or install the Codex CLI and choose Refresh plugin status. Erlkönig has not checked or changed the app's plugin installation.",
                &["OK"],
            );
        } else if id == self.items.claude.id() {
            self.plugin_action(install::Host::ClaudeCode);
        } else if id == self.items.cowork.id() {
            self.manage_cowork();
        } else if id == self.items.refresh_plugins.id() {
            self.check_plugins();
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
        self.next_refresh = Instant::now();
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
                    if let Some(button) =
                        tray.ns_status_item().and_then(|item| item.button(self.mtm))
                    {
                        self.crown = Some(DisplayCrown::new(button, self.origin, self.mtm));
                    } else {
                        self.problem = Some("Cannot create crown display".into());
                        self.items.preview.set_enabled(false);
                    }
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
            self.check_plugins();
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
        preview: CheckMenuItem::new("Preview sync animation", true, false, None),
        copy_id: MenuItem::new("Copy my ID", true, None),
        integrations: Submenu::new("Integrations", true),
        no_clients: MenuItem::new("Checking clients…", false, None),
        chatgpt: CheckMenuItem::new("OpenAI local plugin · checking…", false, false, None),
        openai_manual: MenuItem::new("OpenAI plugin · CLI unavailable…", false, None),
        claude: CheckMenuItem::new("Claude Code plugin · checking…", false, false, None),
        cowork: MenuItem::new("Manage Cowork plugin…", false, None),
        refresh_plugins: MenuItem::new("Refresh plugin status…", false, None),
        login: CheckMenuItem::new("Open at login", true, false, None),
        piles: Submenu::new("Managed Piles", true),
        add_pile: MenuItem::new("Add existing pile…", true, None),
        workspaces: Submenu::new("Query Workspaces", true),
        quit: MenuItem::new("Quit Erlkönig", true, None),
    };
    items
        .integrations
        .append_items(&[&items.no_clients, &items.refresh_plugins])
        .expect("integrations menu");
    let menu = Menu::new();
    menu.append_items(&[
        &items.status,
        &items.preview,
        &items.piles,
        &items.workspaces,
        &PredefinedMenuItem::separator(),
        &items.copy_id,
        &items.integrations,
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
        crown: None,
        transfer_bytes: 0,
        next_refresh: Instant::now(),
        quitting: None,
        terminate,
        pile_items: Vec::new(),
        workspace_items: Vec::new(),
        config_generation: u64::MAX,
        plugin_states: [None, None],
        clients: None,
        plugin_task: None,
    };
    if let Err(error) = event_loop.run_app(&mut app) {
        eprintln!("Erlkönig: {error}");
    }
    // Idempotent fallback for event-loop construction/execution errors.
    app.finish_termination();
}
