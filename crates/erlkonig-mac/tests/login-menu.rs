#[cfg(target_os = "macos")]
#[path = "../src/login_menu.rs"]
mod login_menu;

#[cfg(target_os = "macos")]
fn main() {
    use erlkonig_app::app::Login;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuDelegate};
    use std::cell::Cell;
    use tray_icon::menu::{CheckMenuItem, ContextMenu, Menu};

    thread_local! {
        static REGISTRATION: Cell<Login> = const { Cell::new(Login::Off) };
        static READS: Cell<u32> = const { Cell::new(0) };
    }
    let mtm = MainThreadMarker::new().unwrap();
    let _app = NSApplication::sharedApplication(mtm);
    let menu = Menu::new();
    let check = CheckMenuItem::new("Open at login", true, false, None);
    menu.append(&check).unwrap();
    let delegate = login_menu::LoginMenu::install(&menu, check.clone(), || {
        READS.set(READS.get() + 1);
        REGISTRATION.get()
    }, mtm);
    for _ in 0..16 {
        assert_eq!(delegate.status(), Login::Off);
    }
    assert_eq!(READS.get(), 0);

    // An ordinary update of the hidden menu never consults Service Management.
    let native = unsafe { &*menu.ns_menu().cast::<NSMenu>() };
    native.update();
    assert_eq!(READS.get(), 0);
    let installed = native.delegate().expect("status menu delegate");
    for (i, state) in [Login::On, Login::NeedsApproval, Login::Off].into_iter().enumerate() {
        REGISTRATION.set(state);
        installed.menuNeedsUpdate(native);
        assert_eq!(READS.get(), i as u32 + 1);
        assert_eq!(delegate.status(), state);
        assert_eq!(check.is_checked(), state != Login::Off);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
