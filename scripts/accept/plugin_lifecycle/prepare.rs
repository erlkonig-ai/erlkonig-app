//! Acceptance-only adapter to the candidate renderer and native lifecycle API.
//! Compile/link against the same isolated target's erlkonig_core rlib.
use erlkonig_core::{install, paths::Layout};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert!(
        args.len() == 3 || args.len() == 5,
        "usage: prepare DATA_DIR EXECUTABLE URL [inventory|install|remove codex|claude]"
    );
    let data = PathBuf::from(&args[0]);
    let layout = Layout::new(data);
    let executable = PathBuf::from(&args[1]);
    let url = args[2].to_str().expect("UTF-8 URL");
    if args.len() == 3 {
        let root =
            install::prepare(&layout, &executable, url).expect("render exact candidate package");
        println!("{}", root.display());
        return;
    }
    let host = match args[4].to_str().expect("UTF-8 host") {
        "codex" => install::Host::OpenAi,
        "claude" => install::Host::ClaudeCode,
        _ => panic!("unknown host"),
    };
    let cancel = AtomicBool::new(false);
    let state = match args[3].to_str().expect("UTF-8 operation") {
        "inventory" => install::inventory(host, &layout, &cancel),
        "install" => install::set_installed(host, true, &layout, &executable, url, &cancel),
        "remove" => install::set_installed(host, false, &layout, &executable, url, &cancel),
        _ => panic!("unknown operation"),
    }
    .expect("native lifecycle operation");
    println!(
        "{{\"installed\":{},\"enabled\":{},\"account_managed\":{},\"load_errors\":{}}}",
        state.installed, state.enabled, state.account_managed, state.load_errors
    );
}
