//! Refresh the login check when AppKit asks to populate the status menu,
//! including while a modal dialog owns the main thread's nested event loop.

use erlkonig_app::app::Login;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSMenu, NSMenuDelegate};
use objc2_foundation::{NSObject, NSObjectProtocol};
use std::cell::Cell;
use tray_icon::menu::{CheckMenuItem, ContextMenu, Menu};

pub(crate) struct LoginState {
    item: CheckMenuItem,
    observed: Cell<Login>,
    read: fn() -> Login,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = LoginState]
    pub(crate) struct LoginMenu;

    unsafe impl NSObjectProtocol for LoginMenu {}

    unsafe impl NSMenuDelegate for LoginMenu {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, _: &NSMenu) {
            let state = self.ivars();
            let observed = (state.read)();
            state.observed.set(observed);
            let checked = observed != Login::Off;
            if state.item.is_checked() != checked {
                state.item.set_checked(checked);
            }
        }
    }
);

impl LoginMenu {
    pub(crate) fn install(
        menu: &Menu, item: CheckMenuItem, read: fn() -> Login, mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(LoginState {
            item, observed: Cell::new(Login::Off), read,
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        // ContextMenu supplies the live NSMenu owned by this Menu. Its delegate
        // is weak; MacShell retains ours for the status item's lifetime.
        let menu = unsafe { &*menu.ns_menu().cast::<NSMenu>() };
        menu.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this
    }

    pub(crate) fn status(&self) -> Login {
        self.ivars().observed.get()
    }
}
