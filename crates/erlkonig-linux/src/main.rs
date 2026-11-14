//! erlkonig-linux: the Erlkonig tray app on Linux, installed as `erlkonig`.
//!
//! The main-thread event loop owns the complete native service lifetime. Quit
//! stops the HTTP listener and joins workers. `mcp-stdio` is a client-owned
//! transport to that existing owner; it never enters the shell or starts
//! native services.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.is_empty() {
        if let Err(error) = erlkonig_app::transport_command(&args) {
            eprintln!("Erlkonig transport: {error}");
            std::process::exit(1);
        }
        return;
    }
    linux::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("erlkonig-linux: this binary is built only on Linux");
    std::process::exit(1);
}
