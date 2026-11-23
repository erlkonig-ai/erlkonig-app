fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let report = match args.as_slice() {
        [mode, path] if mode == "--collections" || mode == "--reopen-collections" => {
            erlkonig_ios::collections_proof::run_owned(path.into(), mode == "--reopen-collections")?
        }
        [path] => erlkonig_ios::proof::run_owned(path.into(), false)?,
        [mode, path] if mode == "--reopen" => erlkonig_ios::proof::run_owned(path.into(), true)?,
        _ => anyhow::bail!(
            "usage: ios-replica-proof [--reopen | --collections | --reopen-collections] ABSOLUTE_FIXTURE_DIRECTORY"
        ),
    };
    println!("{report}");
    Ok(())
}
