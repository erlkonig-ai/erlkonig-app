//! The platform-neutral Erlkonig app, shared by the `erlkonig-mac` and
//! `erlkonig-linux` binaries (both installed as `erlkonig`).
//!
//! The main-thread menu owns the complete native service lifetime. Quit stops
//! the HTTP listener and joins workers. `mcp-stdio` is a client-owned transport
//! to that existing owner; it never enters the menu or starts native services.

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod app;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native_menu;

pub mod crown;
pub mod faculties_browser;

/// The command-line verbs: the node CLI and the `mcp-stdio` transport. Each
/// binary calls this whenever it is started with arguments, before any shell.
pub fn transport_command(args: &[std::ffi::OsString]) -> Result<(), String> {
    if faculties_browser::run(args)? { return Ok(()); }
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

mod update_notice;
