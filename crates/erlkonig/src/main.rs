//! erlkonig: the Erlkonig menu-bar app (macOS) and tray app (Linux).
//!
//! The main-thread menu owns the complete native service lifetime. Quit stops
//! the HTTP listener and joins workers. `mcp-stdio` is a client-owned transport
//! to that existing owner; it never enters the menu or starts native services.

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
mod crown_display;

#[cfg(target_os = "macos")]
mod crown_image;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.is_empty() {
        if let Err(error) = erlkonig_app::transport_command(&args) {
            eprintln!("Erlkonig transport: {error}");
            std::process::exit(1);
        }
        return;
    }
    #[cfg(target_os = "macos")]
    mac::main();
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("erlkonig: the menu-bar application requires macOS; on Linux use erlkonig-linux");
        std::process::exit(1);
    }
}
