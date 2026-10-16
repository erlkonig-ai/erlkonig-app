//! The platform-neutral application: menu state, the one application-owned
//! native lifetime, the plugin worker and the quit drain. Each platform owns
//! the event loop and implements [`Shell`] for what only it can do.

use crate::crown::{self, WorkActivity};
use erlkonig_core::{
    install,
    paths::{self, Layout},
    runtime::Runtime,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu,
};
use winit::event_loop::ActiveEventLoop;

/// Observed state of the platform's start-at-login registration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Login {
    Off,
    On,
    /// Registered but waiting for the person's approval (macOS only).
    NeedsApproval,
}

/// What only the platform can do. Everything else lives in [`App`].
pub trait Shell {
    /// A native modal answers with the pressed button's index. A shell that
    /// draws its prompts itself returns `None`: [`App`] then queues the prompt
    /// and the shell answers later through [`App::answer`].
    fn alert(&mut self, title: &str, text: &str, buttons: &[&str]) -> Option<usize>;
    fn open_url(&self, url: &str);
    /// Show `path` selected in the file manager.
    fn reveal(&self, path: &Path);
    /// Open `folder` in the file manager; the error is the person-facing text.
    fn show_folder(&self, folder: &Path) -> Result<(), &'static str>;
    fn login(&self) -> Login;
    fn set_login(&self, on: bool) -> Result<(), String>;
    fn open_login_settings(&self);
    /// Installed desktop apps: the OpenAI app's path and display name, and
    /// whether Claude Desktop is installed.
    fn detect_apps(&self) -> (Option<(PathBuf, String)>, bool);
    fn crown(&mut self, activity: Option<WorkActivity>, needs_attention: bool);
    /// Invalidate crown display callbacks before service teardown.
    fn stop_crown(&mut self);
    /// Ask for the status window to be shown (a no-op where none exists).
    fn show_window(&mut self);
}

pub struct Items {
    pub status: MenuItem,
    pub integrations: Submenu,
    pub no_clients: MenuItem,
    pub chatgpt: CheckMenuItem,
    pub openai_manual: MenuItem,
    pub claude: CheckMenuItem,
    pub cowork: MenuItem,
    pub refresh_plugins: MenuItem,
    pub login: CheckMenuItem,
    pub pause_sync: MenuItem,
    pub retry_services: MenuItem,
    pub storage_folder: MenuItem,
    pub quit: MenuItem,
}

/// What happens once the person has answered a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Then {
    Nothing,
    /// Button 0 opens the account plugin page and, if given, reveals the archive.
    Cowork(Option<PathBuf>),
    /// Button 0 opens the platform's login item settings.
    LoginSettings,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub text: String,
    pub buttons: Vec<String>,
    then: Then,
}

/// One menu entry as a window draws it; activating it is the menu event `id`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    pub id: MenuId,
    pub label: String,
    pub enabled: bool,
    /// `Some` for a check item.
    pub checked: Option<bool>,
}

/// The menu's current state, for a window that shows the same thing.
#[cfg_attr(target_os = "macos", allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub attention: Option<Control>,
    pub details: String,
    pub sync: Control,
    pub retry: Option<Control>,
    pub integrations: Vec<Control>,
    pub storage: Control,
    pub login: Control,
    pub quit: Control,
    pub prompt: Option<Prompt>,
}

pub struct App<S: Shell> {
    pub shell: S,
    layout: Layout,
    runtime: Option<Runtime>,
    pub problem: Option<String>,
    items: Items,
    pub menu: Menu,
    status_visible: bool,
    retry_visible: bool,
    origin: Instant,
    activity: Option<WorkActivity>,
    pub next_refresh: Instant,
    quitting: Option<Instant>,
    terminate: Arc<AtomicBool>,
    plugin_states: [Option<Result<install::Inventory, String>>; 2],
    clients: Option<install::ClientAvailability>,
    plugin_task: Option<PluginTask>,
    /// The last plugin failure in the client's own words, for the details.
    plugin_error: Option<String>,
    /// Standing observations about client configuration, for the details.
    plugin_notes: Vec<String>,
    prompts: VecDeque<Prompt>,
    /// Tests run without services; this stands in for their MCP URL.
    #[cfg(test)]
    test_url: Option<String>,
}

enum PluginResult {
    Inventory {
        clients: install::ClientAvailability,
        states: [Option<Result<install::Inventory, String>>; 2],
        notes: Vec<String>,
    },
    Changed(
        install::Host,
        install::Change,
        Result<install::Inventory, String>,
    ),
    Refreshed(Vec<(install::Host, Result<install::Inventory, String>)>),
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

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn control(item: &MenuItem) -> Control {
    Control {
        id: item.id().clone(),
        label: item.text(),
        enabled: item.is_enabled(),
        checked: None,
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn check_control(item: &CheckMenuItem) -> Control {
    Control {
        id: item.id().clone(),
        label: item.text(),
        enabled: item.is_enabled(),
        checked: Some(item.is_checked()),
    }
}

impl<S: Shell> App<S> {
    pub fn new(shell: S, layout: Layout, terminate: Arc<AtomicBool>) -> Self {
        let items = Items {
            status: MenuItem::new("Attention needed", true, None),
            integrations: Submenu::new("Integrations", true),
            no_clients: MenuItem::new("Checking clients…", false, None),
            chatgpt: CheckMenuItem::new("OpenAI local plugin · checking…", false, false, None),
            openai_manual: MenuItem::new("OpenAI plugin · CLI unavailable…", false, None),
            claude: CheckMenuItem::new("Claude Code plugin · checking…", false, false, None),
            cowork: MenuItem::new("Manage Cowork plugin…", false, None),
            refresh_plugins: MenuItem::new("Refresh plugin status…", false, None),
            login: CheckMenuItem::new("Open at login", true, false, None),
            pause_sync: MenuItem::new("Pause sync", false, None),
            retry_services: MenuItem::new("Retry services", false, None),
            storage_folder: MenuItem::new("Show storage folder", false, None),
            quit: MenuItem::new("Quit Erlkonig", true, None),
        };
        items
            .integrations
            .append_items(&[&items.no_clients, &items.refresh_plugins])
            .expect("integrations menu");
        let menu = Menu::new();
        menu.append_items(&[
            &items.pause_sync,
            &PredefinedMenuItem::separator(),
            &items.integrations,
            &items.storage_folder,
            &PredefinedMenuItem::separator(),
            &items.login,
            &items.quit,
        ])
        .expect("menu");
        App {
            shell,
            layout,
            runtime: None,
            problem: None,
            items,
            menu,
            status_visible: false,
            retry_visible: false,
            origin: Instant::now(),
            activity: None,
            next_refresh: Instant::now(),
            quitting: None,
            terminate,
            plugin_states: [None, None],
            clients: None,
            plugin_task: None,
            plugin_error: None,
            plugin_notes: Vec::new(),
            prompts: VecDeque::new(),
            #[cfg(test)]
            test_url: None,
        }
    }

    /// Start native services once the platform's status item exists.
    pub fn start(&mut self) {
        if std::env::current_exe().is_ok_and(|p| paths::is_translocated(&p)) {
            self.problem = Some("Move Erlkonig to Applications, then open it again.".into());
        } else {
            match Runtime::start(&self.layout) {
                Ok(runtime) => self.runtime = Some(runtime),
                Err(error) => self.problem = Some(format!("{error:#}")),
            }
        }
        self.check_plugins();
    }

    /// The monotonic origin the crown's activity is measured against.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn origin(&self) -> Instant {
        self.origin
    }

    /// Show a prompt and continue with `then` once it is answered: at once
    /// for a native modal, or when the shell later calls [`Self::answer`].
    pub fn ask(&mut self, title: &str, text: &str, buttons: &[&str], then: Then) {
        match self.shell.alert(title, text, buttons) {
            Some(answer) => self.follow(then, answer),
            None => {
                self.prompts.push_back(Prompt {
                    title: title.into(),
                    text: text.into(),
                    buttons: buttons.iter().map(|b| (*b).into()).collect(),
                    then,
                });
                self.shell.show_window();
            }
        }
    }

    /// The oldest queued prompt was answered with button `index`.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn answer(&mut self, index: usize) {
        if let Some(prompt) = self.prompts.pop_front() {
            self.follow(prompt.then, index);
        }
        self.next_refresh = Instant::now();
    }

    fn follow(&mut self, then: Then, answer: usize) {
        match then {
            Then::Nothing => {}
            Then::Cowork(path) => {
                if answer == 0 {
                    self.shell.open_url(install::COWORK_URL);
                    if let Some(path) = path {
                        self.shell.reveal(&path);
                    }
                }
            }
            Then::LoginSettings => {
                if answer == 0 {
                    self.shell.open_login_settings();
                }
            }
        }
    }

    /// The running services' MCP URL, which every staged package names.
    fn mcp_url(&self) -> Option<String> {
        #[cfg(test)]
        if let Some(url) = &self.test_url {
            return Some(url.clone());
        }
        self.runtime.as_ref().and_then(|r| r.status().mcp_url)
    }

    /// The text behind the attention item: the problem, or the live status,
    /// then the last plugin failure and any client configuration notes.
    fn details(&self) -> String {
        let mut text = self.problem.clone().unwrap_or_else(|| {
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
        for line in self.plugin_error.iter().chain(&self.plugin_notes) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(line);
        }
        text
    }

    /// One short prompt for a plugin failure; the client's own words go to
    /// the status details instead of the dialog.
    fn plugin_failed(&mut self, title: &str, sentence: &str, error: String) {
        self.plugin_error = Some(error);
        self.ask(title, sentence, &["OK"], Then::Nothing);
    }

    /// The menu as it is right now, plus the oldest unanswered prompt.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn view(&self) -> View {
        let integrations = self
            .items
            .integrations
            .items()
            .iter()
            .filter_map(|item| match item {
                MenuItemKind::MenuItem(item) => Some(control(item)),
                MenuItemKind::Check(item) => Some(check_control(item)),
                _ => None,
            })
            .collect();
        View {
            attention: self.status_visible.then(|| control(&self.items.status)),
            details: self.details(),
            sync: control(&self.items.pause_sync),
            retry: self
                .retry_visible
                .then(|| control(&self.items.retry_services)),
            integrations,
            storage: control(&self.items.storage_folder),
            login: check_control(&self.items.login),
            quit: control(&self.items.quit),
            prompt: self.prompts.front().cloned(),
        }
    }

    pub fn refresh(&mut self, event_loop: &ActiveEventLoop) {
        if self.tick() {
            event_loop.exit();
        }
    }

    /// One refresh step; true once the event loop should exit.
    pub fn tick(&mut self) -> bool {
        if self.terminate.load(Ordering::Relaxed) && self.quitting.is_none() {
            self.quit();
        }
        if let Some(since) = self.quitting {
            let mut exit = false;
            if since.elapsed() >= Duration::from_secs(15) {
                // The person's Quit is final. A stuck admitted native call
                // cannot leave the app alive indefinitely. Like Force Quit,
                // this fallback cannot promise to flush an unfinished write.
                eprintln!(
                    "Erlkonig: 15-second shutdown grace elapsed; terminating this application process"
                );
                std::process::exit(1);
            }
            self.items
                .quit
                .set_text(if since.elapsed() < Duration::from_secs(5) {
                    "Shutting down…"
                } else {
                    "Finishing work…"
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
                        eprintln!("Erlkonig shutdown: {error:#}");
                    }
                }
                exit = true;
            }
            self.next_refresh = Instant::now() + Duration::from_millis(50);
            return exit;
        }
        // Status/config polling is independent of the native display clock.
        // Image-caused events neither redraw nor slide this deadline forward.
        if Instant::now() < self.next_refresh {
            return false;
        }
        self.finish_plugin_task();
        let status = self.runtime.as_ref().map(Runtime::status);
        let attention = if self.problem.is_some() {
            Some("Attention needed")
        } else {
            status
                .as_ref()
                .and_then(|s| s.attention_label())
                .or(self.plugin_error.as_ref().map(|_| "Plugin problem"))
        };
        if let Some(label) = attention {
            self.items.status.set_text(label);
            if !self.status_visible {
                self.menu
                    .insert(&self.items.status, 0)
                    .expect("insert attention item");
                self.status_visible = true;
            }
        } else if self.status_visible {
            self.menu
                .remove(&self.items.status)
                .expect("remove attention item");
            self.status_visible = false;
        }
        let retry_visible = status.as_ref().is_some_and(|s| {
            s.config_problem.is_none()
                && s.node.services == erlkonig_core::status::ServiceState::Stopped
        });
        let retry_pending = status.as_ref().is_some_and(|s| s.node.retry_pending);
        self.items.retry_services.set_text(if retry_pending {
            "Retry requested…"
        } else {
            "Retry services"
        });
        self.items
            .retry_services
            .set_enabled(retry_visible && !retry_pending);
        if retry_visible && !self.retry_visible {
            self.menu
                .insert(
                    &self.items.retry_services,
                    1 + usize::from(self.status_visible),
                )
                .expect("insert retry item");
            self.retry_visible = true;
        } else if !retry_visible && self.retry_visible {
            self.menu
                .remove(&self.items.retry_services)
                .expect("remove retry item");
            self.retry_visible = false;
        }
        self.items.storage_folder.set_enabled(
            self.runtime
                .as_ref()
                .and_then(|runtime| runtime.binding().storage_folder())
                .is_some(),
        );
        self.items
            .pause_sync
            .set_enabled(status.as_ref().is_some_and(|s| s.config_problem.is_none()));
        self.items
            .pause_sync
            .set_text(if status.as_ref().is_some_and(|s| s.node.sync_enabled) {
                "Pause sync"
            } else {
                "Resume sync"
            });
        self.update_plugin_items();
        self.items
            .login
            .set_checked(self.shell.login() != Login::Off);
        let now = self.origin.elapsed();
        crown::observe(
            &mut self.activity,
            status.as_ref().is_some_and(|s| s.active_work > 0),
            now,
        );
        self.shell.crown(self.activity, attention.is_some());
        self.next_refresh = Instant::now() + Duration::from_millis(250);
        false
    }

    pub fn quit(&mut self) {
        self.quitting.get_or_insert_with(Instant::now);
        self.shell.stop_crown(); // Invalidate display callbacks before service teardown.
        // Quit is final: unanswered prompts no longer lead anywhere.
        self.prompts.clear();
        if let Some(task) = &self.plugin_task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        self.items.chatgpt.set_enabled(false);
        self.items.claude.set_enabled(false);
        for item in [
            &self.items.openai_manual,
            &self.items.cowork,
            &self.items.refresh_plugins,
            &self.items.storage_folder,
            &self.items.quit,
        ] {
            item.set_enabled(false);
        }
        self.items.login.set_enabled(false);
        self.items.status.set_enabled(false);
        self.items.pause_sync.set_enabled(false);
        self.items.retry_services.set_enabled(false);
        if self.retry_visible {
            self.menu
                .remove(&self.items.retry_services)
                .expect("remove retry item");
            self.retry_visible = false;
        }
        self.items.integrations.set_enabled(false);
        if let Some(runtime) = &self.runtime {
            runtime.begin_shutdown();
        }
    }

    /// The platform can terminate without returning from its event loop
    /// (AppKit delivers LoopExiting synchronously from
    /// applicationWillTerminate), so drain here, with the same final
    /// process-exit deadline.
    pub fn finish_termination(&mut self) {
        self.shell.stop_crown();
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
                    eprintln!("Erlkonig: shutdown grace elapsed during application termination");
                    std::process::exit(1);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if let Err(error) = runtime.finish() {
                eprintln!("Erlkonig shutdown: {error:#}");
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
            Err(error) => self.plugin_failed(
                "Plugin not changed",
                "Erlkonig could not start the plugin operation; the details are in its status.",
                format!("Plugin operation: {error}"),
            ),
        }
        self.update_plugin_items();
    }

    fn check_plugins(&mut self) {
        if self.plugin_task.is_some() || self.quitting.is_some() {
            return;
        }
        // The current OpenAI app is branded ChatGPT but retains this bundle ID.
        // Use its public display name, not its path or ID, for the menu label.
        let (openai, claude_desktop) = self.shell.detect_apps();
        let openai_apps = openai
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let mut clients = install::ClientAvailability {
            openai_app: openai.map(|(_, name)| name),
            claude_desktop,
            ..Default::default()
        };
        let layout = self.layout.clone();
        self.start_plugin_task(move |cancel| {
            clients.openai_cli =
                install::find_client_with_apps(install::Host::OpenAi, &openai_apps);
            clients.claude_cli = install::find_client_with_apps(install::Host::ClaudeCode, &[]);
            let hosts = [install::Host::OpenAi, install::Host::ClaudeCode];
            let states = hosts.map(|host| {
                clients
                    .cli(host)
                    .map(|program| install::inventory_with_client(host, program, &layout, &cancel))
            });
            let notes = hosts
                .into_iter()
                .filter(|host| clients.cli(*host).is_some())
                .filter_map(install::legacy_config_notice)
                .collect();
            PluginResult::Inventory {
                clients,
                states,
                notes,
            }
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
            Ok(PluginResult::Inventory {
                clients,
                states,
                notes,
            }) => {
                self.clients = Some(clients);
                if states.iter().flatten().all(Result::is_ok) {
                    self.plugin_error = None;
                }
                self.plugin_states = states;
                self.plugin_notes = notes;
                self.rebuild_integrations();
                self.refresh_stale_plugins();
            }
            // Success is the check mark itself; only a failure speaks.
            Ok(PluginResult::Changed(host, change, result)) => {
                match &result {
                    Ok(_) => self.plugin_error = None,
                    Err(error) => {
                        let (title, verb) = match change {
                            install::Change::Install => ("Plugin not installed", "install"),
                            install::Change::Remove => ("Plugin not removed", "remove"),
                            install::Change::Refresh => ("Plugin not updated", "update"),
                        };
                        self.plugin_failed(
                            title,
                            &format!(
                                "{} did not {verb} the Erlkonig plugin; the details are in Erlkonig's status.",
                                host.client()
                            ),
                            format!("{}: {error}", host.label()),
                        );
                    }
                }
                self.plugin_states[host_index(host)] = Some(result);
            }
            // A quiet refresh speaks once, however many clients failed.
            Ok(PluginResult::Refreshed(results)) => {
                let (mut clients, mut errors) = (Vec::new(), Vec::new());
                for (host, result) in results {
                    if let Err(error) = &result {
                        clients.push(host.client());
                        errors.push(format!("{}: {error}", host.label()));
                    }
                    self.plugin_states[host_index(host)] = Some(result);
                }
                if !errors.is_empty() {
                    self.plugin_failed(
                        "Plugin not updated",
                        &format!(
                            "{} could not update the Erlkonig plugin; the details are in Erlkonig's status.",
                            clients.join(" and ")
                        ),
                        errors.join("\n"),
                    );
                }
            }
            Ok(PluginResult::Archive(result)) => match result {
                Ok(path) => {
                    self.ask(
                        "Manage Cowork plugin",
                        &format!("{}\n\nArchive: {}", install::COWORK_HELP, path.display()),
                        &["Open account plugins", "Cancel"],
                        Then::Cowork(Some(path)),
                    );
                }
                Err(error) => self.plugin_failed(
                    "Cowork package unavailable",
                    "Erlkonig could not prepare the plugin ZIP; the details are in its status.",
                    format!("Cowork package: {error}"),
                ),
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

    /// The check mark is the consent: a click on an unchecked row installs,
    /// a click on a checked row removes, with no dialog in between.
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
        let change = if state.installed {
            install::Change::Remove
        } else {
            install::Change::Install
        };
        let url = self.mcp_url();
        if change == install::Change::Install && url.is_none() {
            self.plugin_failed(
                "Plugin not installed",
                "Erlkonig's services are not running, so the plugin would have nothing to connect to.",
                format!("{}: services are not running", host.label()),
            );
            return;
        }
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                self.plugin_failed(
                    "Plugin not changed",
                    "Erlkonig cannot locate its own program file; the details are in its status.",
                    format!("{}: {error}", host.label()),
                );
                return;
            }
        };
        let layout = self.layout.clone();
        self.start_plugin_task(move |cancel| {
            PluginResult::Changed(
                host,
                change,
                install::change_with_client(
                    host,
                    &program,
                    change,
                    &layout,
                    &executable,
                    url.as_deref().unwrap_or(""),
                    &cancel,
                ),
            )
        });
    }

    /// After an inventory: if the staged package was made for another
    /// executable path, data directory, endpoint or package version, quietly
    /// reinstall the plugins the clients report installed. Unchecked plugins
    /// are never touched; the worker re-reads each inventory before acting.
    fn refresh_stale_plugins(&mut self) {
        let installed = [install::Host::OpenAi, install::Host::ClaudeCode]
            .into_iter()
            .filter(|host| {
                matches!(
                    &self.plugin_states[host_index(*host)],
                    Some(Ok(state)) if state.installed
                )
            })
            .filter_map(|host| {
                let clients = self.clients.as_ref()?;
                Some((host, clients.cli(host)?.to_path_buf()))
            })
            .collect::<Vec<_>>();
        if installed.is_empty() {
            return;
        }
        let (Some(url), Ok(executable)) = (self.mcp_url(), std::env::current_exe()) else {
            return;
        };
        let layout = self.layout.clone();
        self.start_plugin_task(move |cancel| {
            PluginResult::Refreshed(install::refresh_if_stale(
                &installed,
                &layout,
                &executable,
                &url,
                &cancel,
            ))
        });
    }

    fn manage_cowork(&mut self) {
        let Some(url) = self.runtime.as_ref().and_then(|r| r.status().mcp_url) else {
            self.ask(
                "Manage Cowork plugin",
                install::COWORK_HELP,
                &["Open account plugins", "Cancel"],
                Then::Cowork(None),
            );
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

    fn toggle_login(&mut self) {
        let on = self.shell.login();
        if on == Login::NeedsApproval {
            self.ask(
                "Open at login",
                "macOS requires approval in System Settings > General > Login Items & Extensions.",
                &["Open Settings", "Cancel"],
                Then::LoginSettings,
            );
            return;
        }
        if let Err(error) = self.shell.set_login(on != Login::On) {
            self.ask("Open at login", &error, &["OK"], Then::Nothing);
        }
    }

    fn toggle_sync(&mut self) {
        let Some(runtime) = &self.runtime else {
            return;
        };
        let result = (|| {
            let expected = runtime.config().map_err(|error| error.to_string())?;
            let mut settings = expected.settings().map_err(|error| error.to_string())?;
            settings.sync_enabled = !settings.sync_enabled;
            runtime
                .put_settings(&expected, &settings)
                .map_err(|error| error.to_string())?;
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            self.ask("Sync setting unchanged", &error, &["OK"], Then::Nothing);
        }
    }

    fn show_storage_folder(&mut self) {
        if self.quitting.is_some() {
            return;
        }
        let Some(folder) = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.binding().storage_folder())
        else {
            return;
        };
        if let Err(text) = self.shell.show_folder(&folder) {
            self.ask("Storage folder unavailable", text, &["OK"], Then::Nothing);
        }
    }

    /// Handle one menu (or window) activation; the caller refreshes next.
    pub fn on_menu(&mut self, event: MenuEvent) {
        let id = event.id();
        if id == self.items.quit.id() {
            self.quit();
        } else if id == self.items.pause_sync.id() {
            self.toggle_sync();
        } else if id == self.items.retry_services.id() {
            if let Some(Err(error)) = self.runtime.as_ref().map(Runtime::retry_services) {
                self.ask(
                    "Services not retried",
                    &error.to_string(),
                    &["OK"],
                    Then::Nothing,
                );
            }
        } else if id == self.items.storage_folder.id() {
            self.show_storage_folder();
        } else if id == self.items.chatgpt.id() {
            self.plugin_action(install::Host::OpenAi);
        } else if id == self.items.openai_manual.id() {
            self.ask(
                "Codex CLI not found",
                "Erlkonig installs this plugin with the Codex CLI. Install it, then choose Refresh plugin status.",
                &["OK"],
                Then::Nothing,
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
            let text = self.details();
            self.ask("Erlkonig", &text, &["OK"], Then::Nothing);
        }
        self.next_refresh = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Records what the app asked the platform to do.
    #[derive(Default)]
    struct Fake {
        modal: Option<usize>,
        login: Option<Login>,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Shell for Fake {
        fn alert(&mut self, title: &str, _: &str, _: &[&str]) -> Option<usize> {
            self.log.borrow_mut().push(format!("alert {title}"));
            self.modal
        }
        fn open_url(&self, url: &str) {
            self.log.borrow_mut().push(format!("open {url}"));
        }
        fn reveal(&self, path: &Path) {
            self.log
                .borrow_mut()
                .push(format!("reveal {}", path.display()));
        }
        fn show_folder(&self, _: &Path) -> Result<(), &'static str> {
            Ok(())
        }
        fn login(&self) -> Login {
            self.login.unwrap_or(Login::Off)
        }
        fn set_login(&self, on: bool) -> Result<(), String> {
            self.log.borrow_mut().push(format!("login {on}"));
            Ok(())
        }
        fn open_login_settings(&self) {
            self.log.borrow_mut().push("settings".into());
        }
        fn detect_apps(&self) -> (Option<(PathBuf, String)>, bool) {
            (None, false)
        }
        fn crown(&mut self, _: Option<WorkActivity>, attention: bool) {
            self.log.borrow_mut().push(format!("crown {attention}"));
        }
        fn stop_crown(&mut self) {
            self.log.borrow_mut().push("stop crown".into());
        }
        fn show_window(&mut self) {
            self.log.borrow_mut().push("window".into());
        }
    }

    fn app(shell: Fake) -> App<Fake> {
        let dir = std::env::temp_dir().join("erlkonig-app-tests-unused");
        App::new(shell, Layout::new(dir), Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn drawn_prompts_queue_in_order_and_answer_the_oldest_first() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = app(Fake {
            log: log.clone(),
            ..Fake::default()
        });
        app.ask("first", "one", &["OK"], Then::Nothing);
        app.ask(
            "second",
            "two",
            &["Open account plugins", "Cancel"],
            Then::Cowork(Some(PathBuf::from("/tmp/erlkonig.zip"))),
        );
        app.ask(
            "third",
            "three",
            &["Open Settings", "Cancel"],
            Then::LoginSettings,
        );
        assert_eq!(app.view().prompt.map(|p| p.title), Some("first".into()));
        app.answer(0);
        assert_eq!(app.view().prompt.map(|p| p.title), Some("second".into()));
        assert!(!log.borrow().iter().any(|line| line.starts_with("open")));
        app.answer(0);
        assert_eq!(app.view().prompt.map(|p| p.title), Some("third".into()));
        app.answer(1); // Cancel: nothing follows.
        assert_eq!(app.view().prompt, None);
        app.answer(0); // Nothing left to answer; nothing happens.
        let expected = [
            "alert first".to_owned(),
            "window".into(),
            "alert second".into(),
            "window".into(),
            "alert third".into(),
            "window".into(),
            format!("open {}", install::COWORK_URL),
            "reveal /tmp/erlkonig.zip".into(),
        ];
        assert_eq!(*log.borrow(), expected);
    }

    #[test]
    fn a_native_modal_answers_at_once_without_queueing() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = app(Fake {
            modal: Some(0),
            log: log.clone(),
            ..Fake::default()
        });
        app.ask(
            "Open at login",
            "approve",
            &["Open Settings", "Cancel"],
            Then::LoginSettings,
        );
        assert_eq!(app.view().prompt, None);
        assert_eq!(
            *log.borrow(),
            ["alert Open at login", "settings"]
                .map(String::from)
                .to_vec()
        );
    }

    #[test]
    fn quit_drops_unanswered_prompts() {
        let mut app = app(Fake::default());
        app.ask("first", "one", &["OK"], Then::Nothing);
        app.quit();
        assert_eq!(app.view().prompt, None);
    }

    #[test]
    fn the_window_view_is_the_menu_state() {
        let mut app = app(Fake {
            login: Some(Login::On),
            ..Fake::default()
        });
        app.problem = Some("Cannot create menu: no host".into());
        assert!(!app.tick());
        let view = app.view();
        let attention = view.attention.expect("attention shown");
        assert_eq!(attention.label, "Attention needed");
        assert_eq!(view.details, "Cannot create menu: no host");
        assert_eq!(view.sync.label, "Resume sync");
        assert!(!view.sync.enabled);
        assert_eq!(view.retry, None);
        assert_eq!(
            view.integrations
                .iter()
                .map(|c| (c.label.as_str(), c.enabled))
                .collect::<Vec<_>>(),
            [
                ("Checking clients…", false),
                ("Refresh plugin status…", true)
            ]
        );
        assert!(!view.storage.enabled);
        assert_eq!(view.login.checked, Some(true));
        assert!(view.login.enabled);
        assert_eq!(
            (view.quit.label.as_str(), view.quit.enabled),
            ("Quit Erlkonig", true)
        );
        // The quit control activates the same menu event as the tray.
        app.on_menu(MenuEvent {
            id: view.quit.id.clone(),
        });
        assert!(app.tick(), "nothing to drain, so the loop exits");
        let view = app.view();
        assert_eq!(
            (view.quit.label.as_str(), view.quit.enabled),
            ("Shutting down…", false)
        );
        assert!(!view.login.enabled);
    }

    #[test]
    fn login_toggle_follows_the_observed_state() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = app(Fake {
            login: Some(Login::On),
            log: log.clone(),
            ..Fake::default()
        });
        let id = app.view().login.id;
        app.on_menu(MenuEvent { id });
        assert_eq!(*log.borrow(), ["login false"].map(String::from).to_vec());
    }

    const URL: &str = "http://127.0.0.1:41773/";

    fn fake_client(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let program = dir.join(name);
        std::fs::write(
            &program,
            include_str!("../../erlkonig-core/tests/fixtures/fake-client.sh"),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        program
    }

    /// The argv lists a fake client received since the last call.
    fn take_argv(program: &Path) -> Vec<Vec<String>> {
        let log = program.with_file_name("argv.log");
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        std::fs::write(&log, "").unwrap();
        text.lines()
            .map(|line| line.split_terminator('\u{1f}').map(String::from).collect())
            .collect()
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn alerts(log: &Rc<RefCell<Vec<String>>>) -> Vec<String> {
        log.borrow()
            .iter()
            .filter(|line| line.starts_with("alert "))
            .cloned()
            .collect()
    }

    /// Let the plugin worker (and anything it starts) finish.
    fn settle(app: &mut App<Fake>) {
        for _ in 0..1500 {
            app.next_refresh = Instant::now();
            app.tick();
            if app.plugin_task.is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("plugin worker did not finish");
    }

    /// A fake client whose plugin is (or is not) installed from our marketplace.
    fn client(dir: &Path, name: &str, layout: &Layout, installed: bool) -> PathBuf {
        let program = fake_client(dir, name);
        let root = install::marketplace_root(layout);
        std::fs::write(dir.join("market"), format!("{}\n", root.display())).unwrap();
        std::fs::write(dir.join("installed"), if installed { "1\n" } else { "0\n" }).unwrap();
        program
    }

    /// An app whose Claude Code row shows the fake client's inventory.
    fn plugin_app(temp: &Path, shell: Fake, installed: bool) -> (App<Fake>, PathBuf) {
        let layout = Layout::new(temp.join("data"));
        std::fs::create_dir_all(&layout.dir).unwrap();
        let claude = client(&temp.join("claude"), "claude", &layout, installed);
        let mut app = App::new(shell, layout.clone(), Arc::new(AtomicBool::new(false)));
        app.test_url = Some(URL.into());
        app.clients = Some(install::ClientAvailability {
            claude_cli: Some(claude.clone()),
            ..Default::default()
        });
        app.plugin_states[1] = Some(install::inventory_with_client(
            install::Host::ClaudeCode,
            &claude,
            &layout,
            &AtomicBool::new(false),
        ));
        take_argv(&claude);
        app.rebuild_integrations();
        app.update_plugin_items();
        assert_eq!(app.items.claude.is_checked(), installed);
        (app, claude)
    }

    fn click_claude(app: &mut App<Fake>) {
        let id = app.items.claude.id().clone();
        app.on_menu(MenuEvent { id });
    }

    #[test]
    fn clicking_an_unchecked_plugin_installs_it_without_a_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let shell = Fake {
            log: log.clone(),
            ..Fake::default()
        };
        let (mut app, claude) = plugin_app(temp.path(), shell, false);
        click_claude(&mut app);
        assert_eq!(app.view().prompt, None);
        settle(&mut app);
        let calls = take_argv(&claude);
        assert!(
            calls.contains(&argv(&[
                "plugin",
                "install",
                install::PLUGIN_ID,
                "--scope",
                "user"
            ])),
            "{calls:?}"
        );
        assert!(app.items.claude.is_checked());
        assert_eq!(alerts(&log), Vec::<String>::new());
        assert_eq!(app.view().prompt, None);
    }

    #[test]
    fn clicking_a_checked_plugin_uninstalls_it_without_a_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let shell = Fake {
            log: log.clone(),
            ..Fake::default()
        };
        let (mut app, claude) = plugin_app(temp.path(), shell, true);
        click_claude(&mut app);
        assert_eq!(app.view().prompt, None);
        settle(&mut app);
        let calls = take_argv(&claude);
        assert!(
            calls.contains(&argv(&[
                "plugin",
                "uninstall",
                install::PLUGIN_ID,
                "--scope",
                "user"
            ])),
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.contains(&"install".to_owned()))
        );
        assert!(!app.items.claude.is_checked());
        assert_eq!(alerts(&log), Vec::<String>::new());
    }

    #[test]
    fn success_raises_no_prompt_even_where_modals_answer_at_once() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        // A native modal that would press its first button.
        let shell = Fake {
            modal: Some(0),
            log: log.clone(),
            ..Fake::default()
        };
        let (mut app, _claude) = plugin_app(temp.path(), shell, false);
        click_claude(&mut app);
        settle(&mut app);
        assert!(app.items.claude.is_checked());
        assert_eq!(alerts(&log), Vec::<String>::new());
    }

    #[test]
    fn a_client_error_raises_exactly_one_short_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let shell = Fake {
            log: log.clone(),
            ..Fake::default()
        };
        let (mut app, claude) = plugin_app(temp.path(), shell, false);
        std::fs::write(claude.with_file_name("fail"), "").unwrap();
        click_claude(&mut app);
        settle(&mut app);
        assert_eq!(alerts(&log), ["alert Plugin not installed"]);
        let prompt = app.view().prompt.expect("one error prompt");
        assert_eq!(prompt.buttons, ["OK"]);
        assert!(!prompt.text.contains('\n'), "{}", prompt.text);
        assert!(prompt.text.len() <= 100, "{}", prompt.text);
        assert_eq!(prompt.text.matches(". ").count(), 0, "{}", prompt.text);
        // The client's own words stay available in the status details.
        assert!(
            app.view().details.contains("failed"),
            "{}",
            app.view().details
        );
        assert!(!app.items.claude.is_checked());
    }

    /// Inventory as the worker reports it, from the given fake clients.
    fn inventory(app: &mut App<Fake>, claude: PathBuf, codex: PathBuf) {
        let layout = app.layout.clone();
        let clients = install::ClientAvailability {
            openai_cli: Some(codex),
            claude_cli: Some(claude),
            ..Default::default()
        };
        app.start_plugin_task(move |cancel| {
            let states = [install::Host::OpenAi, install::Host::ClaudeCode].map(|host| {
                clients
                    .cli(host)
                    .map(|program| install::inventory_with_client(host, program, &layout, &cancel))
            });
            PluginResult::Inventory {
                clients,
                states,
                notes: Vec::new(),
            }
        });
        settle(app);
    }

    #[test]
    fn a_stale_package_is_refreshed_quietly_for_installed_plugins_only() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let layout = Layout::new(temp.path().join("data"));
        std::fs::create_dir_all(&layout.dir).unwrap();
        // Staged for an app bundle that has since moved.
        install::prepare(&layout, Path::new("/old/Erlkonig.app/erlkonig"), URL).unwrap();
        let claude = client(&temp.path().join("claude"), "claude", &layout, true);
        let codex = client(&temp.path().join("codex"), "codex", &layout, false);
        let mut app = App::new(
            Fake {
                log: log.clone(),
                ..Fake::default()
            },
            layout.clone(),
            Arc::new(AtomicBool::new(false)),
        );
        app.test_url = Some(URL.into());
        inventory(&mut app, claude.clone(), codex.clone());
        let calls = take_argv(&claude);
        let reinstall = [
            argv(&[
                "plugin",
                "uninstall",
                install::PLUGIN_ID,
                "--scope",
                "user",
                "--keep-data",
            ]),
            argv(&["plugin", "install", install::PLUGIN_ID, "--scope", "user"]),
        ];
        assert!(calls.windows(2).any(|w| w == reinstall), "{calls:?}");
        // The unchecked plugin is never installed or touched.
        let calls = take_argv(&codex);
        assert!(
            calls.iter().all(|call| call.contains(&"list".to_owned())),
            "{calls:?}"
        );
        assert!(app.items.claude.is_checked());
        assert!(!app.items.chatgpt.is_checked());
        let staged = install::marketplace_root(&layout).join("claude/.mcp.json");
        let staged = std::fs::read_to_string(staged).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert!(staged.contains(&format!("\"command\": \"{}\"", exe.display())));
        assert!(!staged.contains("/old/"));
        assert_eq!(alerts(&log), Vec::<String>::new());
        // Once current, a later inventory changes nothing.
        inventory(&mut app, claude.clone(), codex.clone());
        let calls = take_argv(&claude);
        assert!(
            calls.iter().all(|call| call.contains(&"list".to_owned())),
            "{calls:?}"
        );
    }

    #[test]
    fn a_current_package_is_not_refreshed() {
        let temp = tempfile::tempdir().unwrap();
        let layout = Layout::new(temp.path().join("data"));
        std::fs::create_dir_all(&layout.dir).unwrap();
        let exe = std::env::current_exe().unwrap();
        install::prepare(&layout, &exe, URL).unwrap();
        let claude = client(&temp.path().join("claude"), "claude", &layout, true);
        let codex = client(&temp.path().join("codex"), "codex", &layout, true);
        let mut app = App::new(Fake::default(), layout, Arc::new(AtomicBool::new(false)));
        app.test_url = Some(URL.into());
        inventory(&mut app, claude.clone(), codex.clone());
        for program in [claude, codex] {
            let calls = take_argv(&program);
            assert!(
                calls.iter().all(|call| call.contains(&"list".to_owned())),
                "{calls:?}"
            );
        }
        assert!(app.items.claude.is_checked() && app.items.chatgpt.is_checked());
    }
}
