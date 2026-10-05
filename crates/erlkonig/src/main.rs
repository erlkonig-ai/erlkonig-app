//! erlkonig: the Erlkonig menu-bar app (macOS, milestone 1).
//!
//! The menu shows the node's state and offers Copy my ID, Install in ChatGPT,
//! Install in Claude, Open at login (the node's LaunchAgent) and Quit. Quit
//! closes only this menu; the node keeps running.

#[cfg(target_os = "macos")]
mod mac;

mod icon;

#[cfg(target_os = "macos")]
fn main() {
    mac::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    // Keep the icon decoder exercised on every platform.
    let _ = icon::menubar_rgba();
    eprintln!("erlkonig: the menu-bar app is macOS-only for now; run erlkonig-node directly");
    std::process::exit(1);
}
