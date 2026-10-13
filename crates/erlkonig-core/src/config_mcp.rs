//! Node-scoped configuration on the app's existing authenticated local MCP.
//! No workspace is needed to configure the first one. These adapters retain
//! the resident owner, not a second settings store or a client selection.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use faculties::mcp::{self, Bytes, Faculty, Tool};
use faculties::out::Out;
use faculties::schemas::config::config;
use serde::Deserialize;
use serde_json::{Value, json};
use triblespace::core::collection::CollectionHandle;
use triblespace::core::metadata;
use triblespace::core::query::register::{ObservationOrder, maximal};
use triblespace::core::repo::{BlobStoreGet, SnapshotSource};
use triblespace::prelude::*;

use crate::config::{NodeBinding, parse_handle};
use crate::node_schema as attrs;
use crate::node_store::{ConfigView, NodeSettings};
use crate::runtime::{ServiceControl, SharedNode};
use crate::workspace::{self, Discovery, Workspace};

const TOOLS: &[Tool] = &[
    Tool {
        name: "config_retry_services",
        description: "Request one retry of stopped node sync/maintenance with the current settings. No settings or workspace are changed. A fresh resource guard must pass before startup; requests are coalesced and already-running services are left alone. queued means requested, not running: inspect runtime in config_inspect for the outcome. No workspace required.",
        input_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    },
    Tool {
        name: "config_inspect",
        description: "Inspect this running Erlkonig node's public identity, typed settings, workspace bundles and service runtime state. Runtime includes safe stop/retry reasons and cleanup failure codes, never raw local errors. No workspace required. Returns an opaque observation to pass unchanged on a configuration edit. Conflicted settings remain visible; no secrets, private keys, paths or raw Config facts are returned. Configuration is local administration, not a sharing grant or a global workspace switch.",
        input_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    },
    Tool {
        name: "config_list_collections",
        description: "Discover existing local collection roots, names and independent READ/WRITE admission, plus known faculty role names and existing Config mappings. No workspace required. This neither fetches remote data nor grants permission. Use exact roots returned here for a requested bundle; do not infer authority from a name or handle.",
        input_schema: r#"{"type":"object","properties":{},"additionalProperties":false}"#,
    },
    Tool {
        name: "config_create_workspace",
        description: "Create a named workspace in this node's one store with its existing identity. Omit routes to create a fresh private collection bundle, or supply a nonempty complete list of existing local role/root choices from config_list_collections. After successful workspace publication, newly requested roots are separately selected for native sync. A selection failure does not roll back publication: reinspect before retrying. No sharing grant, key, path, secret or global workspace default is changed. Pass the observation from config_inspect; returns the exact workspace revision for ordinary faculty calls.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"label":{"type":"string"},"routes":{"type":"array","minItems":1,"items":{"type":"object","properties":{"role":{"type":"string"},"root":{"type":"string"}},"required":["role","root"],"additionalProperties":false}}},"required":["observation","label"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_edit_workspace",
        description: "Edit an exact current workspace revision, never a label or prefix. Omitted label/routes are preserved; supplied routes replace the entire bundle with nonempty existing local role/root choices. After workspace publication, newly added roots are separately selected for native sync; a label-only edit preserves native unselection/conflicts. Selection failure does not roll back publication: reinspect before retrying. Old workspace IDs become unavailable, never redirected. Stale observations or conflicted revisions are rejected: inspect again and explicitly resolve the choice. No sharing permissions change.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"workspace":{"type":"string"},"label":{"type":"string"},"routes":{"type":"array","minItems":1,"items":{"type":"object","properties":{"role":{"type":"string"},"root":{"type":"string"}},"required":["role","root"],"additionalProperties":false}}},"required":["observation","workspace"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_remove_workspace",
        description: "Withdraw exactly one current workspace revision, including an unavailable or conflicted revision shown by config_inspect. Other concurrent revisions remain visible; no winner is guessed. Collections, data, sharing authority, native sync selections, identity and services remain unchanged. Requires the latest observation from config_inspect.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"workspace":{"type":"string"}},"required":["observation","workspace"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_share_workspace",
        description: "Share an exact current workspace revision's resolved collection bundle with an exact recipient public node identity. access is read_only or read_write; delegate defaults false (invoke-only). Native grants are valid immediately, without recipient approval or acknowledgement. Returns bounded signed role/root/descriptor-name metadata, never proof or definition payload. Native grant exchange delivers capability evidence independently of this metadata handoff; grants alone neither enroll nor select collections locally. Only these roots/actions are granted: no old or future membership, settings, keys or global workspace selection. Before choosing delegate=true, explain and obtain the user's choice: the recipient may invite additional participants within these roots/actions without the original user choosing each one. Disclosed or copied data cannot be made unread. Native per-collection authority must support every requested grant; reader/writer access alone does not confer onward delegation. Requires the latest config_inspect observation.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"workspace":{"type":"string"},"recipient":{"type":"string"},"access":{"type":"string","enum":["read_only","read_write"]},"delegate":{"type":"boolean","default":false}},"required":["observation","workspace","recipient","access"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_probe_workspace_share",
        description: "Read-only verification of bounded signed workspace metadata on its intended recipient node. Checks the native signature and recipient binding; reports source label, exact descriptor names/roles/roots and the issuer's declared access/delegation choice. Actual native collection admission is NOT confirmed by this probe; enrolled known roots acquire evidence through native repair. Does not import evidence, enroll a workspace, activate repair, download payloads or start maintenance. Explain contents and resource consequences, then let the recipient user/agent choose whether to enroll and a local label. Rights already exist; probing/enrollment is not an authorization handshake and sends no reply to the source.",
        input_schema: r#"{"type":"object","properties":{"bootstrap":{"type":"string","maxLength":4194304}},"required":["bootstrap"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_enroll_workspace_share",
        description: "After recipient-local consent, enroll a verified one-way bootstrap under a chosen nonempty local label. Imports descriptor/name metadata and configures the SAME exact roots/role routes under the existing node key; never imports proofs, copies signing keys or creates replacement data roots. Workspace publication completes first; newly requested roots are then separately selected for enabled native Full sync, even while local READ evidence is pending. Selection failure does not roll back publication: reinspect before retrying. Native grant exchange and participant discovery deliver authority evidence; native admission governs local reads/writes and upkeep. Sync follows the native Selected relation, not arbitrary grants; maintenance remains independently all-accessible and may acquire exact missing bodies while sync is enabled. No global workspace default or authorization handshake is created. Explain additive non-expiring rights, possible onward invitations when delegate=true, and that disclosed/copied data cannot be made unread. Repeat enrollment never overwrites a later edit/withdrawal. Removing local enrollment neither revokes grants nor unselects native roots. Returns local publication only, not synchronization completion. Requires the latest config_inspect observation.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"bootstrap":{"type":"string","maxLength":4194304},"label":{"type":"string","minLength":1}},"required":["observation","bootstrap","label"],"additionalProperties":false}"#,
    },
    Tool {
        name: "config_set_settings",
        description: "Save the complete typed port, sync and maintenance settings after config_inspect. Explicitly supersedes observed settings forks; never guesses a winner. Native collection selection and participant discovery are independent; no peer bootstrap setting exists. Sync/maintenance apply while running; a changed MCP port takes effect after restarting Erlkonig and refreshing client integration. Port 0 requests an ephemeral local port. No keys, tokens, arbitrary paths or collection capabilities are accepted.",
        input_schema: r#"{"type":"object","properties":{"observation":{"type":"string"},"mcp_port":{"type":"integer","minimum":0,"maximum":65535},"sync_enabled":{"type":"boolean"},"maintenance_enabled":{"type":"boolean"}},"required":["observation","mcp_port","sync_enabled","maintenance_enabled"],"additionalProperties":false}"#,
    },
];

pub(crate) struct ConfigMcp {
    binding: NodeBinding,
    node: SharedNode,
    mcp_url: String,
    startup_port: u16,
    services: ServiceControl,
}

impl ConfigMcp {
    pub(crate) fn new(
        binding: NodeBinding,
        node: SharedNode,
        mcp_url: String,
        startup_port: u16,
        services: ServiceControl,
    ) -> Self {
        Self {
            binding,
            node,
            mcp_url,
            startup_port,
            services,
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Share {
    observation: String,
    workspace: String,
    recipient: String,
    access: crate::workspace_sharing::Access,
    #[serde(default)]
    delegate: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeShare {
    bootstrap: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollShare {
    observation: String,
    bootstrap: String,
    label: String,
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
        "maintenance_enabled":value.maintenance_enabled})
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
            "config_retry_services" => {
                let _: Empty = mcp::decode_arguments(arguments)?;
                let _ = view.settings()?;
                json!({"queued":self.services.retry()?, "settings_changed":false})
            }
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
                })).collect::<Vec<_>>();
                let (settings, problem) = match view.settings() {
                    Ok(value) => (settings_json(&value), None),
                    Err(error) => (Value::Null, Some(error.to_string())),
                };
                let status = self.services.status.lock().unwrap_or_else(|e| e.into_inner());
                json!({"node_id":hex::encode(self.binding.signer), "observation":view.observation()?,
                    "settings":settings, "settings_problem":problem, "settings_revisions":revisions,
                    "mcp_url":self.mcp_url,
                    // Raw session errors can include paths or capability handles.
                    // Keep their details in the local UI, not MCP inspection.
                    "runtime":{"services":status.node.services,
                        "sync_running":status.node.sync_running,
                        "maintenance_running":status.node.maintenance_running,
                        "stop_reason":status.node.stop_reason,
                        "retry_stop_reason":status.node.retry_stop_reason,
                        "session_failed":status.node.problem.is_some(),
                        "cleanup_failures":status.node.cleanup_failures,
                        "retry_pending":status.node.retry_pending,
                        "retry_available":status.config_problem.is_none()
                            && status.node.services == crate::status::ServiceState::Stopped},
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
            "config_share_workspace" => {
                let args: Share = mcp::decode_arguments(arguments)?;
                current(&view, &args.observation)?;
                let bootstrap = node.share_workspace(
                    &view, revision(&args.workspace)?,
                    crate::workspace_sharing::public_identity(&args.recipient)?,
                    args.access, args.delegate,
                )?;
                json!({"bootstrap":bootstrap, "workspace":args.workspace,
                    "recipient":args.recipient, "delegate":args.delegate,
                    "issuer_routing_hint":hex::encode(node.signer.verifying_key().to_bytes()),
                    "handoff":"signed_workspace_metadata_only", "publication":"local_native_grants", "sync_complete":false})
            }
            "config_probe_workspace_share" => {
                let args: ProbeShare = mcp::decode_arguments(arguments)?;
                let mut prepared = node.probe_workspace_share(&args.bootstrap)?;
                let snapshot = prepared.store.snapshot()?;
                let mut collections = Vec::new();
                for (role, root) in &prepared.workspace.routes {
                    let facts: TribleSet = snapshot.get(*root)?;
                    let mut names = Vec::new();
                    for name in find!(name: Inline<inlineencodings::Handle<blobencodings::UTF8String>>, pattern!(&facts, [{
                        metadata::tag: triblespace::core::collection::records::KIND_COLLECTION_DESCRIPTOR,
                        triblespace::core::collection::records::collection_name: ?name,
                    }])) {
                        let name: View<str> = snapshot.get(name)?;
                        names.push(name.to_string());
                    }
                    names.sort(); names.dedup();
                    collections.push(json!({"role":faculties::collection_names::name_for(*role),
                        "role_id":format!("{role:X}"),"root":format!("blake3:{}",hex::encode(root.raw)),"names":names}));
                }
                json!({"source_workspace":format!("{:X}",prepared.source),
                    "source_label":prepared.workspace.label, "issuer":hex::encode(prepared.issuer.to_bytes()),
                    "recipient":hex::encode(self.binding.signer), "collections":collections,
                    "access":if prepared.write { "read_write" } else { "read_only" },
                    "delegate":prepared.delegate, "metadata_verified":true, "recipient_bound":true,
                    "native_admission":"not_confirmed_by_probe", "issuer_is_routing_hint_not_authority":true,
                    "enrollment":"not_changed_by_probe", "read_only_probe":true})
            }
            "config_enroll_workspace_share" => {
                let args: EnrollShare = mcp::decode_arguments(arguments)?;
                current(&view, &args.observation)?;
                let accepted = node.enroll_workspace_share(&view, &args.bootstrap, &args.label)?;
                json!({"workspace":workspace_json(&accepted.workspace),
                    "source_workspace":format!("{:X}",accepted.source),
                    "access":if accepted.write { "read_write" } else { "read_only" },
                    "delegate":accepted.delegate, "already_enrolled":accepted.already_enrolled,
                    "publication":"local_workspace", "sync_complete":false, "native_admission":"requires_native_acquired_evidence",
                    "payloads":"not transferred by enrollment; selected roots use existing enabled node sync"})
            }
            "config_set_settings" => {
                let args: Settings = mcp::decode_arguments(arguments)?;
                current(&view, &args.observation)?;
                let value = NodeSettings {
                    mcp_port: args.mcp_port,
                    sync_enabled: args.sync_enabled,
                    maintenance_enabled: args.maintenance_enabled,
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
