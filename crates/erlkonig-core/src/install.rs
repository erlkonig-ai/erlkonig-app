//! "Install in ChatGPT" and "Install in Claude".
//!
//! Both copy the bundle's plugin folder (manifests and a skill, no binary) to
//! `D/plugins/erlkonig-plugin`, a stable path outside the app bundle, and make
//! `D/plugins` a local marketplace that Codex (`.agents/plugins/
//! marketplace.json`) and Claude Code (`.claude-plugin/marketplace.json`) both
//! accept. ChatGPT installs through its bundled `codex` CLI; Claude Desktop
//! only takes an uploaded zip, so that path writes one.

use crate::paths::{Layout, ensure_private_dir};
use crate::supervise::is_executable;
use serde_json::json;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const MARKETPLACE: &str = "erlkonig-local";
pub const PLUGIN: &str = "erlkonig";
pub const PLUGIN_DIR: &str = "erlkonig-plugin";
/// Claude's upload limit for a plugin zip.
pub const CLAUDE_ZIP_LIMIT: u64 = 200 * 1000 * 1000;
pub const CHATGPT_CODEX: &str = "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex";

#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Each command run, with its exit status and output.
    pub steps: Vec<String>,
    /// What to tell the person.
    pub message: String,
}

/// The plugin folder must carry the Claude manifest and MCP file, and must
/// not have a top-level `bin/` (Claude refuses such uploads).
pub fn check_plugin_source(src: &Path) -> Result<(), String> {
    for needed in [".claude-plugin/plugin.json", ".codex-plugin/plugin.json", ".mcp.json"] {
        if !src.join(needed).is_file() {
            return Err(format!("{} has no {needed}; is this the Erlkonig plugin folder?", src.display()));
        }
    }
    if fs::symlink_metadata(src.join("bin")).is_ok() {
        return Err(format!("{} has a top-level bin/, which Claude refuses", src.display()));
    }
    Ok(())
}

/// Copies regular files and directories, keeping permission bits. Symlinks
/// are refused: the plugin is plain files.
pub fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&from, &to)?;
        } else if ty.is_file() {
            fs::copy(&from, &to)?;
        } else {
            return Err(io::Error::other(format!("{} is not a plain file", from.display())));
        }
    }
    let mode = fs::metadata(src)?.permissions().mode();
    fs::set_permissions(dst, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Copies the plugin to `D/plugins/erlkonig-plugin` (replacing an older copy
/// in one rename) and writes both marketplace files. Returns the marketplace
/// root, `D/plugins`.
pub fn stage_plugin(layout: &Layout, src: &Path) -> Result<PathBuf, String> {
    check_plugin_source(src)?;
    let root = layout.plugins();
    ensure_private_dir(&layout.dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let pid = std::process::id();
    let fresh = root.join(format!(".{PLUGIN_DIR}.new-{pid}"));
    let retired = root.join(format!(".{PLUGIN_DIR}.old-{pid}"));
    let target = root.join(PLUGIN_DIR);
    let _ = fs::remove_dir_all(&fresh);
    copy_tree(src, &fresh).map_err(|e| format!("cannot copy the plugin: {e}"))?;
    if target.exists() {
        fs::rename(&target, &retired).map_err(|e| format!("cannot move the old plugin aside: {e}"))?;
    }
    fs::rename(&fresh, &target).map_err(|e| format!("cannot put the plugin in place: {e}"))?;
    let _ = fs::remove_dir_all(&retired);
    write_marketplaces(&root).map_err(|e| format!("cannot write the marketplace: {e}"))?;
    Ok(root)
}

pub fn write_marketplaces(root: &Path) -> io::Result<()> {
    let source = format!("./{PLUGIN_DIR}");
    let codex = json!({
        "name": MARKETPLACE,
        "interface": { "displayName": "Erlkönig (this computer)" },
        "plugins": [{
            "name": PLUGIN,
            "source": { "source": "local", "path": source },
            "policy": { "installation": "AVAILABLE", "authentication": "ON_INSTALL" },
            "category": "Productivity"
        }]
    });
    let claude = json!({
        "name": MARKETPLACE,
        "description": "The Erlkönig plugin from the app installed on this computer.",
        "owner": { "name": "Erlkönig" },
        "plugins": [{
            "name": PLUGIN,
            "source": source,
            "description": "Goals, wiki, journal, people, files and messages in the Erlkönig pile on this computer."
        }]
    });
    for (dir, value) in [(".agents/plugins", codex), (".claude-plugin", claude)] {
        let d = root.join(dir);
        fs::create_dir_all(&d)?;
        let mut text = serde_json::to_vec_pretty(&value).map_err(io::Error::other)?;
        text.push(b'\n');
        crate::status::write_atomic(&d.join("marketplace.json"), &text)?;
        fs::set_permissions(d.join("marketplace.json"), fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

fn which(name: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|p| env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

/// ChatGPT's bundled Codex CLI, else `codex` on PATH.
pub fn find_codex() -> Option<PathBuf> {
    let mut candidates = vec![PathBuf::from(CHATGPT_CODEX)];
    if let Some(home) = env::var_os("HOME") {
        candidates.push(Path::new(&home).join("Applications").join(&CHATGPT_CODEX[14..]));
    }
    candidates.into_iter().find(|p| is_executable(p)).or_else(|| which("codex"))
}

/// `claude` on PATH, else where its installer usually puts it. A menu-bar
/// app started by launchd has a bare PATH, so the usual places are checked.
pub fn find_claude() -> Option<PathBuf> {
    which("claude").or_else(|| {
        let home = env::var_os("HOME").map(PathBuf::from)?;
        [home.join(".local/bin/claude"), "/opt/homebrew/bin/claude".into(), "/usr/local/bin/claude".into()]
            .into_iter()
            .find(|p| is_executable(p))
    })
}

fn run(report: &mut Report, program: &Path, args: &[&str]) -> Result<Output, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    report.steps.push(format!(
        "$ {} {}\n{}{}[{}]",
        program.display(),
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        out.status
    ));
    Ok(out)
}

fn run_ok(report: &mut Report, program: &Path, args: &[&str]) -> Result<Output, String> {
    let out = run(report, program, args)?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(format!(
            "`{} {}` failed: {}",
            program.file_name().unwrap_or_default().to_string_lossy(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Adds `root` as the `erlkonig-local` marketplace unless the CLI already
/// lists it at that root (a listing at another root is replaced).
fn ensure_marketplace(report: &mut Report, cli: &Path, root: &str, remove: &str) -> Result<(), String> {
    let listed = run(report, cli, &["plugin", "marketplace", "list"])
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let ours = listed.lines().find(|l| l.contains(MARKETPLACE));
    match ours {
        Some(line) if line.contains(root) => return Ok(()),
        Some(_) => {
            run_ok(report, cli, &["plugin", "marketplace", remove, MARKETPLACE])?;
        }
        None => {}
    }
    run_ok(report, cli, &["plugin", "marketplace", "add", root]).map(drop)
}

/// Install in ChatGPT: stage the plugin, register the marketplace with the
/// Codex CLI and install the plugin. ChatGPT loads it after a restart.
pub fn install_chatgpt(layout: &Layout, plugin_src: &Path, codex: &Path) -> Result<Report, String> {
    let mut report = Report::default();
    let root = stage_plugin(layout, plugin_src)?;
    let root = root.to_string_lossy().into_owned();
    ensure_marketplace(&mut report, codex, &root, "remove")?;
    let id = format!("{PLUGIN}@{MARKETPLACE}");
    run_ok(&mut report, codex, &["plugin", "add", &id])?;
    report.message = "Erlkönig is installed in ChatGPT. Quit ChatGPT completely (ChatGPT menu > Quit ChatGPT) \
and open it again; the Erlkönig plugin then appears under Plugins."
        .into();
    Ok(report)
}

/// Install in Claude Code through the `claude` CLI (the Desktop Code tab
/// reads the same settings).
pub fn install_claude_code(layout: &Layout, plugin_src: &Path, claude: &Path) -> Result<Report, String> {
    let mut report = Report::default();
    let root = stage_plugin(layout, plugin_src)?;
    let root = root.to_string_lossy().into_owned();
    ensure_marketplace(&mut report, claude, &root, "remove")?;
    let id = format!("{PLUGIN}@{MARKETPLACE}");
    run_ok(&mut report, claude, &["plugin", "install", &id])?;
    report.message = "Erlkönig is installed in Claude Code. Start a new session to load it.".into();
    Ok(report)
}

/// Writes `D/erlkonig-claude-plugin.zip` for Claude's Upload plugin dialog.
/// Returns its path and size.
pub fn build_claude_zip(layout: &Layout, plugin_src: &Path) -> Result<(PathBuf, u64), String> {
    use zip::write::SimpleFileOptions;
    check_plugin_source(plugin_src)?;
    ensure_private_dir(&layout.dir).map_err(|e| e.to_string())?;
    let dest = layout.claude_zip();
    let tmp = dest.with_file_name(format!(".erlkonig-claude-plugin.zip.tmp-{}", std::process::id()));
    let file = fs::File::create(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let mut stack = vec![PathBuf::new()];
    let mut entries = Vec::new();
    while let Some(rel) = stack.pop() {
        let dir = plugin_src.join(&rel);
        let mut children: Vec<_> = fs::read_dir(&dir)
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        children.sort_by_key(|e| e.file_name());
        for entry in children {
            let rel_child = rel.join(entry.file_name());
            let ty = entry.file_type().map_err(|e| e.to_string())?;
            let mode = entry.metadata().map_err(|e| e.to_string())?.permissions().mode();
            let name = rel_child.to_string_lossy().into_owned();
            if ty.is_dir() {
                zip.add_directory(format!("{name}/"), SimpleFileOptions::default().unix_permissions(mode))
                    .map_err(|e| e.to_string())?;
                stack.push(rel_child);
            } else if ty.is_file() {
                let opts = SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(mode);
                zip.start_file(&name, opts).map_err(|e| e.to_string())?;
                let bytes = fs::read(entry.path()).map_err(|e| e.to_string())?;
                zip.write_all(&bytes).map_err(|e| e.to_string())?;
                entries.push(name);
            } else {
                return Err(format!("{} is not a plain file", entry.path().display()));
            }
        }
    }
    zip.finish().map_err(|e| e.to_string())?;
    let size = fs::metadata(&tmp).map_err(|e| e.to_string())?.len();
    if size > CLAUDE_ZIP_LIMIT {
        let _ = fs::remove_file(&tmp);
        return Err(format!("the plugin zip is {size} bytes, over Claude's 200 MB limit"));
    }
    fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    Ok((dest, size))
}

/// The entry names of a zip file, for checks.
pub fn zip_entry_names(path: &Path) -> Result<Vec<String>, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    (0..archive.len())
        .map(|i| archive.by_index(i).map(|f| f.name().to_string()).map_err(|e| e.to_string()))
        .collect()
}

/// The steps the menu shows after writing the zip.
pub fn claude_upload_steps(zip: &Path) -> String {
    format!(
        "Claude needs the plugin uploaded once:\n\n\
1. In Claude, open Customize, then Plugins.\n\
2. Choose Add plugin, then Upload plugin.\n\
3. Pick {} (Finder is showing it).\n\n\
Local Cowork tasks and Claude Code then start Erlkönig's tools. Ordinary chats do not run local tools.",
        zip.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = env::temp_dir().join(format!("erlkonig-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn template() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugin")
    }

    #[test]
    fn the_repository_plugin_template_is_valid() {
        check_plugin_source(&template()).unwrap();
        let mcp: serde_json::Value =
            serde_json::from_slice(&fs::read(template().join(".mcp.json")).unwrap()).unwrap();
        let server = &mcp["mcpServers"]["faculties"];
        assert_eq!(server["command"], "/bin/sh");
        let script = server["args"][1].as_str().unwrap();
        assert!(!script.contains("${"), "no placeholder expansion may be needed: {script}");
        assert!(!template().join("plugin.json").exists(), "a top-level plugin.json would make Codex cap the tools");
    }

    #[test]
    fn staging_writes_both_marketplaces_and_replaces_the_old_copy() {
        let d = scratch("stage");
        let layout = Layout::new(d.join("D"));
        let root = stage_plugin(&layout, &template()).unwrap();
        fs::write(root.join(PLUGIN_DIR).join("stale.txt"), "x").unwrap();
        let root = stage_plugin(&layout, &template()).unwrap();
        assert!(!root.join(PLUGIN_DIR).join("stale.txt").exists());
        assert!(root.join(PLUGIN_DIR).join(".codex-plugin/plugin.json").is_file());
        let codex: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(".agents/plugins/marketplace.json")).unwrap()).unwrap();
        assert_eq!(codex["name"], MARKETPLACE);
        assert_eq!(codex["plugins"][0]["source"]["path"], "./erlkonig-plugin");
        let claude: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(".claude-plugin/marketplace.json")).unwrap()).unwrap();
        assert_eq!(claude["plugins"][0]["source"], "./erlkonig-plugin");
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".new-") || n.contains(".old-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn claude_zip_has_the_plugin_at_its_root_and_no_bin() {
        let d = scratch("zip");
        let layout = Layout::new(d.join("D"));
        let (zip, size) = build_claude_zip(&layout, &template()).unwrap();
        assert!(size < CLAUDE_ZIP_LIMIT);
        let names = zip_entry_names(&zip).unwrap();
        assert!(names.contains(&".claude-plugin/plugin.json".to_string()), "{names:?}");
        assert!(names.contains(&".mcp.json".to_string()));
        assert!(names.contains(&"skills/faculties/SKILL.md".to_string()));
        assert!(!names.iter().any(|n| n == "bin/" || n.starts_with("bin/")));
    }

    #[test]
    fn a_plugin_with_a_top_level_bin_is_refused() {
        let d = scratch("bin");
        copy_tree(&template(), &d.join("p")).unwrap();
        fs::create_dir_all(d.join("p/bin")).unwrap();
        assert!(check_plugin_source(&d.join("p")).unwrap_err().contains("bin/"));
    }
}
