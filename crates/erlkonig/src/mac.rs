//! Main-thread menu and the one application-owned native lifetime.

use crate::crown::{self, TransferActivity};
use crate::crown_display::DisplayCrown;
use erlkonig_core::{
    install,
    paths::{self, Layout},
    runtime::Runtime,
    workspace::{self, Discovery, RootChoice, Workspace},
};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSButton, NSControlStateValueOn,
    NSPasteboard, NSPasteboardTypeString, NSPopUpButton, NSScrollView, NSTextField, NSTextView,
    NSView, NSWorkspace,
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
    settings: MenuItem,
    advanced: Submenu,
    workspaces: Submenu,
    add_workspace: MenuItem,
    quit: MenuItem,
}

struct WorkspaceItems {
    id: String,
    usage: MenuItem,
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

fn access_label(root: &RootChoice) -> &'static str {
    match (root.read, root.write) {
        (true, true) => "read and write",
        (true, false) => "read only",
        (false, true) => "write only",
        (false, false) => "access needed",
    }
}

/// Ordinals distinguish equal display names without pretending the names are
/// identities. They belong only to this frozen menu/dialog, never to storage.
fn distinguish_names(names: &[String]) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let total = names.iter().filter(|other| *other == name).count();
            if total > 1 {
                let ordinal = names[..=index]
                    .iter()
                    .filter(|other| *other == name)
                    .count();
                format!("{name} · choice {ordinal} of {total}")
            } else {
                name.clone()
            }
        })
        .collect()
}

fn collection_labels(roots: &[RootChoice]) -> Vec<String> {
    distinguish_names(
        &roots
            .iter()
            .map(|root| root.name.clone())
            .collect::<Vec<_>>(),
    )
    .into_iter()
    .zip(roots)
    .map(|(name, root)| format!("{name} · {}", access_label(root)))
    .collect()
}

fn sole_named_choice(roots: &[RootChoice], name: &str) -> Option<usize> {
    let mut candidates = roots
        .iter()
        .enumerate()
        .filter(|(_, root)| root.name == name && (root.read || root.write));
    let first = candidates.next()?.0;
    candidates.next().is_none().then_some(first)
}

#[derive(Debug, PartialEq, Eq)]
enum CollectionSelection {
    Omit,
    Choose(usize),
    PreserveUnavailable,
}

fn collection_selection(index: isize, available: usize) -> CollectionSelection {
    match usize::try_from(index).ok() {
        Some(0) => CollectionSelection::Omit,
        Some(index) if index <= available => CollectionSelection::Choose(index - 1),
        _ => CollectionSelection::PreserveUnavailable,
    }
}

fn common_role(name: &str) -> bool {
    matches!(
        name,
        "wiki" | "compass" | "files" | "memory-journal" | "relations" | "message"
    )
}

fn role_label(name: &str) -> String {
    match name {
        "wiki" => "Notes & knowledge".into(),
        "compass" => "Goals & tasks".into(),
        "files" => "Files & documents".into(),
        "memory-journal" => "Journal".into(),
        "memory-comb" => "Journal context".into(),
        "relations" => "People & groups".into(),
        "message" => "Messages".into(),
        "config" => "Tool settings".into(),
        "decide" => "Decisions".into(),
        "secrets" => "Tool credentials".into(),
        "teams" => "Teams & access".into(),
        other => {
            let label = other.replace('-', " ");
            let mut chars = label.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => label,
            }
        }
    }
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
                self.rebuild_workspaces();
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
        self.items.advanced.set_enabled(false);
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

    fn settings(&self) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        let result = (|| {
            let expected = runtime.config().map_err(|e| format!("{e:#}"))?;
            let mut settings = expected.settings().map_err(|e| format!("{e:#}"))?;
            let binding = runtime.binding();
            #[allow(deprecated)]
            NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
            let alert = NSAlert::new(self.mtm);
            alert.setMessageText(&NSString::from_str("Erlkönig Settings"));
            alert.setInformativeText(&NSString::from_str(
                "One identity, one local store, every workspace.\n\nSync includes all collections this identity is permitted to share. Local maintenance keeps supported attached collections ready. Workspace choices never change your identity, storage or these settings."
            ));
            alert.addButtonWithTitle(&NSString::from_str("Save"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            let view = NSView::initWithFrame(
                NSView::alloc(self.mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 236.0)),
            );
            for (y, label, value) in [
                (210.0, "Local store", binding.pile.display().to_string()),
                (
                    158.0,
                    "Identity key — configured once",
                    binding.key.display().to_string(),
                ),
            ] {
                let title = NSTextField::labelWithString(&NSString::from_str(label), self.mtm);
                title.setFrame(NSRect::new(NSPoint::new(0.0, y), NSSize::new(640.0, 20.0)));
                view.addSubview(&title);
                let field = NSTextField::textFieldWithString(&NSString::from_str(&value), self.mtm);
                field.setEditable(false);
                field.setFrame(NSRect::new(
                    NSPoint::new(0.0, y - 27.0),
                    NSSize::new(640.0, 24.0),
                ));
                view.addSubview(&field);
            }
            // No target/action: these controls edit only this unsaved dialog.
            let sync = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &NSString::from_str("Sync all permitted collections"),
                    None,
                    None,
                    self.mtm,
                )
            };
            sync.setState(if settings.sync_enabled {
                NSControlStateValueOn
            } else {
                0
            });
            sync.setFrame(NSRect::new(
                NSPoint::new(0.0, 80.0),
                NSSize::new(640.0, 24.0),
            ));
            view.addSubview(&sync);
            let maintenance = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &NSString::from_str("Maintain supported attached collections"),
                    None,
                    None,
                    self.mtm,
                )
            };
            maintenance.setState(if settings.maintenance_enabled {
                NSControlStateValueOn
            } else {
                0
            });
            maintenance.setFrame(NSRect::new(
                NSPoint::new(0.0, 46.0),
                NSSize::new(640.0, 24.0),
            ));
            view.addSubview(&maintenance);
            let note = NSTextField::labelWithString(
                &NSString::from_str(
                    "Identity and storage stay fixed for this app session. No keys are copied.",
                ),
                self.mtm,
            );
            note.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(640.0, 22.0)));
            view.addSubview(&note);
            alert.setAccessoryView(Some(&view));
            if alert.runModal() != NSAlertFirstButtonReturn {
                return Ok(());
            }
            settings.sync_enabled = sync.state() == NSControlStateValueOn;
            settings.maintenance_enabled = maintenance.state() == NSControlStateValueOn;
            runtime
                .put_settings(&expected, &settings)
                .map_err(|e| format!("{e:#}"))
        })();
        if let Err(error) = result {
            self.alert("Settings not saved", &error, &["OK"]);
        }
    }

    fn rebuild_workspaces(&mut self) {
        while self.items.workspaces.remove_at(0).is_some() {}
        self.workspace_items.clear();
        let _ = self.items.workspaces.append(&self.items.add_workspace);
        let _ = self.items.workspaces.append(&MenuItem::new(
            "Named collection bundles · one shared local store",
            false,
            None,
        ));
        let _ = self
            .items
            .workspaces
            .append(&PredefinedMenuItem::separator());
        let result = (|| {
            let runtime = self.runtime.as_ref().ok_or("Not running".to_owned())?;
            let config = runtime.config().map_err(|e| format!("{e:#}"))?;
            let ids = config.workspace_ids().map_err(|e| format!("{e:#}"))?;
            let entries = ids
                .into_iter()
                .map(|id| {
                    let (label, editable) = match config.workspace_revision(id) {
                        Ok(Some(workspace)) => (workspace.label, true),
                        _ => ("Unavailable workspace".to_owned(), false),
                    };
                    (id, label, editable)
                })
                .collect::<Vec<_>>();
            let labels = distinguish_names(
                &entries
                    .iter()
                    .map(|(_, label, _)| label.clone())
                    .collect::<Vec<_>>(),
            );
            for ((id, _, editable), label) in entries.into_iter().zip(labels) {
                let submenu = Submenu::new(label, true);
                let items = WorkspaceItems {
                    id: format!("{id:X}"),
                    usage: MenuItem::new("Use with your assistant…", editable, None),
                    edit: MenuItem::new("Edit collections…", editable, None),
                    details: MenuItem::new("Details and diagnostics…", true, None),
                    copy_id: MenuItem::new("Copy tool request ID (advanced)", true, None),
                    remove: MenuItem::new("Remove workspace…", true, None),
                };
                let _ = submenu.append_items(&[
                    &items.usage,
                    &items.edit,
                    &items.details,
                    &items.copy_id,
                    &items.remove,
                ]);
                let _ = self.items.workspaces.append(&submenu);
                self.workspace_items.push(items);
            }
            if self.workspace_items.is_empty() {
                let _ = self.items.workspaces.append(&MenuItem::new(
                    "Create a workspace to choose collections for your tools",
                    false,
                    None,
                ));
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            let _ = self.items.workspaces.append(&MenuItem::new(
                format!("Unavailable: {error}"),
                false,
                None,
            ));
        }
    }

    fn access_help(&self) {
        self.alert(
            "Need access to a collection?",
            "Ask the collection owner for an invitation or access for this Mac's existing identity. Read and write access are separate permissions.\n\nErlkönig does not yet import invitations or send access requests from this dialog. Have the owner or your administrator arrange access and make the collection available locally, then reopen the editor. Do not create another identity or paste a secret key into chat.",
            &["OK"],
        );
    }

    fn collection_diagnostics(&self, roots: &[RootChoice], unavailable: &[String]) {
        let labels = collection_labels(roots);
        let mut text = roots
            .iter()
            .zip(labels)
            .map(|(root, label)| {
                format!(
                    "{label}\nCollection: blake3:{}",
                    hex::encode(root.handle.raw)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if !unavailable.is_empty() {
            text.push_str("\n\nUnavailable locally:\n");
            text.push_str(&unavailable.join("\n"));
        }
        if text.is_empty() {
            text = "No named collections are available locally yet.".into();
        }
        self.diagnostics("Collection diagnostics", "Read-only collection details. These identifiers are for troubleshooting; normal workspace setup never requires typing them.", &text);
    }

    fn diagnostics(&self, title: &str, explanation: &str, text: &str) {
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(explanation));
        alert.addButtonWithTitle(&NSString::from_str("OK"));
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(self.mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(680.0, 360.0)),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        let content = NSTextView::initWithFrame(
            NSTextView::alloc(self.mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(656.0, 360.0)),
        );
        content.setEditable(false);
        content.setSelectable(true);
        content.setHorizontallyResizable(false);
        content.setVerticallyResizable(true);
        content.setMinSize(NSSize::new(656.0, 360.0));
        content.setMaxSize(NSSize::new(656.0, f64::MAX));
        content.setString(&NSString::from_str(text));
        content.sizeToFit();
        scroll.setDocumentView(Some(&content));
        alert.setAccessoryView(Some(&scroll));
        alert.runModal();
    }

    fn add_workspace(&self) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        let result = (|| {
            let expected = runtime.config().map_err(|e| format!("{e:#}"))?;
            #[allow(deprecated)]
            NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
            let mut label = String::new();
            let use_existing = loop {
                let alert = NSAlert::new(self.mtm);
                alert.setMessageText(&NSString::from_str("New workspace"));
                alert.setInformativeText(&NSString::from_str(
                    "Give your workspace a name. Create makes a private bundle for notes, goals, files, your journal and the other tools — ready to use.\n\nIt uses this Mac's existing identity and local store. Nothing is saved until Create. To organize collections you already have, choose Use existing collections instead."
                ));
                for title in ["Create", "Cancel", "Use existing collections…"] {
                    alert.addButtonWithTitle(&NSString::from_str(title));
                }
                let name = NSTextField::textFieldWithString(&NSString::from_str(&label), self.mtm);
                name.setPlaceholderString(Some(&NSString::from_str(
                    "For example: Personal, Research, or Team",
                )));
                name.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(560.0, 28.0)));
                alert.setAccessoryView(Some(&name));
                let action = alert.runModal() - NSAlertFirstButtonReturn;
                if action == 1 || !(0..=2).contains(&action) {
                    return Ok(());
                }
                label = name.stringValue().to_string().trim().to_owned();
                if action == 2 {
                    break true;
                }
                if label.is_empty() {
                    self.alert(
                        "Name required",
                        "Give this workspace a name before creating it.",
                        &["OK"],
                    );
                    continue;
                }
                runtime
                    .create_workspace(&expected, &label)
                    .map_err(|e| format!("{e:#}"))?;
                break false;
            };
            if !use_existing {
                return Ok(());
            }
            let discovery = Discovery::open(runtime.binding()).map_err(|e| format!("{e:#}"))?;
            let mut workspace = Workspace::new(label);
            let (roots, _) = discovery.roots().map_err(|e| format!("{e:#}"))?;
            for (role, name) in workspace::roles() {
                if let Some(index) = sole_named_choice(&roots, name) {
                    workspace.routes.insert(role, roots[index].handle);
                }
            }
            if self.edit_workspace(&mut workspace, &discovery)? {
                runtime
                    .put_workspace(&expected, &workspace)
                    .map_err(|e| format!("{e:#}"))?;
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            self.alert("Workspace not saved", &error, &["OK"]);
        }
    }

    /// Popup indices refer to the exact objects captured by this dialog.
    /// A label is presentation only; neither its text nor its position is saved.
    fn edit_workspace(
        &self,
        workspace: &mut Workspace,
        discovery: &Discovery,
    ) -> Result<bool, String> {
        #[allow(deprecated)]
        NSApplication::sharedApplication(self.mtm).activateIgnoringOtherApps(true);
        let (mut roots, unavailable) = discovery.roots().map_err(|e| format!("{e:#}"))?;
        // Retain already selected descriptor-only collections without requiring
        // a person to paste their handles. An unavailable selection is preserved
        // visibly below, never changed to "Not included" by opening the editor.
        for handle in workspace.routes.values() {
            if !roots.iter().any(|root| root.handle == *handle) {
                if let Ok(root) = discovery.probe(*handle) {
                    roots.push(root);
                }
            }
        }
        roots.sort_by(|a, b| a.name.cmp(&b.name).then(a.handle.cmp(&b.handle)));
        let labels = collection_labels(&roots);
        let roles = workspace::roles();
        let mut draft = workspace.clone();
        let mut advanced = false;
        loop {
            let visible_roles = roles
                .iter()
                .filter(|(_, name)| advanced || common_role(name))
                .collect::<Vec<_>>();
            let alert = NSAlert::new(self.mtm);
            alert.setMessageText(&NSString::from_str("Workspace"));
            let additional = roles
                .iter()
                .filter(|(role, name)| !common_role(name) && draft.routes.contains_key(role))
                .count();
            alert.setInformativeText(&NSString::from_str(&format!(
                "A workspace brings together collections for your assistant. Review the choices below; unused areas can stay Not included. All workspaces share this Mac's identity and local store.\n\n{} tool areas included; {} additional areas are under More collection settings. Nothing changes until Save. Numbered choices are different collections — ask the owner if you cannot identify one.",
                draft.routes.len(), additional,
            )));
            for title in [
                "Save",
                "Cancel",
                if advanced {
                    "Collection diagnostics…"
                } else {
                    "More collection settings…"
                },
                "Need access?",
            ] {
                alert.addButtonWithTitle(&NSString::from_str(title));
            }
            let view = NSView::initWithFrame(
                NSView::alloc(self.mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(680.0, 436.0)),
            );
            let name_label =
                NSTextField::labelWithString(&NSString::from_str("Workspace name"), self.mtm);
            name_label.setFrame(NSRect::new(
                NSPoint::new(0.0, 410.0),
                NSSize::new(680.0, 20.0),
            ));
            view.addSubview(&name_label);
            let name =
                NSTextField::textFieldWithString(&NSString::from_str(&draft.label), self.mtm);
            name.setPlaceholderString(Some(&NSString::from_str(
                "For example: Personal, Research, or Team",
            )));
            name.setFrame(NSRect::new(
                NSPoint::new(0.0, 380.0),
                NSSize::new(680.0, 26.0),
            ));
            view.addSubview(&name);
            let height = visible_roles.len() as f64 * 38.0;
            let document = NSView::initWithFrame(
                NSView::alloc(self.mtm),
                NSRect::new(NSPoint::ZERO, NSSize::new(656.0, height)),
            );
            let mut popups = Vec::new();
            for (i, (role, role_name)) in visible_roles.iter().copied().enumerate() {
                let y = height - (i + 1) as f64 * 38.0;
                let label = NSTextField::labelWithString(
                    &NSString::from_str(&role_label(role_name)),
                    self.mtm,
                );
                label.setFrame(NSRect::new(
                    NSPoint::new(0.0, y + 5.0),
                    NSSize::new(210.0, 24.0),
                ));
                document.addSubview(&label);
                let popup = NSPopUpButton::initWithFrame_pullsDown(
                    NSPopUpButton::alloc(self.mtm),
                    NSRect::new(NSPoint::new(214.0, y), NSSize::new(438.0, 32.0)),
                    false,
                );
                popup.addItemWithTitle(&NSString::from_str("Not included"));
                for label in &labels {
                    popup.addItemWithTitle(&NSString::from_str(label));
                }
                let current = draft.routes.get(role);
                let selected =
                    current.and_then(|handle| roots.iter().position(|root| root.handle == *handle));
                let index = if let Some(index) = selected {
                    index + 1
                } else if current.is_some() {
                    popup.addItemWithTitle(&NSString::from_str(
                        "Previously selected collection · unavailable",
                    ));
                    roots.len() + 1
                } else {
                    0
                };
                popup.selectItemAtIndex(index as isize);
                document.addSubview(&popup);
                popups.push(popup);
            }
            let scroll = NSScrollView::initWithFrame(
                NSScrollView::alloc(self.mtm),
                NSRect::new(NSPoint::new(0.0, 8.0), NSSize::new(680.0, 358.0)),
            );
            scroll.setHasVerticalScroller(true);
            scroll.setAutohidesScrollers(true);
            scroll.setDocumentView(Some(&document));
            // NSView's coordinates run upward: begin at the first tool area.
            document.scrollPoint(NSPoint::new(0.0, (height - 358.0).max(0.0)));
            view.addSubview(&scroll);
            alert.setAccessoryView(Some(&view));
            let action = alert.runModal() - NSAlertFirstButtonReturn;
            if action == 1 || !(0..=3).contains(&action) {
                return Ok(false);
            }
            draft.label = name.stringValue().to_string();
            for ((role, _), popup) in visible_roles.into_iter().zip(popups) {
                match collection_selection(popup.indexOfSelectedItem(), roots.len()) {
                    CollectionSelection::Omit => {
                        draft.routes.remove(role);
                    }
                    CollectionSelection::Choose(index) => {
                        draft.routes.insert(*role, roots[index].handle);
                    }
                    CollectionSelection::PreserveUnavailable => {}
                }
            }
            match action {
                2 => {
                    if advanced {
                        self.collection_diagnostics(&roots, &unavailable);
                    } else {
                        advanced = true;
                    }
                    continue;
                }
                3 => {
                    self.access_help();
                    continue;
                }
                _ => {}
            }
            draft.label = draft.label.trim().to_owned();
            if draft.label.is_empty() || draft.routes.is_empty() {
                self.alert("Workspace not ready", "Give the workspace a name and choose at least one collection. Unused tool areas can stay Not included.", &["OK"]);
                continue;
            }
            if let Some(error) = draft.routes.iter().find_map(|(role, handle)| {
                discovery
                    .select(*role, *handle)
                    .err()
                    .map(|e| format!("{e:#}"))
            }) {
                self.alert("Collection unavailable", &format!("{error}\n\nYour changes have not been saved. Choose another collection or use Need access? for help."), &["OK"]);
                continue;
            }
            *workspace = draft;
            return Ok(true);
        }
    }

    fn workspace_action(&self, selected: &str, action: &str) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        let result = (|| {
            let expected = runtime.config().map_err(|e| format!("{e:#}"))?;
            let id = expected
                .workspace_ids()
                .map_err(|e| format!("{e:#}"))?
                .into_iter()
                .find(|id| format!("{id:X}") == selected)
                .ok_or("This workspace has changed. Reopen the Workspaces menu.")?;
            if action == "copy" {
                let pasteboard = NSPasteboard::generalPasteboard();
                pasteboard.clearContents();
                if !unsafe {
                    pasteboard
                        .setString_forType(&NSString::from_str(selected), NSPasteboardTypeString)
                } {
                    return Err("The clipboard could not be updated.".into());
                }
                return Ok(());
            }
            if action == "remove" {
                if self.alert("Remove workspace?", "This removes the named bundle from your tools. Its collections and data remain in the shared local store, and sync and maintenance settings are unchanged.", &["Remove", "Cancel"]) == 0 {
                    runtime.remove_workspace_revision(&expected, id).map_err(|e| format!("{e:#}"))?;
                }
                return Ok(());
            }
            let mut workspace = expected
                .workspace_revision(id)
                .map_err(|e| format!("{e:#}"))?
                .ok_or("This workspace is no longer available.")?;
            if action == "usage" {
                self.alert("Use this workspace", &format!("Tell your assistant: Use the {} workspace.\n\nThe assistant lists available workspaces and selects this bundle on each tool request. You do not need to type or copy its internal ID. If names are ambiguous, it should ask which collection bundle you mean.\n\nThis does not switch other conversations or clients to this workspace.", workspace.label), &["OK"]);
                return Ok(());
            }
            let discovery = Discovery::open(runtime.binding()).map_err(|e| format!("{e:#}"))?;
            match action {
                "details" => {
                    let roles = workspace::roles();
                    let routes = workspace
                        .routes
                        .iter()
                        .map(|(role, root)| {
                            let role = roles
                                .iter()
                                .find(|(id, _)| id == role)
                                .map(|(_, name)| role_label(name))
                                .unwrap_or_else(|| "Unknown tool area".into());
                            let name = discovery
                                .probe(*root)
                                .map(|root| format!("{} · {}", root.name, access_label(&root)))
                                .unwrap_or_else(|_| "Unavailable locally".into());
                            format!(
                                "{role}: {name}\nCollection: blake3:{}",
                                hex::encode(root.raw)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    self.diagnostics(&format!("{} — details", workspace.label), "Tell your assistant which workspace to use; it selects the bundle separately on every request. There is no global switch affecting other clients. All workspaces use this Mac's one identity and store.", &format!("Tool request ID (advanced): {id:X}\n\n{routes}"));
                }
                "edit" => {
                    if self.edit_workspace(&mut workspace, &discovery)? {
                        runtime
                            .put_workspace(&expected, &workspace)
                            .map_err(|e| format!("{e:#}"))?;
                    }
                }
                _ => {}
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            self.alert("Workspace unavailable", &error, &["OK"]);
        }
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
        } else if id == self.items.settings.id() {
            self.settings();
        } else if id == self.items.add_workspace.id() {
            self.add_workspace();
        } else if let Some((entry, action)) = self.workspace_items.iter().find_map(|workspace| {
            [
                (workspace.usage.id(), "usage"),
                (workspace.edit.id(), "edit"),
                (workspace.details.id(), "details"),
                (workspace.copy_id.id(), "copy"),
                (workspace.remove.id(), "remove"),
            ]
            .into_iter()
            .find(|(item, _)| id == *item)
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
        copy_id: MenuItem::new("Copy this Mac's public ID", true, None),
        integrations: Submenu::new("Integrations", true),
        no_clients: MenuItem::new("Checking clients…", false, None),
        chatgpt: CheckMenuItem::new("OpenAI local plugin · checking…", false, false, None),
        openai_manual: MenuItem::new("OpenAI plugin · CLI unavailable…", false, None),
        claude: CheckMenuItem::new("Claude Code plugin · checking…", false, false, None),
        cowork: MenuItem::new("Manage Cowork plugin…", false, None),
        refresh_plugins: MenuItem::new("Refresh plugin status…", false, None),
        login: CheckMenuItem::new("Open at login", true, false, None),
        settings: MenuItem::new("Identity, storage and services…", true, None),
        advanced: Submenu::new("Advanced", true),
        workspaces: Submenu::new("Workspaces", true),
        add_workspace: MenuItem::new("New workspace…", true, None),
        quit: MenuItem::new("Quit Erlkönig", true, None),
    };
    items
        .integrations
        .append_items(&[&items.no_clients, &items.refresh_plugins])
        .expect("integrations menu");
    let menu = Menu::new();
    items
        .advanced
        .append_items(&[&items.settings, &items.copy_id])
        .expect("advanced menu");
    menu.append_items(&[
        &items.status,
        &items.preview,
        &items.workspaces,
        &PredefinedMenuItem::separator(),
        &items.integrations,
        &items.advanced,
        &PredefinedMenuItem::separator(),
        &items.login,
        &items.quit,
    ])
    .expect("menu");
    let mut app = App {
        mtm,
        layout: Layout::application(paths::default_data_dir().expect("data directory")).expect("node bootstrap location"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn root(byte: u8, name: &str, read: bool, write: bool) -> RootChoice {
        RootChoice {
            handle: erlkonig_core::config::parse_handle(&hex::encode([byte; 32])).unwrap(),
            name: name.into(),
            read,
            write,
        }
    }

    #[test]
    fn defaults_only_choose_a_sole_eligible_named_collection() {
        let roots = vec![root(1, "wiki", true, true), root(2, "wiki", true, false)];
        assert_eq!(sole_named_choice(&roots, "wiki"), None);
        assert_eq!(sole_named_choice(&roots, "files"), None);
        let roots = vec![root(1, "wiki", false, false), root(2, "wiki", true, false)];
        assert_eq!(sole_named_choice(&roots, "wiki"), Some(1));
        assert_eq!(sole_named_choice(&roots[..1], "wiki"), None);
    }

    #[test]
    fn same_name_choices_stay_distinct_without_exposing_handles() {
        let roots = vec![
            root(1, "Notes", true, true),
            root(2, "Notes", true, true),
            root(3, "Files", true, false),
        ];
        let labels = collection_labels(&roots);
        assert_ne!(labels[0], labels[1]);
        assert!(labels[0].contains("choice 1 of 2"));
        assert!(labels[1].contains("choice 2 of 2"));
        assert_eq!(labels[2], "Files · read only");
        for (root, label) in roots.iter().zip(labels) {
            assert!(!label.contains(&hex::encode(root.handle.raw)));
        }
    }

    #[test]
    fn unavailable_selection_is_never_silently_omitted() {
        assert_eq!(collection_selection(0, 2), CollectionSelection::Omit);
        assert_eq!(collection_selection(1, 2), CollectionSelection::Choose(0));
        assert_eq!(collection_selection(2, 2), CollectionSelection::Choose(1));
        assert_eq!(
            collection_selection(3, 2),
            CollectionSelection::PreserveUnavailable
        );
        assert_eq!(
            collection_selection(-1, 2),
            CollectionSelection::PreserveUnavailable
        );
    }

    #[test]
    fn read_and_write_permissions_remain_independent() {
        assert_eq!(
            access_label(&root(1, "Notes", true, true)),
            "read and write"
        );
        assert_eq!(access_label(&root(1, "Notes", true, false)), "read only");
        assert_eq!(access_label(&root(1, "Notes", false, true)), "write only");
        assert_eq!(
            access_label(&root(1, "Notes", false, false)),
            "access needed"
        );
    }
}
