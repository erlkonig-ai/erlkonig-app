//! Node-scoped configuration on the app's existing authenticated local MCP.
//! No workspace is needed to configure the first one. These adapters retain
//! the resident owner, not a second settings store or a client selection.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use faculties::mcp::{self, Bytes, Faculty, Tool};
use faculties::out::Out;
use faculties::schemas::config::config;
use serde::Deserialize;
use serde_json::{Value, json};
use triblespace::core::collection::CollectionHandle;
use triblespace::core::metadata;
use triblespace::core::query::register::{ObservationOrder, maximal};
use triblespace::prelude::*;
use triblespace_net::health_record;

use crate::config::{NodeBinding, parse_handle};
use crate::node_schema as attrs;
use crate::node_store::{ConfigView, NodeSettings};
use crate::runtime::SharedNode;
use crate::workspace::{self, Discovery, Workspace};

const TOOLS: &[Tool] = &[
    Tool {
        name: "config_inspect",
        description: "Inspect this running Erlkonig node's public identity, typed settings and workspace bundles. No workspace required. Returns an opaque observation to pass unchanged on a configuration edit. Conflicted settings remain visible; no secrets, private keys, paths or raw Config facts are returned. Configuration is local administration, not a sharing grant or a global workspace switch.",
        input_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    },
    Tool {
        name: "config_list_collections",
        description: "Discover existing local collection roots, names and independent READ/WRITE admission, plus known faculty role names and existing Config mappings. No workspace required. This neither fetches remote data nor grants permission. Use exact roots returned here for a requested bundle; do not infer authority from a name or handle.",
        input_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    },
    Tool {
        name: "config_create_workspace",
        description: "Create a named workspace in this node's one store with its existing identity. Omit routes to create a fresh private collection bundle, or supply a nonempty complete list of existing local role/root choices from config_list_collections. No sharing grant, key, path, secret or global selection is changed. Pass the observation from config_inspect; returns the exact workspace revision for ordinary faculty calls.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"label":{"type":"string"},"routes":{"type":"array","minItems":1,"items":{"type":"object","properties":{"role":{"type":"string"},"root":{"type":"string"}},"required":["role","root"],"additionalProperties":false}}},"required":["observation","label"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_edit_workspace",
        description: "Edit an exact current workspace revision, never a label or prefix. Omitted label/routes are preserved; supplied routes replace the entire bundle with nonempty existing local role/root choices. Old workspace IDs become unavailable, never redirected. Stale observations or conflicted revisions are rejected: inspect again and explicitly resolve the choice. No sharing permissions change.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"workspace":{"type":"string"},"label":{"type":"string"},"routes":{"type":"array","minItems":1,"items":{"type":"object","properties":{"role":{"type":"string"},"root":{"type":"string"}},"required":["role","root"],"additionalProperties":false}}},"required":["observation","workspace"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_remove_workspace",
        description: "Withdraw exactly one current workspace revision, including an unavailable or conflicted revision shown by config_inspect. Other concurrent revisions remain visible; no winner is guessed. Collections, data, sharing authority, identity and services remain unchanged. Requires the latest observation from config_inspect.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"workspace":{"type":"string"}},"required":["observation","workspace"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_set_settings",
        description: "Save the complete typed node settings after config_inspect. Explicitly supersedes observed settings forks; never guesses a winner. Peers are public node IDs used only as sync bootstrap hints, not access grants. Sync/maintenance apply while running; a changed MCP port takes effect after restarting Erlkonig and refreshing client integration. Port 0 requests an ephemeral local port. No keys, tokens, arbitrary paths or collection capabilities are accepted.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"mcp_port":{"type":"integer","minimum":0,"maximum":65535},"sync_enabled":{"type":"boolean"},"maintenance_enabled":{"type":"boolean"},"peers":{"type":"array","items":{"type":"string"}}},"required":["observation","mcp_port","sync_enabled","maintenance_enabled","peers"],"additionalProperties":false}"#,
    },
];

pub(crate) struct ConfigMcp {
    binding: NodeBinding,
    node: SharedNode,
    mcp_url: String,
    startup_port: u16,
}

impl ConfigMcp {
    pub(crate) fn new(
        binding: NodeBinding,
        node: SharedNode,
        mcp_url: String,
        startup_port: u16,
    ) -> Self {
        Self {
            binding,
            node,
            mcp_url,
            startup_port,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    role: String,
    root: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    observation: String,
    label: String,
    routes: Option<Vec<Route>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    observation: String,
    workspace: String,
    label: Option<String>,
    routes: Option<Vec<Route>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Remove {
    observation: String,
    workspace: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    observation: String,
    mcp_port: u16,
    sync_enabled: bool,
    maintenance_enabled: bool,
    peers: Vec<String>,
}

fn current(view: &ConfigView, observation: &str) -> Result<()> {
    if view.observation()? != observation {
        bail!("node configuration changed since inspection; call config_inspect before saving");
    }
    Ok(())
}

fn revision(value: &str) -> Result<Id> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(mcp::invalid_arguments(
            "workspace must be an exact 32-hex revision ID",
        ));
    }
    Id::from_hex(value).context("invalid workspace revision")
}

fn routes(discovery: &Discovery, values: Vec<Route>) -> Result<BTreeMap<Id, CollectionHandle>> {
    if values.is_empty() {
        bail!("choose at least one collection role");
    }
    let known = workspace::roles();
    let mut routes = BTreeMap::new();
    for value in values {
        let role = known
            .iter()
            .find(|(_, name)| *name == value.role)
            .map(|(role, _)| *role)
            .context("unknown faculty role; use config_list_collections")?;
        let root = parse_handle(&value.root)?;
        discovery.select(role, root)?;
        if routes.insert(role, root).is_some() {
            bail!("specify each faculty role once");
        }
    }
    Ok(routes)
}

pub(crate) fn workspace_json(workspace: &Workspace) -> Value {
    json!({
        "workspace": format!("{:X}", workspace.id), "label": workspace.label,
        "roles": workspace.routes.iter().map(|(role, root)| json!({
            "role": faculties::collection_names::name_for(*role),
            "role_id": format!("{role:X}"), "root": format!("blake3:{}", hex::encode(root.raw)),
        })).collect::<Vec<_>>(),
    })
}

fn settings_json(value: &NodeSettings) -> Value {
    json!({"mcp_port":value.mcp_port, "sync_enabled":value.sync_enabled,
        "maintenance_enabled":value.maintenance_enabled,
        "peers":value.peers.iter().map(hex::encode).collect::<Vec<_>>()})
}

impl Faculty for ConfigMcp {
    fn tools(&self) -> &[Tool] {
        TOOLS
    }

    fn call(&self, name: &str, arguments: Bytes, out: &mut Out<'_>) -> Result<()> {
        self.execute(name, arguments, out).map_err(|error| {
            // Native error chains may contain Config payload or text handles.
            // A handle is a bearer READ capability, not harmless diagnostics.
            // Preserve protocol argument classification, but not store causes.
            if error.is::<mcp::InvalidArguments>() {
                error
            } else {
                anyhow::anyhow!(error.to_string())
            }
        })
    }
}

impl ConfigMcp {
    fn execute(&self, name: &str, arguments: Bytes, out: &mut Out<'_>) -> Result<()> {
        let mut guard = self.node.lock().unwrap_or_else(|error| error.into_inner());
        let node = guard.as_mut().context("node store is closed")?;
        let view = node
            .config()
            .context("node configuration is unavailable; native data recovery is required")?;
        let result = match name {
            "config_inspect" => {
                let _: Empty = mcp::decode_arguments(arguments)?;
                let mut workspaces = Vec::new();
                let mut unavailable = Vec::new();
                for id in view.workspace_ids()? {
                    match view.workspace(id) {
                        Ok(Some(value)) => workspaces.push(workspace_json(&value)),
                        other => unavailable.push(json!({"workspace":format!("{id:X}"),
                            "reason": match other { Err(error) => error.to_string(),
                                _ => "workspace was removed or superseded".to_owned() }})),
                    }
                }
                // Typed inspection of the observed forks, not a raw fact dump.
                let facts = view.facts()?;
                let order = ObservationOrder::new(&facts, metadata::supersedes.id());
                let revisions = find!(state: Id, and!(
                    pattern!(&facts, [{ ?state @ config::anchor: attrs::NODE_SETTINGS }]),
                    maximal(state, &order),
                )).map(|state| json!({
                    "revision":format!("{state:X}"),
                    "mcp_ports":find!(port: u16, pattern!(&facts,[{state @ attrs::mcp_port: ?port}])).collect::<Vec<_>>(),
                    "sync_enabled":find!(value: bool, pattern!(&facts,[{state @ attrs::sync_enabled: ?value}])).collect::<Vec<_>>(),
                    "maintenance_enabled":find!(value: bool, pattern!(&facts,[{state @ attrs::maintenance_enabled: ?value}])).collect::<Vec<_>>(),
                    "peers":find!(peer: VerifyingKey, pattern!(&facts,[{state @ health_record::attrs::peer: ?peer}]))
                        .map(|peer| hex::encode(peer.to_bytes())).collect::<Vec<_>>(),
                })).collect::<Vec<_>>();
                let (settings, problem) = match view.settings() {
                    Ok(value) => (settings_json(&value), None),
                    Err(error) => (Value::Null, Some(error.to_string())),
                };
                json!({"node_id":hex::encode(self.binding.signer), "observation":view.observation()?,
                    "settings":settings, "settings_problem":problem, "settings_revisions":revisions,
                    "mcp_url":self.mcp_url,
                    "port_restart_required":view.settings().is_ok_and(|value| value.mcp_port != self.startup_port),
                    "workspaces":workspaces, "unavailable":unavailable})
            }
            "config_list_collections" => {
                let _: Empty = mcp::decode_arguments(arguments)?;
                let discovery = Discovery::from_snapshot(
                    view.observed.snapshot().clone(),
                    node.signer.verifying_key(),
                );
                let (roots, unavailable) = discovery.roots()?;
                let facts = view.facts()?;
                let roles = workspace::roles().into_iter().map(|(role, name)| {
                    let configured = match faculties::config::configured_in(&facts, role) {
                        Ok(root) => json!({"root":root.map(|root| format!("blake3:{}",hex::encode(root.raw)))}),
                        Err(error) => json!({"problem":error.to_string()}),
                    };
                    json!({"role":name, "role_id":format!("{role:X}"), "configured":configured})
                }).collect::<Vec<_>>();
                json!({"roles":roles, "collections":roots.into_iter().map(|root| json!({
                    "root":format!("blake3:{}",hex::encode(root.handle.raw)), "name":root.name,
                    "read":root.read, "write":root.write,
                })).collect::<Vec<_>>(), "unavailable_count":unavailable.len()})
            }
            "config_create_workspace" => {
                let args: Create = mcp::decode_arguments(arguments)?;
                current(&view, &args.observation)?;
                let id = if let Some(choices) = args.routes {
                    let discovery = Discovery::from_snapshot(
                        view.observed.snapshot().clone(),
                        node.signer.verifying_key(),
                    );
                    let mut value = Workspace::new(args.label.trim());
                    value.routes = routes(&discovery, choices)?;
                    node.put_workspace(&view, &value)?
                } else {
                    node.create_workspace(&view, &args.label)?
                };
                json!({"workspace":format!("{id:X}")})
            }
            "config_edit_workspace" => {
                let args: Edit = mcp::decode_arguments(arguments)?;
                current(&view, &args.observation)?;
                let mut value = view
                    .workspace(revision(&args.workspace)?)?
                    .context("workspace is no longer current; call config_inspect")?;
                if args.label.is_none() && args.routes.is_none() {
                    bail!("supply a label or complete replacement routes");
                }
                if let Some(label) = args.label {
                    value.label = label.trim().to_owned();
                }
                if let Some(choices) = args.routes {
                    let discovery = Discovery::from_snapshot(
                        view.observed.snapshot().clone(),
                        node.signer.verifying_key(),
                    );
                    value.routes = routes(&discovery, choices)?;
                }
                let id = node.put_workspace(&view, &value)?;
                json!({"workspace":format!("{id:X}")})
            }
            "config_remove_workspace" => {
                let args: Remove = mcp::decode_arguments(arguments)?;
                current(&view, &args.observation)?;
                let id = node.remove_workspace_revision(&view, revision(&args.workspace)?)?;
                json!({"withdrawal":format!("{id:X}"), "data_retained":true})
            }
            "config_set_settings" => {
                let args: Settings = mcp::decode_arguments(arguments)?;
                current(&view, &args.observation)?;
                let mut peers = Vec::new();
                for peer in args.peers {
                    let bytes: [u8; 32] = hex::decode(&peer)
                        .context("peer must be a full public node ID")?
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("peer must be a full public node ID"))?;
                    VerifyingKey::from_bytes(&bytes).context("invalid peer public node ID")?;
                    peers.push(bytes);
                }
                peers.sort_unstable();
                peers.dedup();
                let value = NodeSettings {
                    mcp_port: args.mcp_port,
                    sync_enabled: args.sync_enabled,
                    maintenance_enabled: args.maintenance_enabled,
                    peers,
                };
                let id = node.put_settings(&view, &value)?;
                json!({"revision":format!("{id:X}"), "settings":settings_json(&value),
                    "mcp_url":self.mcp_url, "port_restart_required":value.mcp_port != self.startup_port,
                    "port_activation":"The running endpoint is unchanged. MCP port changes require app restart, then Integrations > your client > Refresh package and client reload; an old-port profile cannot reconnect. Cowork requires a refreshed archive."})
            }
            _ => bail!("unknown node configuration tool {name:?}"),
        };
        out.text(serde_json::to_string(&result)?)
    }
}

#[cfg(test)]
mod tests;
