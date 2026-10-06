//! Local HTTP client configuration. No CLI launcher, credential helper or proxy.
//! Callers choose the destination explicitly; tests use only scratch profiles.

use crate::{credentials, paths::Layout, status::write_atomic};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

pub const CLAUDE_DESKTOP_LIMIT: &str = "Claude Code can connect directly to Erlkönig's authenticated local HTTP server. Claude chat's remote connectors run in Anthropic's cloud and cannot reach this computer's loopback address. Cowork's host-loopback HTTP plugin support has not been verified. No local stdio bridge is installed. Keep Erlkönig open while using its tools.";

fn credential(layout: &Layout) -> Result<String, String> {
    credentials::load(&layout.dir.join("mcp.token"))
}

pub fn codex_config(original: &str, url: &str, token: &str) -> Result<String, String> {
    let mut doc = original
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("cannot parse client config: {e}"))?;
    let server = &mut doc["mcp_servers"]["erlkonig"];
    if !server.is_none() && server.get("url").and_then(|v| v.as_str()) != Some(url) {
        return Err(
            "a different MCP server is already named erlkonig; leaving it unchanged".into(),
        );
    }
    server["url"] = toml_edit::value(url);
    server["http_headers"]["Authorization"] = toml_edit::value(format!("Bearer {token}"));
    server["enabled"] = toml_edit::value(true);
    Ok(doc.to_string())
}

pub fn claude_config(original: &str, url: &str, token: &str) -> Result<String, String> {
    let mut doc: Value = if original.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(original).map_err(|e| format!("cannot parse client config: {e}"))?
    };
    let object = doc
        .as_object_mut()
        .ok_or("client config is not an object")?;
    let servers = object
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("mcpServers is not an object")?;
    if let Some(existing) = servers.get("erlkonig") {
        if existing.get("url").and_then(Value::as_str) != Some(url) {
            return Err(
                "a different MCP server is already named erlkonig; leaving it unchanged".into(),
            );
        }
    }
    servers.insert(
        "erlkonig".into(),
        json!({"type":"http", "url":url, "headers":{"Authorization":format!("Bearer {token}")}}),
    );
    serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())
}

fn read_or_empty(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn save(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    write_atomic(path, text.as_bytes()).map_err(|e| format!("cannot save {}: {e}", path.display()))
}

/// Native config edit, invoked only after the person selects Install.
pub fn install_chatgpt(layout: &Layout, codex_home: &Path, url: &str) -> Result<PathBuf, String> {
    let path = codex_home.join("config.toml");
    let text = codex_config(&read_or_empty(&path)?, url, &credential(layout)?)?;
    save(&path, &text)?;
    Ok(path)
}

pub fn install_claude_code(
    layout: &Layout,
    config_file: &Path,
    url: &str,
) -> Result<PathBuf, String> {
    let text = claude_config(&read_or_empty(config_file)?, url, &credential(layout)?)?;
    save(config_file, &text)?;
    Ok(config_file.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn scratch_clients_preserve_unrelated_settings_and_only_use_http() {
        let d = tempfile::tempdir().unwrap();
        let layout = Layout::new(d.path().join("data"));
        fs::create_dir_all(&layout.dir).unwrap();
        credentials::load_or_create(&layout.dir.join("mcp.token")).unwrap();
        let codex_home = d.path().join("codex");
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(
            codex_home.join("config.toml"),
            "# keep me\nmodel = 'local-test'\n",
        )
        .unwrap();
        let path = install_chatgpt(&layout, &codex_home, "http://127.0.0.1:1234/").unwrap();
        let config = fs::read_to_string(&path).unwrap();
        assert!(config.contains("# keep me") && config.contains("local-test"));
        assert!(!config.contains("command") && !config.contains("helper"));
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let claude = d.path().join("claude.json");
        fs::write(&claude, "{\"theme\":\"dark\"}").unwrap();
        install_claude_code(&layout, &claude, "http://127.0.0.1:1234/").unwrap();
        let value: Value = serde_json::from_slice(&fs::read(claude).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["mcpServers"]["erlkonig"]["type"], "http");
    }
    #[test]
    fn unrelated_server_is_never_replaced() {
        assert!(
            codex_config(
                "[mcp_servers.erlkonig]\nurl='http://elsewhere/'",
                "http://127.0.0.1:1/",
                "secret"
            )
            .is_err()
        );
        assert!(
            claude_config(
                "{\"mcpServers\":{\"erlkonig\":{\"command\":\"other\"}}}",
                "http://127.0.0.1:1/",
                "secret"
            )
            .is_err()
        );
    }
}
