//! The environment every child gets: an allowlist, so no `TRIBLESPACE_*`,
//! `PILE`, `PERSONA`, `ERLKONIG_*` or token variable from the parent leaks in.

use std::env;
use std::ffi::OsString;
use std::process::Command;

pub const KEPT_VARS: &[&str] = &[
    "HOME", "PATH", "TMPDIR", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE",
];

/// launchd gives agents no PATH of its own on recent macOS.
pub const DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

pub fn scrubbed_from<I>(vars: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut out: Vec<(OsString, OsString)> = vars
        .into_iter()
        .filter(|(k, _)| KEPT_VARS.iter().any(|kept| k == kept))
        .collect();
    if !out.iter().any(|(k, v)| k == "PATH" && !v.is_empty()) {
        out.retain(|(k, _)| k != "PATH");
        out.push(("PATH".into(), DEFAULT_PATH.into()));
    }
    out.sort();
    out
}

pub fn scrubbed() -> Vec<(OsString, OsString)> {
    scrubbed_from(env::vars_os())
}

/// Clears `cmd`'s environment and sets only the allowlisted variables.
pub fn apply(cmd: &mut Command) {
    cmd.env_clear();
    for (k, v) in scrubbed() {
        cmd.env(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allowlisted_variables_survive() {
        let input = [
            ("HOME", "/h"),
            ("TRIBLESPACE_KEY", "/k"),
            ("TRIBLESPACE_COLLECTION_WIKI", "x"),
            ("PILE", "/p"),
            ("PERSONA", "liora"),
            ("DISCORD_TOKEN", "t"),
            ("ERLKONIG_DATA_DIR", "/d"),
            ("LANG", "en_US.UTF-8"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let out = scrubbed_from(input);
        let keys: Vec<_> = out.iter().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
        assert_eq!(keys, vec!["HOME", "LANG", "PATH"]);
        assert!(out.contains(&("PATH".into(), DEFAULT_PATH.into())));
    }
}
