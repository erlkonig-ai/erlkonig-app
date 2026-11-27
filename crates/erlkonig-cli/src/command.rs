use anyhow::{Context, Result, bail};
use erlkonig_core::transport::client::Tool;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

pub const INPUT_LIMIT: usize = 1024 * 1024;
pub const HELP: &str = "Erlkonig — foreground owner and Faculty client

  erlkonig erp --help
  erlkonig --version
  erlkonig serve [--data-dir ABSOLUTE_PATH]
  erlkonig workspaces list --url http://127.0.0.1:PORT/ [--output json]
  erlkonig tools list --url http://127.0.0.1:PORT/ [--output json]
  erlkonig call TOOL --url http://127.0.0.1:PORT/ [--workspace LABEL_OR_ID]
      [--json JSON | --json -] [--output json]

Owner and Faculty client commands accept --data-dir (the same app directory as the owner).
Client commands require an already-running owner and its explicit URL.
--json - reads one JSON object from stdin. Default arguments are {}.
--workspace resolves one current label or revision; selection lasts one call.
Without --workspace, an explicit JSON workspace argument is passed unchanged.
Tools and schemas come from the owner; `tools list --output json` shows both.
serve stays in the foreground until Ctrl-C or SIGTERM. It uses the same node,
workspaces and locks as the desktop app. GUI prompts are unavailable; storage
upgrades needing approval are refused. No command installs a daemon.";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Serve,
    Workspaces,
    Tools,
    Call(String),
}
pub struct Args {
    pub command: Command,
    pub data_dir: Option<PathBuf>,
    pub url: Option<String>,
    pub workspace: Option<String>,
    pub json: Option<String>,
    pub output_json: bool,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args> {
    let mut args = args.into_iter();
    let mut words = Vec::new();
    let mut options = BTreeMap::new();
    while let Some(arg) = args.next() {
        let word = arg.to_str().context("command option is not UTF-8")?;
        if word == "--help" || word == "-h" || word == "--version" {
            return Ok(Args {
                command: if word == "--version" {
                    Command::Version
                } else {
                    Command::Help
                },
                data_dir: None,
                url: None,
                workspace: None,
                json: None,
                output_json: false,
            });
        }
        if word.starts_with('-') {
            if !matches!(
                word,
                "--data-dir" | "--url" | "--workspace" | "--json" | "--output"
            ) {
                bail!("unknown option; use --help");
            }
            let value = args.next().context("option requires a value")?;
            if options.insert(word.to_owned(), value).is_some() {
                bail!("duplicate option {word}");
            }
        } else {
            words.push(word.to_owned());
        }
    }
    let command = match words
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => Command::Help,
        ["serve"] => Command::Serve,
        ["workspaces", "list"] => Command::Workspaces,
        ["tools", "list"] => Command::Tools,
        ["call", name] => Command::Call((*name).to_owned()),
        _ => bail!("unknown command; use --help"),
    };
    let data_dir = options.remove("--data-dir").map(PathBuf::from);
    let mut text = |name| -> Result<Option<String>> {
        options
            .remove(name)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("option value must be UTF-8"))
            })
            .transpose()
    };
    let url = text("--url")?;
    let workspace = text("--workspace")?;
    let json = text("--json")?;
    let output_json = match text("--output")?.as_deref() {
        None => false,
        Some("json") => true,
        _ => bail!("--output supports json"),
    };
    if !matches!(command, Command::Call(_)) && (workspace.is_some() || json.is_some()) {
        bail!("--workspace and --json apply only to call");
    }
    if command == Command::Serve && (url.is_some() || output_json) {
        bail!("serve uses the configured endpoint and prints its actual URL");
    }
    Ok(Args {
        command,
        data_dir,
        url,
        workspace,
        json,
        output_json,
    })
}

#[derive(Deserialize)]
pub struct Workspace {
    pub workspace: String,
    pub label: String,
}

pub fn workspace_report(result: &Value) -> Result<Value> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        bail!("list_workspaces failed; inspect the running owner's configuration");
    }
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .context("invalid workspace response")?;
    let [item] = content.as_slice() else {
        bail!("unexpected workspace response");
    };
    if item["type"] != "text" {
        bail!("unexpected workspace response type");
    }
    serde_json::from_str(item["text"].as_str().context("missing workspace report")?)
        .map_err(|_| anyhow::anyhow!("invalid workspace report"))
}

pub fn workspaces(report: &Value) -> Result<Vec<Workspace>> {
    serde_json::from_value(
        report
            .get("workspaces")
            .cloned()
            .context("missing workspaces")?,
    )
    .map_err(|_| anyhow::anyhow!("invalid workspace rows"))
}

pub fn resolve_workspace(report: &Value, selector: &str) -> Result<String> {
    let rows = workspaces(report)?;
    // Exact IDs and exact labels share one unambiguous match set. A label
    // which equals another row's ID must not silently choose either row.
    let matches: Vec<_> = rows
        .iter()
        .filter(|row| row.workspace == selector || row.label == selector)
        .collect();
    match matches.as_slice() {
        [row] => Ok(row.workspace.clone()),
        [] => bail!("workspace not found among current available revisions; list workspaces again"),
        _ => bail!("workspace selector is ambiguous; use a unique current revision ID"),
    }
}

pub fn validate_workspace_option(tool: &Tool, arguments: &Value) -> Result<()> {
    if tool
        .input_schema
        .pointer("/properties/workspace/type")
        .and_then(Value::as_str)
        != Some("string")
    {
        bail!("this tool does not advertise a string workspace argument");
    }
    if arguments.get("workspace").is_some_and(|v| !v.is_string()) {
        bail!("JSON workspace must be a string");
    }
    Ok(())
}

pub fn insert_workspace(arguments: &mut Value, workspace: &str) -> Result<()> {
    let object = arguments
        .as_object_mut()
        .context("arguments must be an object")?;
    if object
        .get("workspace")
        .is_some_and(|value| value.as_str() != Some(workspace))
    {
        bail!("--workspace conflicts with the JSON workspace argument");
    }
    object.insert("workspace".into(), Value::String(workspace.into()));
    Ok(())
}

pub fn require_workspace(tool: &Tool, arguments: &Value) -> Result<()> {
    let required = tool
        .input_schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|value| value == "workspace"));
    if required && !arguments.get("workspace").is_some_and(Value::is_string) {
        bail!("this tool requires --workspace or an explicit JSON workspace argument");
    }
    Ok(())
}

/// Prevent terminal control sequences from owner-authored human text. JSON
/// output preserves content with JSON's escaping for machine consumers.
pub fn visible(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn args(text: &[&str]) -> Result<Args> {
        parse(text.iter().map(|word| OsString::from(*word)))
    }
    fn tool(schema: Value) -> Tool {
        Tool {
            name: "anything".into(),
            description: None,
            input_schema: schema,
        }
    }
    #[test]
    fn explicit_ownership_and_bounded_command_surface() {
        assert_eq!(args(&[]).unwrap().command, Command::Help);
        assert_eq!(args(&["--version"]).unwrap().command, Command::Version);
        assert_eq!(args(&["serve"]).unwrap().command, Command::Serve);
        for words in [
            vec!["serve", "--url", "http://127.0.0.1:1/"],
            vec!["tools", "list", "--workspace", "x"],
            vec!["call", "knowledge_list", "--json", "{}", "--json", "{}"],
            vec!["daemon"],
        ] {
            assert!(args(&words).is_err());
        }
        let parsed = args(&[
            "call",
            "knowledge_list",
            "--json",
            "-",
            "--workspace",
            "My work",
            "--output",
            "json",
        ])
        .unwrap();
        assert_eq!(parsed.workspace.as_deref(), Some("My work"));
        assert!(parsed.output_json);
    }
    #[test]
    fn ambiguous_labels_and_id_label_collisions_refuse_selection() {
        let report = json!({"workspaces":[{"workspace":"A","label":"same"},{"workspace":"B","label":"same"}]});
        assert!(resolve_workspace(&report, "same").is_err());
        assert_eq!(resolve_workspace(&report, "A").unwrap(), "A");
        assert!(resolve_workspace(&report, "missing").is_err());
        let report =
            json!({"workspaces":[{"workspace":"A","label":"B"},{"workspace":"B","label":"other"}]});
        assert!(resolve_workspace(&report, "B").is_err());
    }
    #[test]
    fn schema_drives_selection_and_conflicting_json_is_never_overwritten() {
        let schema = json!({"properties":{"workspace":{"type":"string"}},"required":["workspace"]});
        let tool = tool(schema);
        assert!(validate_workspace_option(&tool, &json!({})).is_ok());
        assert!(require_workspace(&tool, &json!({})).is_err());
        let mut arguments = json!({"workspace":"A","other":12});
        assert!(insert_workspace(&mut arguments, "B").is_err());
        assert_eq!(arguments["workspace"], "A");
        insert_workspace(&mut arguments, "A").unwrap();
        assert!(require_workspace(&tool, &arguments).is_ok());
        let tool = Tool {
            name: "node_tool".into(),
            description: None,
            input_schema: json!({"properties":{}}),
        };
        assert!(validate_workspace_option(&tool, &json!({})).is_err());
        assert!(require_workspace(&tool, &json!({})).is_ok());
    }
}
