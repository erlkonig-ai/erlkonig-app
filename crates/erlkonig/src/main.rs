//! erlkonig: the Erlkonig menu-bar app (macOS) and tray app (Linux).
//!
//! The main-thread menu owns the complete native service lifetime. Quit stops
//! the HTTP listener and joins workers. `mcp-stdio` is a client-owned transport
//! to that existing owner; it never enters the menu or starts native services.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod app;

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod crown_display;

mod crown;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.is_empty() {
        if let Err(error) = transport_command(&args) {
            eprintln!("Erlkonig transport: {error}");
            std::process::exit(1);
        }
        return;
    }
    #[cfg(target_os = "macos")]
    mac::main();
    #[cfg(target_os = "linux")]
    linux::main();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        eprintln!("erlkonig: the menu-bar application requires macOS or Linux");
        std::process::exit(1);
    }
}

fn transport_command(args: &[std::ffi::OsString]) -> Result<(), String> {
    if erlkonig_core::node_cli::run(args).map_err(|error| format!("{error:#}"))? {
        return Ok(());
    }
    let usage = "usage: erlkonig mcp-stdio --data-dir ABSOLUTE_PATH --url http://127.0.0.1:PORT/";
    if args.len() != 5 || args[0] != "mcp-stdio" || args[1] != "--data-dir" || args[3] != "--url" {
        return Err(usage.into());
    }
    let dir = std::path::PathBuf::from(&args[2]);
    if !dir.is_absolute() {
        return Err(usage.into());
    }
    let url = args[4].to_str().ok_or(usage)?;
    erlkonig_core::transport::stdio(&erlkonig_core::paths::Layout::new(dir), url)
        .map_err(|error| error.to_string())
}
