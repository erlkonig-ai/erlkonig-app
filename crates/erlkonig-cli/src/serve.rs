use anyhow::{Result, anyhow};
use erlkonig_core::{
    local_import::ImportAnswer, paths::Layout, pile_upgrade::StartupControl, protected_access,
    runtime::Runtime, secret_entry::EntryAnswer,
};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::Signals,
};
use std::{thread, time::Duration};

pub fn run(layout: &Layout) -> Result<()> {
    let control = StartupControl::new(false);
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    let handle = signals.handle();
    let cancellation = control.clone();
    let listener = thread::Builder::new()
        .name("erlkonig-cli-signals".into())
        .spawn(move || {
            if signals.forever().next().is_some() {
                cancellation.cancel();
            }
        })?;
    // Install cancellation before startup. Runtime owns inspection/conversion
    // under its existing lock, and retains the same controlled startup path.
    let result = own(layout, &control);
    handle.close();
    let joined = listener
        .join()
        .map_err(|_| anyhow!("signal listener panicked"));
    result.and(joined)
}

fn own(layout: &Layout, control: &StartupControl) -> Result<()> {
    let mut runtime = Runtime::start_controlled(layout, control)?;
    let status = runtime.status();
    eprintln!("Erlkonig is running in the foreground. Stop with Ctrl-C or SIGTERM.");
    if let Some(url) = status.mcp_url {
        eprintln!("Local endpoint: {url}");
    }
    for warning in status.warnings {
        eprintln!("{warning}");
    }
    while !control.is_cancelled() && !runtime.is_finished() {
        answer_unavailable(&runtime);
        thread::sleep(Duration::from_millis(50));
    }
    // Explicit finish closes prompt bridges, drains HTTP, joins native workers
    // and closes the pile before releasing the existing owner lock. No forced
    // timeout or second owner is introduced by this shell.
    runtime.finish()
}

fn answer_unavailable(runtime: &Runtime) {
    if let Some(prompt) = runtime.take_secret_prompt() {
        prompt.answer(EntryAnswer::Unavailable);
    }
    if let Some(prompt) = runtime.take_import_prompt() {
        prompt.answer(ImportAnswer::Unavailable);
    }
    if let Some(prompt) = runtime.take_protected_prompt() {
        prompt.answer(protected_access::unavailable());
    }
}
