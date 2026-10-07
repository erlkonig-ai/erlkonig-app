//! erlkonig: the Erlkonig menu-bar app (macOS, milestone 1).
//!
//! The main-thread menu owns the complete native service lifetime. Quit stops
//! the HTTP listener and joins workers; no daemon or helper survives it.

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
mod crown_display;

mod crown;

#[cfg(target_os = "macos")]
fn main() {
    mac::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("erlkonig: the menu-bar application requires macOS");
    std::process::exit(1);
}
