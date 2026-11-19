//! erlkonig-mac: the Erlkonig menu-bar app on macOS, installed in the bundle
//! as `Contents/MacOS/erlkonig`.
//!
//! The main-thread menu owns the complete native service lifetime. Quit stops
//! the HTTP listener and joins workers. `mcp-stdio` is a client-owned transport
//! to that existing owner; it never enters the menu or starts native services.

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
mod login_menu;

#[cfg(target_os = "macos")]
mod alert;

#[cfg(target_os = "macos")]
mod protected_access;
#[cfg(target_os = "macos")]
mod local_import;

#[cfg(target_os = "macos")]
mod crown_display;

#[cfg(target_os = "macos")]
mod crown_image;

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.is_empty() {
        if let Err(error) = erlkonig_app::transport_command(&args) {
            eprintln!("Erlkonig transport: {error}");
            std::process::exit(1);
        }
        return;
    }
    mac::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("erlkonig-mac: this binary is built only on macOS");
    std::process::exit(1);
}
