//! Read-only acceptance probe over the frozen native migration inspection API.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, path::Path};
use trible::migration::{self, Requirement};

fn stamp(metadata: &fs::Metadata) -> Value {
    json!({
        "device":metadata.dev(), "inode":metadata.ino(), "bytes":metadata.len(),
        "mtime_seconds":metadata.mtime(), "mtime_nanoseconds":metadata.mtime_nsec(),
        "ctime_seconds":metadata.ctime(), "ctime_nanoseconds":metadata.ctime_nsec(),
    })
}

pub fn run(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "inspection requires an explicit absolute pile path");
    let before = fs::metadata(path).context("inspect source metadata before native scan")?;
    ensure!(before.is_file(), "inspection source must be a regular file");
    let mut observed_prefix_bytes = 0;
    // Same implementation as migration::inspect; progress supplies the exact
    // bounded mapped length, which before/after stat alone cannot identify.
    // No key, index, network, conversion, lock or writable store is opened.
    let inspected = migration::inspect_with_progress(path, &mut |progress| {
        observed_prefix_bytes = progress.total_bytes;
        true
    });
    let after = fs::metadata(path).context("inspect source metadata after native scan")?;
    let before_stamp = stamp(&before);
    let after_stamp = stamp(&after);
    let mut receipt = json!({
        "source":path, "observed_prefix_bytes":observed_prefix_bytes,
        "inspection_complete":inspected.is_ok(),
        "metadata_unchanged":before_stamp == after_stamp,
        "same_file_identity":before.dev() == after.dev() && before.ino() == after.ino(),
        "size_increased_during_observation":after.len() > before.len(),
        "before":before_stamp, "after":after_stamp,
        "qualification":"Read-only bounded observation without the owner lock. A suffix appended beyond the observed prefix was not inspected. Metadata equality does not freeze later writes or prove absence of in-place changes; this is not final quiescent cutover validation.",
    });
    match inspected {
        Ok(inspection) => {
            let mut canonical_records = 0usize;
            let mut branch_choices = 0usize;
            for requirement in &inspection.requirements {
                match requirement {
                    Requirement::Canonical { records } => canonical_records += records,
                    Requirement::Branch { .. } => branch_choices += 1,
                }
            }
            receipt["inspection"] = json!({
                "current_framing":inspection.current_framing,
                "requirements":{"canonical_records":canonical_records,"branch_choices":branch_choices},
                "legacy_branch_count":inspection.legacy_branches.len(),
                "unsupported_for_rewrite_kind_count":inspection.unsupported.len(),
                "unsupported_for_rewrite_record_count":inspection.unsupported.iter().map(|kind| kind.occurrences).sum::<usize>(),
            });
            println!("{receipt}");
            Ok(())
        }
        Err(error) => {
            receipt["inspection_error"] = json!(format!("{error:#}"));
            println!("{receipt}");
            Err(error)
        }
    }
}
