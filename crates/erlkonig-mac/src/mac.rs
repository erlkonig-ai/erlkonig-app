//! macOS shell: AppKit alerts, Login Items, LaunchServices and the status-bar
//! crown, around the shared application in `erlkonig_app::app`.

use crate::crown_display::DisplayCrown;
use erlkonig_app::app::{App, Login, Shell};
use erlkonig_app::crown::WorkActivity;
use erlkonig_core::paths::{self, Layout};
use erlkonig_core::secret_entry::{EntryAnswer, EntryKind, EntryValue, SecretPrompt};
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSScreen, NSWorkspace};
use objc2_foundation::{NSBundle, NSSize, NSString, NSURL};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tray_icon::menu::MenuEvent;
use tray_icon::{TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
use winit::window::WindowId;

struct MacShell {
    mtm: MainThreadMarker,
    tray: Option<TrayIcon>,
    crown: Option<DisplayCrown>,
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

impl Shell for MacShell {
    fn alert(&mut self, title: &str, text: &str, buttons: &[&str]) -> Option<usize> {
        let app = NSApplication::sharedApplication(self.mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let visible_size = NSScreen::mainScreen(self.mtm)
            .map(|screen| screen.visibleFrame().size)
            .unwrap_or(NSSize::new(1024.0, 768.0));
        let alert = crate::alert::message_alert(self.mtm, title, text, buttons, visible_size);
        Some((alert.runModal() - NSAlertFirstButtonReturn).max(0) as usize)
    }

    fn open_url(&self, url: &str) {
        if let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) {
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
    }

    fn reveal(&self, path: &Path) {
        NSWorkspace::sharedWorkspace().selectFile_inFileViewerRootedAtPath(
            Some(&NSString::from_str(&path.to_string_lossy())),
            &NSString::from_str(""),
        );
    }

    fn show_folder(&self, folder: &Path) -> Result<(), &'static str> {
        if NSWorkspace::sharedWorkspace().selectFile_inFileViewerRootedAtPath(
            None,
            &NSString::from_str(&folder.to_string_lossy()),
        ) {
            Ok(())
        } else {
            Err("Finder could not open the active storage folder.")
        }
    }

    fn login(&self) -> Login {
        match unsafe { login().status() } {
            SMAppServiceStatus::Enabled => Login::On,
            SMAppServiceStatus::RequiresApproval => Login::NeedsApproval,
            _ => Login::Off,
        }
    }

    fn set_login(&self, on: bool) -> Result<(), String> {
        let service = login();
        let result = unsafe {
            if on {
                service.registerAndReturnError()
            } else {
                service.unregisterAndReturnError()
            }
        };
        result.map_err(|error| error.localizedDescription().to_string())
    }

    fn open_login_settings(&self) {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }

    fn detect_apps(&self) -> (Option<(PathBuf, String)>, bool) {
        (
            installed_app("com.openai.codex", "OpenAI"),
            installed_app("com.anthropic.claudefordesktop", "Claude").is_some(),
        )
    }

    fn crown(&mut self, activity: Option<WorkActivity>, needs_attention: bool) {
        if let Some(crown) = &self.crown {
            crown.update(activity, needs_attention);
        }
    }

    fn stop_crown(&mut self) {
        self.crown.take();
    }

    fn show_window(&mut self) {}

    fn enter_secret(&mut self, prompt: &SecretPrompt, terminate: &Arc<AtomicBool>) -> EntryAnswer {
        use block2::RcBlock;
        use objc2_app_kit::{NSModalPanelRunLoopMode, NSSecureTextField, NSTextField, NSView};
        use objc2_foundation::{NSPoint, NSRect, NSRunLoop, NSSize, NSTimer};
        use zeroize::Zeroizing;

        if prompt.is_cancelled() || terminate.load(Ordering::Relaxed) {
            return EntryAnswer::Cancelled;
        }
        let app = NSApplication::sharedApplication(self.mtm);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let request = &prompt.request;
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&NSString::from_str("Save a local secret"));
        alert.setInformativeText(&NSString::from_str(&format!(
            "Workspace: {}\nID: {}\nService: {}\nSecret name: {}\n\nEnter the credentials in the fields below, then choose Save. Never put them in chat. Saved encrypted in this workspace; no sharing grants are created. Cancel saves nothing.",
            request.workspace_label, request.workspace, request.service, request.name,
        )));
        alert.addButtonWithTitle(&NSString::from_str("Save"));
        let cancel = alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        let login = request.kind == EntryKind::Login;
        let rect = |y, height| NSRect::new(NSPoint::new(0.0, y), NSSize::new(380.0, height));
        let fields = NSView::initWithFrame(
            NSView::alloc(self.mtm),
            rect(0.0, if login { 120.0 } else { 60.0 }),
        );
        let password =
            NSSecureTextField::initWithFrame(NSSecureTextField::alloc(self.mtm), rect(0.0, 24.0));
        let password_label = NSTextField::labelWithString(
            &NSString::from_str(if login { "Password" } else { "Secret value" }),
            self.mtm,
        );
        password_label.setFrame(rect(28.0, 20.0));
        fields.addSubview(&password_label);
        fields.addSubview(&password);
        let username = if login {
            let label =
                NSTextField::labelWithString(&NSString::from_str("Username or email"), self.mtm);
            label.setFrame(rect(88.0, 20.0));
            let field = NSTextField::initWithFrame(NSTextField::alloc(self.mtm), rect(60.0, 24.0));
            fields.addSubview(&label);
            fields.addSubview(&field);
            // Both views are retained by the accessory throughout this modal.
            unsafe {
                field.setNextKeyView(Some(&password));
            }
            Some(field)
        } else {
            None
        };
        alert.setAccessoryView(Some(&fields));
        let window = alert.window();
        if let Some(username) = &username {
            window.setInitialFirstResponder(Some(username));
        } else {
            window.setInitialFirstResponder(Some(&password));
        }

        // runModal has its own loop. A timer in its mode observes an already
        // visible prompt's cancellation, including SIGTERM without a winit tick.
        // Capture only Send Rust state; resolve AppKit objects on the main thread.
        let cancelled = prompt.cancellation_check();
        let stopping = terminate.clone();
        let check = RcBlock::new(move |_timer: std::ptr::NonNull<NSTimer>| {
            if cancelled() || stopping.load(Ordering::Relaxed) {
                if let Some(mtm) = MainThreadMarker::new() {
                    NSApplication::sharedApplication(mtm).abortModal();
                }
            }
        });
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.1, true, &check) };
        unsafe {
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSModalPanelRunLoopMode);
        }
        let result = alert.runModal();
        timer.invalidate();
        let answer = if result == NSAlertFirstButtonReturn
            && !prompt.is_cancelled()
            && !terminate.load(Ordering::Relaxed)
        {
            let value = Zeroizing::new(password.stringValue().to_string());
            EntryAnswer::Submit(if let Some(username) = &username {
                EntryValue::login(Zeroizing::new(username.stringValue().to_string()), value)
            } else {
                EntryValue::secret(value)
            })
        } else {
            EntryAnswer::Cancelled
        };
        // Clear controls promptly; AppKit/editor copies are not guaranteed erased.
        password.setStringValue(&NSString::from_str(""));
        if let Some(username) = username {
            username.setStringValue(&NSString::from_str(""));
        }
        window.orderOut(None);
        answer
    }
}

/// The shared application as this binary's own type: winit's
/// `ApplicationHandler` and `App` both live in other crates, so the orphan
/// rule needs a local type to implement one for the other.
struct Mac(App<MacShell>);

impl ApplicationHandler<MenuEvent> for Mac {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause == StartCause::Init {
            match TrayIconBuilder::new()
                .with_menu(Box::new(self.0.native_menu()))
                .with_autosave_name("ai.erlkonig.app.status")
                .build()
            {
                Ok(tray) => {
                    let mtm = self.0.shell.mtm;
                    if let Some(button) = tray.ns_status_item().and_then(|item| item.button(mtm)) {
                        self.0.shell.crown = Some(DisplayCrown::new(button, self.0.origin(), mtm));
                    } else {
                        self.0.problem = Some("Cannot create crown display".into());
                    }
                    // tray-icon also fits its click-target view when setting
                    // the tooltip. Do this after installing our native image,
                    // so its hit area includes the full, wider crown.
                    let _ = tray.set_tooltip(Some("Erlkonig"));
                    self.0.shell.tray = Some(tray);
                }
                Err(error) => self.0.problem = Some(format!("Cannot create menu: {error}")),
            }
            self.0.start();
        }
        self.0.refresh(event_loop);
    }
    fn resumed(&mut self, _: &ActiveEventLoop) {}
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: MenuEvent) {
        self.0.on_menu(event);
        self.0.refresh(event_loop);
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.0.next_refresh));
    }
    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.0.finish_termination();
    }
}

pub fn main() {
    let mtm = MainThreadMarker::new().expect("Erlkonig starts on the main thread");
    let event_loop = EventLoop::<MenuEvent>::with_user_event()
        .with_activation_policy(ActivationPolicy::Accessory)
        .build()
        .expect("event loop");
    let terminate = Arc::new(AtomicBool::new(false));
    let modal_terminate = terminate.clone();
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        // Native menu callbacks may arrive inside NSAlert's nested loop before
        // winit can dispatch its event. Let the modal's timer observe Quit too.
        if e.id == "erlkonig.quit" {
            modal_terminate.store(true, Ordering::Relaxed);
        }
        let _ = proxy.send_event(e);
    }));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, terminate.clone()).expect("shutdown signal");
    }
    let shell = MacShell {
        mtm,
        tray: None,
        crown: None,
    };
    let layout = Layout::application(paths::default_data_dir().expect("data directory"))
        .expect("node bootstrap location");
    let mut mac = Mac(App::new(shell, layout, terminate));
    if let Err(error) = event_loop.run_app(&mut mac) {
        eprintln!("Erlkonig: {error}");
    }
    // Idempotent fallback for event-loop construction/execution errors.
    mac.0.finish_termination();
}
