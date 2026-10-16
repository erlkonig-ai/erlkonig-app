//! macOS shell: AppKit alerts, Login Items, LaunchServices and the status-bar
//! crown, around the shared application in `erlkonig_app::app`.

use crate::crown_display::DisplayCrown;
use erlkonig_app::app::{App, Login, Shell};
use erlkonig_app::crown::WorkActivity;
use erlkonig_core::paths::{self, Layout};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSWorkspace};
use objc2_foundation::{NSBundle, NSString, NSURL};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::AtomicBool};
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
        let alert = NSAlert::new(self.mtm);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(text));
        for button in buttons {
            alert.addButtonWithTitle(&NSString::from_str(button));
        }
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
}

/// The shared application as this binary's own type: winit's
/// `ApplicationHandler` and `App` both live in other crates, so the orphan
/// rule needs a local type to implement one for the other.
struct Mac(App<MacShell>);

impl ApplicationHandler<MenuEvent> for Mac {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause == StartCause::Init {
            match TrayIconBuilder::new()
                .with_menu(Box::new(self.0.menu.clone()))
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
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = proxy.send_event(e);
    }));
    let terminate = Arc::new(AtomicBool::new(false));
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
