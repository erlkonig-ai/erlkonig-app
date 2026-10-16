//! erlkonig: superseded by the erlkonig-mac and erlkonig-linux binaries.

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !args.is_empty() {
        if let Err(error) = erlkonig_app::transport_command(&args) {
            eprintln!("Erlkonig transport: {error}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!("erlkonig: build erlkonig-mac or erlkonig-linux for the application");
    std::process::exit(1);
}
