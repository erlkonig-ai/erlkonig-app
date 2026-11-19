//! The platform-neutral application: menu state, the one application-owned
//! native lifetime, the plugin worker and the quit drain. Each platform owns
//! the event loop and implements [`Shell`] for what only it can do.

use crate::crown::{self, WorkActivity};
use crate::native_menu::NativeMenu;
use erlkonig_core::{
    install,
    paths::{self, Layout},
    pile_upgrade::{StartupPrompt, StartupControl},
    runtime::Runtime,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tray_icon::menu::{Menu, MenuEvent, MenuId};
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
    /// Queue a wake on the existing event loop, without waiting for that thread.
    /// Called from egui's repaint callback while its context is write-locked.
    fn faculties_wake(&self) -> Box<dyn Fn() + Send + Sync>;
    /// A native modal answers with the pressed button's index. A shell that
    /// draws its prompts itself returns `None`: [`App`] then queues the prompt
    /// and the shell answers later through [`App::answer`].
    fn alert(&mut self, title: &str, text: &str, buttons: &[&str]) -> Option<usize>;
    /// Startup's native modal must also observe application termination.
    fn startup_alert(&mut self, title: &str, text: &str, buttons: &[&str], _terminate: &Arc<AtomicBool>) -> Option<usize> {
        self.alert(title, text, buttons)
    }
    fn open_url(&self, url: &str);
    /// Show `path` selected in the file manager.
    fn reveal(&self, path: &Path);
    /// Open `folder` in the file manager; the error is the person-facing text.
    fn show_folder(&self, folder: &Path) -> Result<(), &'static str>;
    fn login(&self) -> Login;
    /// State for passive display. A platform with an expensive registration
    /// lookup can supply the state it observed when its menu was presented.
    fn displayed_login(&self) -> Login { self.login() }
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
    /// Called only on the shell's event-loop thread. Unsupported platforms
    /// return Unavailable, never collect credentials through an ordinary alert.
    fn enter_secret(
        &mut self,
        _prompt: &erlkonig_core::secret_entry::SecretPrompt,
        _terminate: &Arc<AtomicBool>,
    ) -> erlkonig_core::secret_entry::EntryAnswer {
        erlkonig_core::secret_entry::EntryAnswer::Unavailable
    }
    /// One exact pending read, never an unlocked session. Unsupported shells
    /// deny without collecting a passphrase in an ordinary alert.
    fn unlock_secret(
        &mut self,
        _prompt: &erlkonig_core::protected_access::ProtectedPrompt,
        _terminate: &Arc<AtomicBool>,
    ) -> erlkonig_core::protected_access::AccessResult {
        erlkonig_core::protected_access::unavailable()
    }
    fn import_1password(
        &mut self,
        _prompt: &erlkonig_core::local_import::ImportPrompt,
        _terminate: &Arc<AtomicBool>,
    ) -> erlkonig_core::local_import::ImportAnswer {
        erlkonig_core::local_import::ImportAnswer::Unavailable
    }
}

/// The menu as plain data: what every entry shows, which optional entries
/// are present, and what the Integrations submenu lists. The native menu is
/// a projection of this ([`NativeMenu`]); the status window reads it through
/// [`App::view`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Items {
    pub(crate) status: Control,
    /// The submenu's own label and enabled state.
    pub(crate) integrations: Control,
    pub(crate) no_clients: Control,
    pub(crate) chatgpt: Control,
    pub(crate) openai_manual: Control,
    pub(crate) claude: Control,
    pub(crate) cowork: Control,
    pub(crate) refresh_plugins: Control,
    pub(crate) login: Control,
    pub(crate) pause_sync: Control,
    pub(crate) retry_services: Control,
    pub(crate) storage_folder: Control,
    pub(crate) browse_faculties: Control,
    pub(crate) quit: Control,
    pub(crate) get_update: Control,
    pub(crate) update_visible: bool,
    /// The attention item is shown, first in the menu.
    pub(crate) status_visible: bool,
    /// The retry item is shown, after Pause sync.
    pub(crate) retry_visible: bool,
    /// The Integrations submenu's rows, in order.
    pub(crate) rows: Vec<Row>,
}

/// One row of the Integrations submenu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    NoClients,
    ChatGpt,
    OpenAiManual,
    Claude,
    Cowork,
    Separator,
    Refresh,
}

fn entry(id: &str, label: &str, enabled: bool, checked: Option<bool>) -> Control {
    Control {
        id: MenuId::new(id),
        label: label.into(),
        enabled,
        checked,
    }
}

impl Items {
    fn new() -> Self {
        Items {
            status: entry("erlkonig.status", "Attention needed", true, None),
            integrations: entry("erlkonig.integrations", "Integrations", true, None),
            no_clients: entry("erlkonig.no-clients", "Checking clients…", false, None),
            chatgpt: entry(
                "erlkonig.openai-plugin",
                "OpenAI local plugin · checking…",
                false,
                Some(false),
            ),
            openai_manual: entry(
                "erlkonig.openai-manual",
                "OpenAI plugin · CLI unavailable…",
                false,
                None,
            ),
            claude: entry(
                "erlkonig.claude-plugin",
                "Claude Code plugin · checking…",
                false,
                Some(false),
            ),
            cowork: entry("erlkonig.cowork", "Manage Cowork plugin…", false, None),
            refresh_plugins: entry(
                "erlkonig.refresh-plugins",
                "Refresh plugin status…",
                false,
                None,
            ),
            login: entry("erlkonig.login", "Open at login", true, Some(false)),
            pause_sync: entry("erlkonig.sync", "Pause sync", false, None),
            retry_services: entry("erlkonig.retry", "Retry services", false, None),
            storage_folder: entry("erlkonig.storage", "Show storage folder", false, None),
            browse_faculties: entry("erlkonig.faculties", "View data", false, None),
            quit: entry("erlkonig.quit", "Quit Erlkonig", true, None),
            get_update: entry("erlkonig.get-update", "Get update…", true, None),
            update_visible: false,
            status_visible: false,
            retry_visible: false,
            rows: vec![Row::NoClients, Row::Refresh],
        }
    }

    /// The entry a submenu row shows; `None` for the separator.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn row(&self, row: Row) -> Option<&Control> {
        match row {
            Row::NoClients => Some(&self.no_clients),
            Row::ChatGpt => Some(&self.chatgpt),
            Row::OpenAiManual => Some(&self.openai_manual),
            Row::Claude => Some(&self.claude),
            Row::Cowork => Some(&self.cowork),
            Row::Separator => None,
            Row::Refresh => Some(&self.refresh_plugins),
        }
    }
}

/// What happens once the person has answered a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Then {
    Nothing,
    /// Button 0 opens the account plugin page and, if given, reveals the archive.
    Cowork(Option<PathBuf>),
    /// Button 0 opens the platform's login item settings.
    LoginSettings,
    /// Button 0 installs the plugin for these detected clients.
    Connect(Vec<install::Host>),
    /// Answer only the migration/branch choice offered by this startup task.
    StartupChoice,
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
    pub browse_faculties: Control,
    pub login: Control,
    pub quit: Control,
    pub prompt: Option<Prompt>,
}

pub struct App<S: Shell> {
    pub shell: S,
    layout: Layout,
    runtime: Option<Runtime>,
    faculties_browser: Option<crate::faculties_browser::BrowserWindow>,
    open_faculties: bool,
    update_notice: Option<crate::update_notice::UpdateNotice>,
    startup_task: Option<StartupTask>,
    startup_prompt: Option<StartupPrompt>,
    pub problem: Option<String>,
    items: Items,
    /// The native menu, once a shell has asked for it on its own thread.
    native: Option<NativeMenu>,
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
    /// Clients whose own settings name a separate MCP server `erlkonig`.
    plugin_manual: Vec<install::Host>,
    /// The first inventory of this run has been considered for onboarding.
    onboarded: bool,
    prompts: VecDeque<Prompt>,
    /// Tests run without services; this stands in for their MCP URL.
    #[cfg(test)]
    test_url: Option<String>,
}

enum PluginResult {
    Inventory {
        clients: install::ClientAvailability,
        states: [Option<Result<install::Inventory, String>>; 2],
        manual: Vec<install::Host>,
    },
    Changed(
        install::Change,
        Vec<(install::Host, Result<install::Inventory, String>)>,
    ),
    Archive(Result<PathBuf, String>),
}

struct PluginTask {
    cancel: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<PluginResult>,
}

struct StartupTask {
    control: StartupControl,
    thread: Option<std::thread::JoinHandle<Result<Runtime, String>>>,
}

impl StartupTask {
    fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|thread| thread.is_finished())
    }

    fn join(mut self) -> std::thread::Result<Result<Runtime, String>> {
        self.thread.take().expect("startup worker").join()
    }
}

impl Drop for StartupTask {
    fn drop(&mut self) {
        // Event-loop failures must not detach a startup owner either.
        self.control.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn host_index(host: install::Host) -> usize {
    match host {
        install::Host::OpenAi => 0,
        install::Host::ClaudeCode => 1,
    }
}

impl<S: Shell> App<S> {
    pub fn new(shell: S, layout: Layout, terminate: Arc<AtomicBool>) -> Self {
        App {
            shell,
            layout,
            runtime: None,
            faculties_browser: None,
            open_faculties: false,
            update_notice: None,
            startup_task: None,
            startup_prompt: None,
            problem: None,
            items: Items::new(),
            native: None,
            origin: Instant::now(),
            activity: None,
            next_refresh: Instant::now(),
            quitting: None,
            terminate,
            plugin_states: [None, None],
            clients: None,
            plugin_task: None,
            plugin_error: None,
            plugin_manual: Vec::new(),
            onboarded: false,
            prompts: VecDeque::new(),
            #[cfg(test)]
            test_url: None,
        }
    }

    /// The native menu for the platform's status item, built on first call.
    /// Call it only on the thread that runs the event loop: from then on
    /// every refresh brings it in line with the app's state.
    pub fn native_menu(&mut self) -> Menu {
        let items = &self.items;
        self.native
            .get_or_insert_with(|| NativeMenu::new(items))
            .menu
            .clone()
    }

    /// The Mac menu delegate maintains this check while AppKit tracks the menu.
    #[cfg(target_os = "macos")]
    pub fn native_login_item(&mut self) -> tray_icon::menu::CheckMenuItem {
        self.native.get_or_insert_with(|| NativeMenu::new(&self.items)).login.clone()
    }

    /// Bring the native menu, if there is one, in line with the model.
    fn sync_native(&mut self) {
        if let Some(native) = &mut self.native {
            native.sync(&self.items);
        }
    }

    /// Start native services once the platform's status item exists.
    pub fn start(&mut self) {
        if self.runtime.is_some() || self.startup_task.is_some() || self.quitting.is_some() {
            return;
        }
        if std::env::current_exe().is_ok_and(|p| paths::is_translocated(&p)) {
            self.problem = Some("Move Erlkonig to Applications, then open it again.".into());
        } else {
            let layout = self.layout.clone();
            let control = StartupControl::new(true);
            let worker = control.clone();
            match std::thread::Builder::new().name("erlkonig-startup".into())
                .spawn(move || Runtime::start_controlled(&layout, &worker).map_err(|error| format!("{error:#}")))
            {
                Ok(thread) => self.startup_task = Some(StartupTask { control, thread: Some(thread) }),
                Err(error) => self.problem = Some(format!("Cannot start local services: {error}")),
            }
        }
        self.update_notice.get_or_insert_with(crate::update_notice::UpdateNotice::new);
        self.next_refresh = Instant::now();
    }

    fn finish_startup_task(&mut self) {
        if !self.startup_task.as_ref().is_some_and(StartupTask::is_finished) {
            return;
        }
        let task = self.startup_task.take().expect("finished startup task");
        if self.quitting.is_some() {
            self.quitting = Some(Instant::now());
        }
        self.startup_prompt = None;
        self.prompts.retain(|prompt| prompt.then != Then::StartupChoice);
        match task.join() {
            Ok(Ok(runtime)) => {
                if self.quitting.is_some() { runtime.begin_shutdown(); }
                self.runtime = Some(runtime);
            }
            Ok(Err(error)) => self.problem = Some(error),
            Err(_) => self.problem = Some("Local storage startup did not complete. The original pile was retained; inspect the node locator before retrying.".into()),
        }
        if self.quitting.is_none() {
            self.check_plugins();
        }
    }

    fn offer_startup_choice(&mut self) {
        if self.startup_prompt.is_some() { return; }
        let Some(prompt) = self.startup_task.as_ref().and_then(|task| task.control.take_prompt()) else { return; };
        let title = prompt.title.clone();
        let text = prompt.details.clone();
        let labels = [prompt.approve_label.clone(), prompt.decline_label.clone()];
        let buttons = [labels[0].as_str(), labels[1].as_str()];
        self.startup_prompt = Some(prompt);
        match self.shell.startup_alert(&title, &text, &buttons, &self.terminate) {
            Some(answer) => self.follow(Then::StartupChoice, answer),
            None => {
                self.prompts.push_back(Prompt {
                    title, text, buttons: buttons.iter().map(|s| (*s).into()).collect(), then: Then::StartupChoice,
                });
                self.shell.show_window();
            }
        }
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
            Then::Connect(hosts) => {
                if answer == 0 {
                    self.change_plugins(install::Change::Install, hosts);
                }
            }
            Then::StartupChoice => {
                if let Some(prompt) = self.startup_prompt.take() {
                    prompt.answer(answer == 0 && self.quitting.is_none() && !self.terminate.load(Ordering::Relaxed));
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
            if let Some(task) = &self.startup_task { return task.control.detail(); }
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
        let notes = self
            .plugin_manual
            .iter()
            .map(|h| install::manual_config_note(*h));
        for line in self.plugin_error.iter().cloned().chain(notes) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&line);
        }
        text
    }

    /// One short prompt for a plugin failure; the client's own words go to
    /// the status details instead of the dialog.
    fn plugin_failed(&mut self, title: &str, sentence: &str, error: String) {
        self.plugin_error = Some(error);
        self.ask(title, sentence, &["OK"], Then::Nothing);
    }

    /// Worker cleanup must never open a modal: one raised before Quit can
    /// block a later Quit just as surely as one raised during shutdown.
    fn viewer_failed(&mut self, error: String) {
        let diagnostic = format!("Faculties viewer: {error}");
        eprintln!("Erlkonig: {diagnostic}");
        self.problem = Some(diagnostic);
        self.next_refresh = Instant::now();
    }

    /// The menu as it is right now, plus the oldest unanswered prompt.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn view(&self) -> View {
        let items = &self.items;
        View {
            attention: items.status_visible.then(|| items.status.clone()),
            details: self.details(),
            sync: items.pause_sync.clone(),
            retry: items.retry_visible.then(|| items.retry_services.clone()),
            integrations: items
                .rows
                .iter()
                .filter_map(|row| items.row(*row).cloned())
                .collect(),
            storage: items.storage_folder.clone(),
            browse_faculties: items.browse_faculties.clone(),
            login: items.login.clone(),
            quit: items.quit.clone(),
            prompt: self.prompts.front().cloned(),
        }
    }

    pub fn refresh(&mut self, event_loop: &ActiveEventLoop) {
        if self.tick() {
            event_loop.exit();
            return;
        }
        if self.open_faculties && self.quitting.is_none() {
            if let Some(window) = &self.faculties_browser {
                // A closed window can still be settling its worker. Keep the
                // request until step() reaps it; never create a second owner.
                if window.raise() { self.open_faculties = false; }
            } else if let Some(runtime) = &mut self.runtime {
                self.open_faculties = false;
                match crate::faculties_browser::BrowserWindow::open(event_loop, runtime, self.shell.faculties_wake()) {
                    Ok(window) => self.faculties_browser = Some(window),
                    // A spawn can also reap a prior failed worker. Keep that
                    // route passive too, rather than opening a Mac modal.
                    Err(error) => self.viewer_failed(error),
                }
            }
        }
    }

    /// Route only the Faculties window; platform status windows remain separate.
    pub fn faculties_window_event(&mut self, id: winit::window::WindowId, event: &winit::event::WindowEvent) -> bool {
        self.faculties_browser.as_mut().is_some_and(|window| window.on_event(id, event))
    }

    pub fn next_wake(&self) -> Instant {
        self.faculties_browser.as_ref().map_or(self.next_refresh, |window| window.next_wake(self.next_refresh))
    }

    /// One refresh step; true once the event loop should exit. The native
    /// menu, if any, shows the resulting state.
    pub fn tick(&mut self) -> bool {
        let exit = self.step();
        self.sync_native();
        exit
    }

    fn step(&mut self) -> bool {
        if self.terminate.load(Ordering::Relaxed) && self.quitting.is_none() {
            self.quit();
        }
        self.finish_startup_task();
        if let Some(runtime) = &mut self.runtime {
            if let Err(error) = runtime.reap_faculties() {
                self.viewer_failed(format!("{error:#}"));
            }
        }
        if self.faculties_browser.as_mut().is_some_and(|child| child.finished()) {
            self.faculties_browser = None;
        }
        self.items.browse_faculties.enabled = self.runtime.is_some() && self.quitting.is_none();
        if let Some(since) = self.quitting {
            let mut exit = false;
            if self.startup_task.is_none() && since.elapsed() >= Duration::from_secs(15) {
                // The person's Quit is final. A stuck admitted native call
                // cannot leave the app alive indefinitely. Like Force Quit,
                // this fallback cannot promise to flush an unfinished write.
                eprintln!(
                    "Erlkonig: 15-second shutdown grace elapsed; terminating this application process"
                );
                // Process termination is not a claim that native work drained.
                std::process::exit(1);
            }
            self.items.quit.label = if since.elapsed() < Duration::from_secs(5) {
                "Shutting down…"
            } else {
                "Finishing work…"
            }
            .into();
            if self.runtime.as_ref().is_none_or(Runtime::is_finished)
                && self.startup_task.is_none()
                && self.faculties_browser.is_none()
                && self.update_notice.as_ref().is_none_or(crate::update_notice::UpdateNotice::is_finished)
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
        if let Some(notice) = &mut self.update_notice {
            notice.poll();
            self.items.update_visible = notice.offered.is_some();
        }
        self.offer_startup_choice();
        if self.terminate.load(Ordering::Relaxed) {
            self.quit();
            return false;
        }
        if let Some(prompt) = self.runtime.as_ref().and_then(Runtime::take_protected_prompt) {
            let answer = self.shell.unlock_secret(&prompt, &self.terminate);
            if self.terminate.load(Ordering::Relaxed) { self.quit(); }
            prompt.answer(answer);
            if self.quitting.is_some() {
                self.next_refresh = Instant::now();
                return false;
            }
        }
        if let Some(prompt) = self.runtime.as_ref().and_then(Runtime::take_secret_prompt) {
            let answer = self.shell.enter_secret(&prompt, &self.terminate);
            // A signal may have arrived inside a native modal loop. Close the
            // bridge before sending its answer, so even a stale Save is discarded.
            if self.terminate.load(Ordering::Relaxed) {
                self.quit();
            }
            prompt.answer(answer);
            if self.quitting.is_some() {
                self.next_refresh = Instant::now();
                return false;
            }
        }
        if let Some(prompt) = self.runtime.as_ref().and_then(Runtime::take_import_prompt) {
            let answer = self.shell.import_1password(&prompt, &self.terminate);
            if self.terminate.load(Ordering::Relaxed) { self.quit(); }
            prompt.answer(answer);
            if self.quitting.is_some() {
                self.next_refresh = Instant::now();
                return false;
            }
        }
        self.finish_plugin_task();
        let status = self.runtime.as_ref().map(Runtime::status);
        let attention = if self.problem.is_some() {
            Some("Attention needed")
        } else if self.startup_task.is_some() {
            Some("Preparing local storage…")
        } else {
            status
                .as_ref()
                .and_then(|s| s.attention_label())
                .or(self.plugin_error.as_ref().map(|_| "Plugin problem"))
        };
        if let Some(label) = attention {
            self.items.status.label = label.into();
        }
        self.items.status_visible = attention.is_some();
        let retry_visible = status.as_ref().is_some_and(|s| {
            s.config_problem.is_none()
                && s.node.services == erlkonig_core::status::ServiceState::Stopped
        });
        let retry_pending = status.as_ref().is_some_and(|s| s.node.retry_pending);
        self.items.retry_services.label = if retry_pending {
            "Retry requested…"
        } else {
            "Retry services"
        }
        .into();
        self.items.retry_services.enabled = retry_visible && !retry_pending;
        self.items.retry_visible = retry_visible;
        self.items.storage_folder.enabled = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.binding().storage_folder())
            .is_some();
        self.items.pause_sync.enabled = status.as_ref().is_some_and(|s| s.config_problem.is_none());
        self.items.pause_sync.label = if status.as_ref().is_some_and(|s| s.node.sync_enabled) {
            "Pause sync"
        } else {
            "Resume sync"
        }
        .into();
        self.update_plugin_items();
        self.items.login.checked = Some(self.shell.displayed_login() != Login::Off);
        let now = self.origin.elapsed();
        crown::observe(
            &mut self.activity,
            self.startup_task.is_some() || status.as_ref().is_some_and(|s| s.active_work > 0),
            now,
        );
        self.shell.crown(self.activity, attention.is_some());
        self.next_refresh = Instant::now() + Duration::from_millis(250);
        false
    }

    pub fn quit(&mut self) {
        self.quitting.get_or_insert_with(Instant::now);
        self.open_faculties = false;
        if let Some(child) = &mut self.faculties_browser { child.stop(); }
        if let Some(notice) = &self.update_notice { notice.cancel(); }
        self.shell.stop_crown(); // Invalidate display callbacks before service teardown.
        // Quit is final: unanswered prompts no longer lead anywhere.
        self.prompts.clear();
        self.startup_prompt = None;
        if let Some(task) = &self.startup_task { task.control.cancel(); }
        if let Some(task) = &self.plugin_task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        let items = &mut self.items;
        for item in [
            &mut items.chatgpt,
            &mut items.claude,
            &mut items.openai_manual,
            &mut items.cowork,
            &mut items.refresh_plugins,
            &mut items.storage_folder,
            &mut items.browse_faculties,
            &mut items.quit,
            &mut items.get_update,
            &mut items.login,
            &mut items.status,
            &mut items.pause_sync,
            &mut items.retry_services,
            &mut items.integrations,
        ] {
            item.enabled = false;
        }
        items.retry_visible = false;
        if let Some(runtime) = &self.runtime {
            runtime.begin_shutdown();
        }
    }

    /// The platform can terminate without returning from its event loop
    /// (AppKit delivers LoopExiting synchronously from
    /// applicationWillTerminate), so drain startup here before applying the
    /// existing service-shutdown deadline.
    pub fn finish_termination(&mut self) {
        // AppKit can bypass quit(); cancel and join the advisory request here too.
        self.update_notice = None;
        self.shell.stop_crown();
        if let Some(child) = &mut self.faculties_browser { child.stop(); }
        if let Some(task) = &self.startup_task { task.control.cancel(); }
        self.startup_prompt = None;
        // A converter may be finishing one native blob/branch operation.
        // Keep ownership and node.lock until it reaches its cancellation
        // checkpoint; never detach it or activate a partially checked output.
        while self.startup_task.as_ref().is_some_and(|task| !task.is_finished()) {
            std::thread::sleep(Duration::from_millis(25));
        }
        self.quitting.get_or_insert_with(Instant::now);
        self.finish_startup_task();
        if let Some(task) = &self.plugin_task {
            task.cancel.store(true, Ordering::Relaxed);
        }
        // AppKit may exit without going through quit(). Stop admission before
        // waiting for any child cleanup, even if that cleanup cannot finish.
        if let Some(runtime) = &self.runtime {
            runtime.begin_shutdown();
        }
        let since = self.quitting.unwrap_or_else(Instant::now);
        while self.faculties_browser.as_mut().is_some_and(|child| !child.finished()) {
            if since.elapsed() >= Duration::from_secs(15) {
                // Runtime still owns the worker; only process termination can
                // abandon an uncooperative admitted native operation.
                std::process::exit(1);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.faculties_browser = None;
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
            let manual = hosts
                .into_iter()
                .filter(|h| install::manual_config(*h))
                .collect();
            PluginResult::Inventory {
                clients,
                states,
                manual,
            }
        });
        if self.plugin_task.is_none() {
            self.rebuild_integrations();
        }
    }

    fn rebuild_integrations(&mut self) {
        let mut rows = Vec::new();
        match &self.clients {
            Some(clients) => {
                if clients.openai_label().is_some() {
                    if clients.openai_cli.is_some() {
                        rows.push(Row::ChatGpt);
                    } else {
                        rows.push(Row::OpenAiManual);
                    }
                }
                if clients.claude_cli.is_some() {
                    rows.push(Row::Claude);
                }
                if clients.claude_desktop {
                    rows.push(Row::Cowork);
                }
                if !clients.any() {
                    self.items.no_clients.label = "No supported clients detected".into();
                    rows.push(Row::NoClients);
                }
            }
            None => {
                self.items.no_clients.label = if self.plugin_task.is_some() {
                    "Checking clients…"
                } else {
                    "Client detection unavailable"
                }
                .into();
                rows.push(Row::NoClients);
            }
        }
        rows.extend([Row::Separator, Row::Refresh]);
        self.items.rows = rows;
    }

    fn update_plugin_items(&mut self) {
        for host in [install::Host::OpenAi, install::Host::ClaudeCode] {
            let state = self.plugin_states[host_index(host)].as_ref();
            let (checked, actionable, suffix) = install::inventory_menu_state(state);
            let label = (host == install::Host::OpenAi)
                .then(|| {
                    self.clients
                        .as_ref()
                        .and_then(|clients| clients.openai_label())
                })
                .flatten()
                .unwrap_or_else(|| host.label().into());
            let item = match host {
                install::Host::OpenAi => &mut self.items.chatgpt,
                install::Host::ClaudeCode => &mut self.items.claude,
            };
            // On an error retain the last-known check, disable the control and
            // explicitly mark uncertainty instead of claiming uninstallation.
            if let Some(checked) = checked {
                item.checked = Some(checked);
            }
            item.label = format!("{label} · {suffix}");
            item.enabled = self.plugin_task.is_none() && actionable;
        }
        if let Some(label) = self
            .clients
            .as_ref()
            .and_then(|clients| clients.openai_label())
        {
            self.items.openai_manual.label = format!("{label} · CLI unavailable…");
        }
        let idle = self.plugin_task.is_none();
        self.items.openai_manual.enabled = idle;
        self.items.refresh_plugins.label = if idle {
            "Refresh plugin status…"
        } else {
            "Checking / updating plugins…"
        }
        .into();
        self.items.refresh_plugins.enabled = idle;
        self.items.cowork.enabled = idle;
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
                manual,
            }) => {
                self.clients = Some(clients);
                if states.iter().flatten().all(Result::is_ok) {
                    self.plugin_error = None;
                }
                self.plugin_states = states;
                self.plugin_manual = manual;
                self.rebuild_integrations();
                if !self.onboarded {
                    self.onboarded = true;
                    self.offer_connection();
                }
                self.refresh_stale_plugins();
            }
            // Success is the check mark itself; only a failure speaks, once,
            // however many clients failed.
            Ok(PluginResult::Changed(change, results)) => {
                let (mut clients, mut errors) = (Vec::new(), Vec::new());
                for (host, result) in results {
                    if let Err(error) = &result {
                        clients.push(host.client());
                        errors.push(format!("{}: {error}", host.label()));
                    }
                    self.plugin_states[host_index(host)] = Some(result);
                }
                if errors.is_empty() {
                    self.plugin_error = None;
                } else {
                    let (title, verb) = match change {
                        install::Change::Install => ("Plugin not installed", "install"),
                        install::Change::Remove => ("Plugin not removed", "remove"),
                        install::Change::Refresh => ("Plugin not updated", "update"),
                    };
                    self.plugin_failed(
                        title,
                        &format!(
                            "{} did not {verb} the Erlkonig plugin; the details are in Erlkonig's status.",
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
        // The platform may already have toggled the native check mark. The
        // decision comes from the last inventory, never from that mark, and
        // the next sync restores the mark to the model until the client has
        // confirmed the change.
        let Some(Ok(state)) = self.plugin_states[host_index(host)].clone() else {
            return;
        };
        if self.clients.as_ref().and_then(|c| c.cli(host)).is_none() {
            return;
        }
        if state.account_managed && !state.installed {
            self.manage_cowork();
            return;
        }
        let change = if state.installed {
            install::Change::Remove
        } else {
            install::Change::Install
        };
        self.change_plugins(change, vec![host]);
    }

    /// The one path that installs or removes plugins at a person's request:
    /// a check mark, or Install in the onboarding prompt.
    fn change_plugins(&mut self, change: install::Change, hosts: Vec<install::Host>) {
        if self.plugin_task.is_some() || self.quitting.is_some() {
            return;
        }
        let targets = hosts
            .into_iter()
            .filter_map(|host| {
                let clients = self.clients.as_ref()?;
                Some((host, clients.cli(host)?.to_path_buf()))
            })
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        let url = self.mcp_url();
        if change == install::Change::Install && url.is_none() {
            self.plugin_failed(
                "Plugin not installed",
                "Erlkonig's services are not running, so the plugin would have nothing to connect to.",
                "Plugins: services are not running".into(),
            );
            return;
        }
        // Removal never stages a command and must still work after the image moves.
        let executable = match if change == install::Change::Remove {
            Ok(PathBuf::new())
        } else {
            paths::launcher_executable()
        } {
            Ok(path) => path,
            Err(error) => {
                self.plugin_failed(
                    "Plugin not changed",
                    "Erlkonig cannot locate its own program file; the details are in its status.",
                    format!("Plugins: {error}"),
                );
                return;
            }
        };
        let layout = self.layout.clone();
        self.start_plugin_task(move |cancel| {
            let results = targets
                .iter()
                .map(|(host, program)| {
                    let result = install::change_with_client(
                        *host,
                        program,
                        change,
                        &layout,
                        &executable,
                        url.as_deref().unwrap_or(""),
                        &cancel,
                    );
                    (*host, result)
                })
                .collect();
            PluginResult::Changed(change, results)
        });
    }

    /// Once per run, after the first inventory: when a client could be
    /// connected and nothing is (no plugin, no account plugin, no manual MCP
    /// server), ask. The answer is not stored; a run without any integration
    /// has no point, so the next run asks again while nothing is connected.
    fn offer_connection(&mut self) {
        if self.plugin_task.is_some() || self.quitting.is_some() {
            return;
        }
        let connected = !self.plugin_manual.is_empty()
            || self
                .plugin_states
                .iter()
                .flatten()
                .any(|state| matches!(state, Ok(s) if s.installed || s.account_managed));
        if connected {
            return;
        }
        let hosts = [install::Host::ClaudeCode, install::Host::OpenAi]
            .into_iter()
            .filter(|host| {
                matches!(self.plugin_states[host_index(*host)], Some(Ok(_)))
                    && self
                        .clients
                        .as_ref()
                        .is_some_and(|c| c.cli(*host).is_some())
            })
            .collect::<Vec<_>>();
        if hosts.is_empty() {
            return;
        }
        let names = hosts.iter().map(|h| h.client()).collect::<Vec<_>>();
        self.ask(
            "Integrate Erlkonig",
            &names.join(" and "),
            &["Install", "Not now"],
            Then::Connect(hosts),
        );
    }

    /// After an inventory: reinstall, quietly, each plugin the clients report
    /// installed whose last confirmed package was made for another executable
    /// path, data directory, endpoint or package version. A client whose
    /// reinstall failed is retried next time. Unchecked plugins are never
    /// touched; the worker re-reads each inventory before acting.
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
        let (Some(url), Ok(executable)) = (self.mcp_url(), paths::launcher_executable()) else {
            return;
        };
        let layout = self.layout.clone();
        self.start_plugin_task(move |cancel| {
            PluginResult::Changed(
                install::Change::Refresh,
                install::refresh_if_stale(&installed, &layout, &executable, &url, &cancel),
            )
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
        let executable = match paths::launcher_executable() {
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
        // A check item may have toggled itself natively before this event
        // arrived: the next sync re-applies the whole model.
        if let Some(native) = &mut self.native {
            native.invalidate();
        }
        let id = event.id();
        if id == &self.items.quit.id {
            self.quit();
        } else if id == &self.items.get_update.id {
            if self.quitting.is_none() && self.items.update_visible {
                self.shell.open_url(erlkonig_core::update_catalog::DOWNLOAD_URL);
            }
        } else if id == &self.items.pause_sync.id {
            self.toggle_sync();
        } else if id == &self.items.retry_services.id {
            if let Some(Err(error)) = self.runtime.as_ref().map(Runtime::retry_services) {
                self.ask(
                    "Services not retried",
                    &error.to_string(),
                    &["OK"],
                    Then::Nothing,
                );
            }
        } else if id == &self.items.browse_faculties.id {
            if self.quitting.is_none() && self.runtime.is_some() {
                self.open_faculties = true;
            }
        } else if id == &self.items.storage_folder.id {
            self.show_storage_folder();
        } else if id == &self.items.chatgpt.id {
            self.plugin_action(install::Host::OpenAi);
        } else if id == &self.items.openai_manual.id {
            self.ask(
                "Codex CLI not found",
                "Erlkonig installs this plugin with the Codex CLI. Install it, then choose Refresh plugin status.",
                &["OK"],
                Then::Nothing,
            );
        } else if id == &self.items.claude.id {
            self.plugin_action(install::Host::ClaudeCode);
        } else if id == &self.items.cowork.id {
            self.manage_cowork();
        } else if id == &self.items.refresh_plugins.id {
            self.check_plugins();
        } else if id == &self.items.login.id {
            self.toggle_login();
        } else if id == &self.items.status.id {
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
        login_reads: std::cell::Cell<usize>,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Shell for Fake {
        fn faculties_wake(&self) -> Box<dyn Fn() + Send + Sync> { Box::new(|| {}) }
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
            self.login_reads.set(self.login_reads.get() + 1);
            self.login.unwrap_or(Login::Off)
        }
        fn displayed_login(&self) -> Login {
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
    fn viewer_panic_reporting_is_passive_before_and_during_quit() {
        for already_quitting in [false, true] {
            // The fake records both modal alerts and nonmodal window prompts.
            let mut app = app(Fake::default());
            let log = app.shell.log.clone();
            if already_quitting { app.quit(); }
            app.viewer_failed("Faculties viewer worker panicked".into());
            assert!(app.details().contains("Faculties viewer worker panicked"));
            assert!(app.prompts.is_empty());
            if !already_quitting {
                assert!(!app.tick());
                assert!(app.items.status_visible);
                assert_eq!(app.items.status.label, "Attention needed");
                app.quit();
            }
            assert!(app.tick(), "a worker diagnostic never waits for acknowledgement before Quit");
            assert!(app.prompts.is_empty());
            assert!(!log.borrow().iter().any(|line| line.starts_with("alert ") || line == "window"));
        }
    }

    #[test]
    fn update_is_only_a_passive_menu_command_to_the_fixed_download_page() {
        let mut app = app(Fake::default());
        let log = app.shell.log.clone();
        let click = || MenuEvent { id: MenuId::new("erlkonig.get-update") };
        assert!(!app.items.update_visible);
        app.on_menu(click());
        assert!(log.borrow().is_empty());
        let mut notice = crate::update_notice::UpdateNotice::new();
        notice.offered = Some("0.2.0".into());
        app.update_notice = Some(notice);
        app.tick();
        assert!(app.items.update_visible);
        assert_eq!(app.items.get_update.label, "Get update…");
        assert!(app.prompts.is_empty());
        assert!(!log.borrow().iter().any(|line| line.starts_with("alert ") || line == "window"));
        log.borrow_mut().clear();
        app.on_menu(click());
        assert_eq!(&*log.borrow(), &["open https://erlkonig.ai/#downloads"]);
        app.quit();
        log.borrow_mut().clear();
        app.on_menu(click());
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn startup_progress_stays_responsive_and_quit_joins_the_worker() {
        let mut app = app(Fake::default());
        let control = StartupControl::new(true);
        let worker_control = control.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            wait.recv().unwrap(); // One deliberately nonpreemptible operation.
            assert!(worker_control.is_cancelled());
            Err("startup cancelled".into())
        });
        app.startup_task = Some(StartupTask { control, thread: Some(thread) });
        assert!(!app.tick());
        assert!(app.view().details.contains("Inspecting local storage"));
        assert_eq!(app.view().attention.unwrap().label, "Preparing local storage…");
        app.quit();
        app.quitting = Some(Instant::now() - Duration::from_secs(16));
        assert!(!app.tick(), "Quit retains the in-flight startup owner past the ordinary service deadline");
        // The normal service deadline begins after this startup task drains.
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.tick() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.startup_task.is_none());
        assert!(app.runtime.is_none());
    }

    #[test]
    fn dropping_the_app_cancels_and_joins_startup() {
        let mut app = app(Fake::default());
        let control = StartupControl::new(true);
        let worker_control = control.clone();
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = done.clone();
        let thread = std::thread::spawn(move || {
            while !worker_control.is_cancelled() { std::thread::sleep(Duration::from_millis(1)); }
            worker_done.store(true, Ordering::Relaxed);
            Err("startup cancelled".into())
        });
        app.startup_task = Some(StartupTask { control, thread: Some(thread) });
        drop(app);
        assert!(done.load(Ordering::Relaxed));
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
    fn faculties_browser_requires_the_existing_runtime() {
        let mut app = app(Fake::default());
        assert!(!app.view().browse_faculties.enabled);
        assert_eq!(app.view().browse_faculties.label, "View data");
        app.on_menu(MenuEvent { id: app.items.browse_faculties.id.clone() });
        assert!(!app.open_faculties);
        assert!(app.faculties_browser.is_none());
        app.quit();
        app.on_menu(MenuEvent { id: app.items.browse_faculties.id.clone() });
        assert!(!app.open_faculties);
        assert!(!app.view().browse_faculties.enabled);
        assert_eq!(app.view().browse_faculties.label, "View data");
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
    fn viewer_wakes_use_displayed_login_and_activation_checks_the_service() {
        let mut app = app(Fake::default());
        for _ in 0..8 {
            app.next_refresh = Instant::now();
            assert!(!app.tick()); // due status refresh
            assert!(!app.tick()); // earlier repaint wake
        }
        assert_eq!(app.shell.login_reads.get(), 0);
        assert_eq!(app.view().login.checked, Some(false));

        app.shell.login = Some(Login::On);
        app.next_refresh = Instant::now();
        assert!(!app.tick());
        assert_eq!(app.view().login.checked, Some(true));
        assert_eq!(app.shell.login_reads.get(), 0);

        app.shell.login = Some(Login::NeedsApproval); // changed while open
        app.on_menu(MenuEvent { id: app.view().login.id });
        assert_eq!(app.shell.login_reads.get(), 1);
        assert_eq!(app.view().prompt.unwrap().buttons, ["Open Settings", "Cancel"]);
        app.answer(0);
        assert!(app.shell.log.borrow().iter().any(|line| line == "settings"));
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
    /// AppKit refuses native menu objects off its main thread, and libtest
    /// runs each test on a worker: the app's state never needs one.
    #[test]
    fn the_app_runs_on_any_thread_without_native_menu_objects() {
        std::thread::spawn(|| {
            let log = Rc::new(RefCell::new(Vec::new()));
            let mut app = app(Fake {
                login: Some(Login::On),
                log: log.clone(),
                ..Fake::default()
            });
            app.problem = Some("Cannot create menu: no host".into());
            assert!(!app.tick());
            let view = app.view();
            app.on_menu(MenuEvent { id: view.login.id });
            let attention = view.attention.expect("attention shown");
            app.on_menu(MenuEvent { id: attention.id });
            app.answer(0);
            let clients = install::ClientAvailability {
                claude_cli: Some("/nonexistent/claude".into()),
                ..Default::default()
            };
            report(&mut app, clients, [None, Some(Ok(absent()))], Vec::new());
            app.answer(1);
            app.on_menu(MenuEvent { id: view.quit.id });
            assert!(app.tick());
            assert!(app.native.is_none(), "no native menu was created");
            assert_eq!(alerts(&log), ["alert Erlkonig", "alert Integrate Erlkonig"]);
            assert!(log.borrow().contains(&"login false".to_owned()));
        })
        .join()
        .expect("the scenario runs on a worker thread");
    }

    #[cfg(target_os = "linux")]
    type NativeRow = (String, bool, Option<bool>);

    /// The native entries as (label, enabled, checked); "—" is a separator.
    #[cfg(target_os = "linux")]
    fn native_rows(items: &[tray_icon::menu::MenuItemKind]) -> Vec<NativeRow> {
        use tray_icon::menu::MenuItemKind as Kind;
        items
            .iter()
            .map(|item| match item {
                Kind::MenuItem(i) => (i.text(), i.is_enabled(), None),
                Kind::Check(i) => (i.text(), i.is_enabled(), Some(i.is_checked())),
                Kind::Submenu(i) => (i.text(), i.is_enabled(), None),
                Kind::Predefined(_) => ("—".into(), true, None),
                Kind::Icon(i) => (i.text(), i.is_enabled(), None),
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    fn row(label: &str, enabled: bool, checked: Option<bool>) -> NativeRow {
        (label.into(), enabled, checked)
    }

    #[cfg(target_os = "linux")]
    fn integrations_submenu(menu: &Menu) -> tray_icon::menu::Submenu {
        menu.items()
            .into_iter()
            .find_map(|item| match item {
                tray_icon::menu::MenuItemKind::Submenu(submenu) => Some(submenu),
                _ => None,
            })
            .expect("integrations submenu")
    }

    /// The native menu is built on request and follows the model on every
    /// refresh, including a check mark the platform toggled on a click.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_native_menu_is_a_projection_of_the_model() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = app(Fake {
            log: log.clone(),
            ..Fake::default()
        });
        assert!(app.native.is_none());
        let menu = app.native_menu();
        let sep = || row("—", true, None);
        assert_eq!(
            native_rows(&menu.items()),
            [
                row("Pause sync", false, None),
                sep(),
                row("Integrations", true, None),
                row("View data", false, None),
                row("Show storage folder", false, None),
                sep(),
                row("Open at login", true, Some(false)),
                row("Quit Erlkonig", true, None),
            ]
        );
        assert_eq!(
            native_rows(&integrations_submenu(&menu).items()),
            [
                row("Checking clients…", false, None),
                row("Refresh plugin status…", false, None),
            ]
        );

        // Text, enabled and checked follow the model on the next refresh.
        app.problem = Some("no services".into());
        app.shell.login = Some(Login::On);
        assert!(!app.tick());
        let top = native_rows(&menu.items());
        assert_eq!(top[0], row("Attention needed", true, None));
        assert_eq!(top[1], row("Resume sync", false, None));
        assert_eq!(top[7], row("Open at login", true, Some(true)));
        assert_eq!(
            native_rows(&integrations_submenu(&menu).items())[1],
            row("Refresh plugin status…", true, None)
        );

        // A click toggles the native mark before its event arrives. The
        // intent comes from the observed state, and the mark is restored.
        let login = menu
            .items()
            .into_iter()
            .find_map(|item| match item {
                tray_icon::menu::MenuItemKind::Check(item) => Some(item),
                _ => None,
            })
            .expect("login item");
        assert_eq!(login.id(), &app.view().login.id);
        login.set_checked(false);
        app.on_menu(MenuEvent {
            id: login.id().clone(),
        });
        assert!(!app.tick());
        let logins = log
            .borrow()
            .iter()
            .filter(|line| line.starts_with("login "))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(logins, ["login false"]);
        assert!(
            login.is_checked(),
            "the mark shows the observed state again"
        );

        // Integration entries appear and disappear with the clients.
        app.clients = Some(install::ClientAvailability {
            claude_cli: Some("/nonexistent/claude".into()),
            claude_desktop: true,
            ..Default::default()
        });
        app.plugin_states[1] = Some(Ok(install::Inventory {
            installed: true,
            enabled: true,
            ..absent()
        }));
        app.rebuild_integrations();
        app.update_plugin_items();
        app.sync_native();
        let claude = &app.items.claude;
        assert!(claude.label.starts_with("Claude Code plugin · "));
        assert_eq!(
            native_rows(&integrations_submenu(&menu).items()),
            [
                row(&claude.label, claude.enabled, Some(true)),
                row("Manage Cowork plugin…", true, None),
                sep(),
                row("Refresh plugin status…", true, None),
            ]
        );
        app.clients = Some(Default::default());
        app.rebuild_integrations();
        app.sync_native();
        assert_eq!(
            native_rows(&integrations_submenu(&menu).items()),
            [
                row("No supported clients detected", false, None),
                sep(),
                row("Refresh plugin status…", true, None),
            ]
        );

        // The attention and retry items come and go in their places.
        app.items.retry_visible = true;
        app.items.retry_services.enabled = true;
        app.sync_native();
        assert_eq!(
            native_rows(&menu.items())[..3],
            [
                row("Attention needed", true, None),
                row("Resume sync", false, None),
                row("Retry services", true, None),
            ]
        );
        app.items.status_visible = false;
        app.sync_native();
        assert_eq!(
            native_rows(&menu.items())[..2],
            [
                row("Resume sync", false, None),
                row("Retry services", true, None),
            ]
        );
        app.items.status_visible = true;
        app.sync_native();
        assert_eq!(native_rows(&menu.items())[0].0, "Attention needed");
        assert_eq!(native_rows(&menu.items())[2].0, "Retry services");

        // Quit relabels Quit and disables every entry, retry included.
        app.on_menu(MenuEvent {
            id: app.items.quit.id.clone(),
        });
        assert!(app.tick());
        let top = native_rows(&menu.items());
        assert_eq!(top.last(), Some(&row("Shutting down…", false, None)));
        assert!(!top.iter().any(|(label, _, _)| label == "Retry services"));
        assert!(
            top.iter()
                .filter(|(label, _, _)| label != "—")
                .all(|(_, enabled, _)| !enabled),
            "{top:?}"
        );
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
        // Tests run in parallel: a child another thread forked while this
        // script was open for writing makes exec fail with "text file busy"
        // until that child execs. Wait that out once, then forget the probe.
        for _ in 0..500 {
            match std::process::Command::new(&program).output() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                _ => break,
            }
        }
        std::fs::write(dir.join("argv.log"), "").unwrap();
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
        assert_eq!(
            checked(&app.items.claude),
            installed,
            "{:?}",
            app.plugin_states[1]
        );
        (app, claude)
    }

    /// A check item's mark in the model.
    fn checked(control: &Control) -> bool {
        control.checked.expect("a check item")
    }

    fn click_claude(app: &mut App<Fake>) {
        let id = app.items.claude.id.clone();
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
        assert!(checked(&app.items.claude));
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
        assert!(!checked(&app.items.claude));
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
        assert!(checked(&app.items.claude));
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
        std::fs::write(claude.with_file_name("fail"), "1\n").unwrap();
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
        assert!(!checked(&app.items.claude));
    }

    type States = [Option<Result<install::Inventory, String>>; 2];

    /// The worker's inventory report; `manual` lists the clients with a
    /// separate MCP server named erlkonig in their settings.
    fn inventory_result(
        clients: install::ClientAvailability,
        states: States,
        manual: Vec<install::Host>,
    ) -> PluginResult {
        PluginResult::Inventory {
            clients,
            states,
            manual,
        }
    }

    /// A canned inventory report, run through the worker.
    fn report(
        app: &mut App<Fake>,
        clients: install::ClientAvailability,
        states: States,
        manual: Vec<install::Host>,
    ) {
        app.start_plugin_task(move |_| inventory_result(clients, states, manual));
        settle(app);
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
            inventory_result(clients, states, Vec::new())
        });
        settle(app);
    }

    fn absent() -> install::Inventory {
        install::Inventory {
            installed: false,
            enabled: false,
            account_managed: false,
            load_errors: false,
        }
    }

    fn data_entries(layout: &Layout) -> Vec<PathBuf> {
        let mut entries = std::fs::read_dir(&layout.dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    /// An app with two fake clients that report nothing installed.
    fn unconnected(temp: &Path, log: &Rc<RefCell<Vec<String>>>) -> (App<Fake>, PathBuf, PathBuf) {
        let layout = Layout::new(temp.join("data"));
        std::fs::create_dir_all(&layout.dir).unwrap();
        let claude = fake_client(&temp.join("claude"), "claude");
        let codex = fake_client(&temp.join("codex"), "codex");
        let mut app = App::new(
            Fake {
                log: log.clone(),
                ..Fake::default()
            },
            layout,
            Arc::new(AtomicBool::new(false)),
        );
        app.test_url = Some(URL.into());
        (app, claude, codex)
    }

    #[test]
    fn onboarding_asks_once_when_a_client_is_detected_and_nothing_is_connected() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut app, claude, codex) = unconnected(temp.path(), &log);
        inventory(&mut app, claude.clone(), codex.clone());
        let prompt = app.view().prompt.expect("onboarding prompt");
        assert_eq!(prompt.title, "Integrate Erlkonig");
        assert_eq!(prompt.text, "Claude Code and Codex");
        assert_eq!(prompt.buttons, ["Install", "Not now"]);
        app.answer(1);
        // A second inventory in the same run does not ask again.
        inventory(&mut app, claude, codex);
        assert_eq!(app.view().prompt, None);
        assert_eq!(alerts(&log), ["alert Integrate Erlkonig"]);
    }

    #[test]
    fn onboarding_names_only_the_detected_clients() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = app(Fake {
            log: log.clone(),
            ..Fake::default()
        });
        let clients = install::ClientAvailability {
            openai_app: Some("ChatGPT".into()),
            claude_desktop: true,
            claude_cli: Some("/nonexistent/claude".into()),
            ..Default::default()
        };
        report(&mut app, clients, [None, Some(Ok(absent()))], Vec::new());
        let prompt = app.view().prompt.expect("onboarding prompt");
        assert_eq!(prompt.text, "Claude Code");
    }

    #[test]
    fn onboarding_does_not_ask_when_anything_is_connected() {
        let installed = install::Inventory {
            installed: true,
            enabled: true,
            ..absent()
        };
        let account = install::Inventory {
            account_managed: true,
            ..absent()
        };
        for (states, manual) in [
            ([Some(Ok(absent())), Some(Ok(installed))], vec![]),
            ([Some(Ok(absent())), Some(Ok(account))], vec![]),
            (
                [Some(Ok(absent())), Some(Ok(absent()))],
                vec![install::Host::ClaudeCode],
            ),
        ] {
            let log = Rc::new(RefCell::new(Vec::new()));
            let mut app = app(Fake {
                log: log.clone(),
                ..Fake::default()
            });
            let clients = install::ClientAvailability {
                openai_cli: Some("/nonexistent/codex".into()),
                claude_cli: Some("/nonexistent/claude".into()),
                ..Default::default()
            };
            report(&mut app, clients, states, manual);
            assert_eq!(app.view().prompt, None);
            assert_eq!(alerts(&log), Vec::<String>::new());
        }
    }

    #[test]
    fn onboarding_does_not_ask_without_a_manageable_client() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut app = app(Fake {
            log: log.clone(),
            ..Fake::default()
        });
        // Desktop apps alone cannot be connected by a click.
        let clients = install::ClientAvailability {
            openai_app: Some("ChatGPT".into()),
            claude_desktop: true,
            ..Default::default()
        };
        report(&mut app, clients, [None, None], Vec::new());
        report(&mut app, Default::default(), [None, None], Vec::new());
        assert_eq!(app.view().prompt, None);
        assert_eq!(alerts(&log), Vec::<String>::new());
    }

    #[test]
    fn onboarding_install_installs_exactly_the_detected_clients() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut app, claude, codex) = unconnected(temp.path(), &log);
        inventory(&mut app, claude.clone(), codex.clone());
        take_argv(&claude);
        take_argv(&codex);
        app.answer(0);
        settle(&mut app);
        let market = install::marketplace_root(&app.layout);
        let market = market.to_str().unwrap();
        let markets = argv(&["plugin", "marketplace", "list", "--json"]);
        let claude_list = argv(&["plugin", "list", "--json"]);
        assert_eq!(
            take_argv(&claude),
            [
                claude_list.clone(),
                markets.clone(),
                argv(&["plugin", "marketplace", "add", market]),
                markets.clone(),
                argv(&["plugin", "install", install::PLUGIN_ID, "--scope", "user"]),
                claude_list,
            ]
        );
        let codex_list = argv(&[
            "--disable",
            "remote_plugin",
            "plugin",
            "list",
            "--marketplace",
            "erlkonig-local",
            "--json",
        ]);
        assert_eq!(
            take_argv(&codex),
            [
                codex_list.clone(),
                markets.clone(),
                argv(&["plugin", "marketplace", "add", market, "--json"]),
                markets,
                argv(&["plugin", "add", install::PLUGIN_ID, "--json"]),
                codex_list,
            ]
        );
        assert!(checked(&app.items.claude) && checked(&app.items.chatgpt));
        assert_eq!(alerts(&log), ["alert Integrate Erlkonig"]);
        assert_eq!(app.view().prompt, None);
    }

    #[test]
    fn onboarding_not_now_does_nothing_and_asks_again_next_run() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut app, claude, codex) = unconnected(temp.path(), &log);
        let layout = app.layout.clone();
        let before = data_entries(&layout);
        inventory(&mut app, claude.clone(), codex.clone());
        take_argv(&claude);
        take_argv(&codex);
        app.answer(1);
        settle(&mut app);
        assert!(take_argv(&claude).is_empty() && take_argv(&codex).is_empty());
        assert_eq!(data_entries(&layout), before, "nothing is stored");
        assert!(!checked(&app.items.claude));
        // The next run asks again while nothing is connected.
        let (mut next, _, _) = unconnected(temp.path(), &log);
        inventory(&mut next, claude, codex);
        assert_eq!(
            next.view().prompt.map(|p| p.title),
            Some("Integrate Erlkonig".into())
        );
        assert_eq!(
            alerts(&log),
            ["alert Integrate Erlkonig", "alert Integrate Erlkonig"]
        );
    }

    #[test]
    fn a_failed_quiet_refresh_is_retried_on_the_next_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let log = Rc::new(RefCell::new(Vec::new()));
        let layout = Layout::new(temp.path().join("data"));
        std::fs::create_dir_all(&layout.dir).unwrap();
        install::prepare(&layout, Path::new("/old/Erlkonig.app/erlkonig"), URL).unwrap();
        let claude = client(&temp.path().join("claude"), "claude", &layout, true);
        let codex = client(&temp.path().join("codex"), "codex", &layout, true);
        std::fs::write(codex.with_file_name("fail"), "1\n").unwrap();
        let mut app = App::new(
            Fake {
                log: log.clone(),
                ..Fake::default()
            },
            layout,
            Arc::new(AtomicBool::new(false)),
        );
        app.test_url = Some(URL.into());
        inventory(&mut app, claude.clone(), codex.clone());
        assert_eq!(alerts(&log), ["alert Plugin not updated"]);
        app.answer(0);
        take_argv(&claude);
        take_argv(&codex);
        // Codex works again: the next inventory retries Codex alone.
        std::fs::write(codex.with_file_name("fail"), "0\n").unwrap();
        inventory(&mut app, claude.clone(), codex.clone());
        let calls = take_argv(&codex);
        let reinstall = [
            argv(&["plugin", "remove", install::PLUGIN_ID, "--json"]),
            argv(&["plugin", "add", install::PLUGIN_ID, "--json"]),
        ];
        assert!(calls.windows(2).any(|w| w == reinstall), "{calls:?}");
        let calls = take_argv(&claude);
        assert!(
            calls.iter().all(|call| call.contains(&"list".to_owned())),
            "{calls:?}"
        );
        assert_eq!(alerts(&log), ["alert Plugin not updated"]);
        assert!(checked(&app.items.chatgpt) && checked(&app.items.claude));
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
        assert!(checked(&app.items.claude));
        assert!(!checked(&app.items.chatgpt));
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
        // Installed by this very executable, for this endpoint.
        let exe = std::env::current_exe().unwrap();
        let claude = client(&temp.path().join("claude"), "claude", &layout, false);
        let codex = client(&temp.path().join("codex"), "codex", &layout, false);
        for (host, program) in [
            (install::Host::ClaudeCode, &claude),
            (install::Host::OpenAi, &codex),
        ] {
            let cancel = AtomicBool::new(false);
            install::set_installed_with_client(host, program, true, &layout, &exe, URL, &cancel)
                .unwrap();
            take_argv(program);
        }
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
        assert!(checked(&app.items.claude) && checked(&app.items.chatgpt));
    }
}
