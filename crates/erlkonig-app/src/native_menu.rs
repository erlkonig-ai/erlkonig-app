//! The native menu: a projection of the plain menu model in [`Items`].
//!
//! AppKit objects may only be created and touched on the main thread, so
//! nothing here exists until a shell asks for it on its event-loop thread
//! (`App::native_menu`). Afterwards every refresh brings the native items in
//! line with the model; the native items are never read back as state.

use crate::app::{Control, Items, Row};
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

pub(crate) struct NativeMenu {
    pub(crate) menu: Menu,
    status: MenuItem,
    integrations: Submenu,
    no_clients: MenuItem,
    chatgpt: CheckMenuItem,
    openai_manual: MenuItem,
    claude: CheckMenuItem,
    cowork: MenuItem,
    refresh_plugins: MenuItem,
    login: CheckMenuItem,
    pause_sync: MenuItem,
    retry_services: MenuItem,
    storage_folder: MenuItem,
    quit: MenuItem,
    status_shown: bool,
    retry_shown: bool,
    /// The rows the Integrations submenu holds now.
    rows: Vec<Row>,
    /// The model the native items show, or `None` when a click may have
    /// changed them natively (a check item toggles itself before its event).
    applied: Option<Items>,
}

fn item(control: &Control) -> MenuItem {
    MenuItem::with_id(control.id.clone(), &control.label, control.enabled, None)
}

fn check_item(control: &Control) -> CheckMenuItem {
    CheckMenuItem::with_id(
        control.id.clone(),
        &control.label,
        control.enabled,
        control.checked == Some(true),
        None,
    )
}

fn sync_item(item: &MenuItem, now: &Control, before: Option<&Control>) {
    if before.is_none_or(|b| b.label != now.label) {
        item.set_text(&now.label);
    }
    if before.is_none_or(|b| b.enabled != now.enabled) {
        item.set_enabled(now.enabled);
    }
}

fn sync_check(item: &CheckMenuItem, now: &Control, before: Option<&Control>) {
    if before.is_none_or(|b| b.label != now.label) {
        item.set_text(&now.label);
    }
    if before.is_none_or(|b| b.enabled != now.enabled) {
        item.set_enabled(now.enabled);
    }
    if before.is_none_or(|b| b.checked != now.checked) {
        item.set_checked(now.checked == Some(true));
    }
}

impl NativeMenu {
    /// Build the native menu showing `items`. Call only on the thread that
    /// runs the platform's event loop.
    pub(crate) fn new(items: &Items) -> Self {
        let mut native = NativeMenu {
            menu: Menu::with_id("erlkonig.menu"),
            status: item(&items.status),
            integrations: Submenu::with_id(
                items.integrations.id.clone(),
                &items.integrations.label,
                items.integrations.enabled,
            ),
            no_clients: item(&items.no_clients),
            chatgpt: check_item(&items.chatgpt),
            openai_manual: item(&items.openai_manual),
            claude: check_item(&items.claude),
            cowork: item(&items.cowork),
            refresh_plugins: item(&items.refresh_plugins),
            login: check_item(&items.login),
            pause_sync: item(&items.pause_sync),
            retry_services: item(&items.retry_services),
            storage_folder: item(&items.storage_folder),
            quit: item(&items.quit),
            status_shown: false,
            retry_shown: false,
            rows: Vec::new(),
            applied: None,
        };
        native
            .menu
            .append_items(&[
                &native.pause_sync,
                &PredefinedMenuItem::separator(),
                &native.integrations,
                &native.storage_folder,
                &PredefinedMenuItem::separator(),
                &native.login,
                &native.quit,
            ])
            .expect("menu");
        native.sync(items);
        native
    }

    /// The next sync re-applies every item, whatever the model says changed.
    pub(crate) fn invalidate(&mut self) {
        self.applied = None;
    }

    /// Bring the native menu in line with `items`.
    pub(crate) fn sync(&mut self, items: &Items) {
        if self.applied.as_ref() == Some(items) {
            return;
        }
        let before = self.applied.take();
        let before = before.as_ref();
        let plain: [(&MenuItem, fn(&Items) -> &Control); 9] = [
            (&self.status, |i| &i.status),
            (&self.no_clients, |i| &i.no_clients),
            (&self.openai_manual, |i| &i.openai_manual),
            (&self.cowork, |i| &i.cowork),
            (&self.refresh_plugins, |i| &i.refresh_plugins),
            (&self.pause_sync, |i| &i.pause_sync),
            (&self.retry_services, |i| &i.retry_services),
            (&self.storage_folder, |i| &i.storage_folder),
            (&self.quit, |i| &i.quit),
        ];
        for (item, field) in plain {
            sync_item(item, field(items), before.map(field));
        }
        let checks: [(&CheckMenuItem, fn(&Items) -> &Control); 3] = [
            (&self.chatgpt, |i| &i.chatgpt),
            (&self.claude, |i| &i.claude),
            (&self.login, |i| &i.login),
        ];
        for (item, field) in checks {
            sync_check(item, field(items), before.map(field));
        }
        let integrations = before.map(|b| &b.integrations);
        if integrations.is_none_or(|b| b.label != items.integrations.label) {
            self.integrations.set_text(&items.integrations.label);
        }
        if integrations.is_none_or(|b| b.enabled != items.integrations.enabled) {
            self.integrations.set_enabled(items.integrations.enabled);
        }
        // The attention item is always first; Retry follows Pause sync.
        if items.status_visible != self.status_shown {
            if items.status_visible {
                self.menu
                    .insert(&self.status, 0)
                    .expect("insert attention item");
            } else {
                self.menu
                    .remove(&self.status)
                    .expect("remove attention item");
            }
            self.status_shown = items.status_visible;
        }
        if items.retry_visible != self.retry_shown {
            if items.retry_visible {
                self.menu
                    .insert(&self.retry_services, 1 + usize::from(self.status_shown))
                    .expect("insert retry item");
            } else {
                self.menu
                    .remove(&self.retry_services)
                    .expect("remove retry item");
            }
            self.retry_shown = items.retry_visible;
        }
        if self.rows != items.rows {
            while self.integrations.remove_at(0).is_some() {}
            for row in &items.rows {
                let _ = match row {
                    Row::NoClients => self.integrations.append(&self.no_clients),
                    Row::ChatGpt => self.integrations.append(&self.chatgpt),
                    Row::OpenAiManual => self.integrations.append(&self.openai_manual),
                    Row::Claude => self.integrations.append(&self.claude),
                    Row::Cowork => self.integrations.append(&self.cowork),
                    Row::Separator => self.integrations.append(&PredefinedMenuItem::separator()),
                    Row::Refresh => self.integrations.append(&self.refresh_plugins),
                };
            }
            self.rows = items.rows.clone();
        }
        self.applied = Some(items.clone());
    }
}
