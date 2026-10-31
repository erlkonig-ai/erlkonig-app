//! Explicit operator bootstrap/inspection/migration. None of these entrypoints
//! constructs a Peer or starts app services. Plans are read-only.
use crate::{
    config::{NodeLocator, parse_handle},
    node_store::{ConfigView, NodeSettings, NodeStore},
    paths::Layout,
};
use anyhow::{Context, Result, bail};
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use serde_json::json;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
use triblespace::core::collection::{Collection, CollectionHandle, descriptor};
use triblespace::core::repo::pile::{Pile, PileFile, PileSnapshot};
use triblespace::core::repo::{BlobStoreGet, BlobStoreList, SnapshotSource};
use triblespace::prelude::*;

fn options(args: &[OsString]) -> Result<std::collections::BTreeMap<String, String>> {
    if args.len() % 2 != 0 {
        bail!("every option requires a value");
    }
    let mut result = std::collections::BTreeMap::new();
    for pair in args.chunks_exact(2) {
        let key = pair[0].to_str().context("option is not UTF-8")?;
        let value = pair[1].to_str().context("option value is not UTF-8")?;
        if result.insert(key.to_owned(), value.to_owned()).is_some() {
            bail!("duplicate option {key}");
        }
    }
    Ok(result)
}

fn public(text: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = hex::decode(text)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected a full 32-byte public key"))?;
    Ok(VerifyingKey::from_bytes(&bytes)?)
}

fn snapshot(path: &Path, signer: VerifyingKey) -> Result<PileSnapshot> {
    let file = PileFile::open_read_only(path)?;
    let mut pile = Pile::with_host(file, Some(signer));
    Ok(pile.snapshot()?)
}

/// Bind only existing prepared storage after validating its expected key and
/// interpretable settings. Retained old app files are never opened or deleted.
pub fn bind(layout: &Layout, locator: &NodeLocator) -> Result<()> {
    let binding = locator.binding()?;
    let signer = triblespace::core::signing_key_file::load_existing(&binding.key)?;
    if signer.verifying_key().to_bytes() != binding.signer {
        bail!("node locator signing identity mismatch");
    }
    let observed = snapshot(&binding.pile, signer.verifying_key())?;
    let handle = faculties::config::handle(signer.verifying_key());
    if observed.contains_blob(handle)? {
        let config = Collection::open(&observed, handle)?;
        ConfigView::from_snapshot(observed, config)?.settings()?;
    }
    crate::paths::ensure_private_dir(&layout.dir)?;
    crate::status::write_atomic(
        &layout.dir.join("node.json"),
        &serde_json::to_vec_pretty(locator)?,
    )?;
    Ok(())
}

/// Returns true when this is one of the explicit node operator commands.
pub fn run(args: &[OsString]) -> Result<bool> {
    let Some(command) = args.first().and_then(|arg| arg.to_str()) else {
        return Ok(false);
    };
    if command == "--help" {
        println!(
            "erlkonig public-key --key EXISTING_KEY\nerlkonig inspect-node --pile EXISTING_PILE --public PUBLIC_HEX\nerlkonig inspect-fd-events --pile EXISTING_PILE --public PUBLIC_HEX [--collection OBSERVED_METRICS_HANDLE]\n  Without --collection: list accessible recorded metrics descriptors only.\nerlkonig bind-node --data-dir APP_DIR --pile EXISTING_PILE --key EXISTING_KEY --expect-public PUBLIC_HEX\nerlkonig migration-plan --request REQUEST_JSON\nerlkonig migrate-settings --request REQUEST_JSON\nerlkonig mcp-stdio --data-dir APP_DIR --url http://127.0.0.1:PORT/"
        );
        return Ok(true);
    }
    if !matches!(
        command,
        "public-key" | "inspect-node" | "inspect-fd-events" | "bind-node" | "migration-plan" | "migrate-settings"
    ) {
        return Ok(false);
    }
    let mut opts = options(&args[1..])?;
    let mut get = |name: &str| opts.remove(name).with_context(|| format!("missing {name}"));
    match command {
        "inspect-fd-events" => {
            let path = PathBuf::from(get("--pile")?);
            let signer = public(&get("--public")?)?;
            let selected = opts.remove("--collection").as_deref().map(parse_handle).transpose()?;
            if !opts.is_empty() {
                bail!("unknown FD inspection options");
            }
            let observed = snapshot(&path, signer)
                .map_err(|_| anyhow::anyhow!("read-only node snapshot unavailable"))?;
            let report = crate::node_metrics::fd::reader::inspect(&observed, signer, selected)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "public-key" => {
            let key = PathBuf::from(get("--key")?);
            if !opts.is_empty() {
                bail!("unknown options: {:?}", opts.keys());
            }
            let key = triblespace::core::signing_key_file::load_existing(&key)?;
            println!("{}", hex::encode(key.verifying_key().to_bytes()));
        }
        "inspect-node" => {
            let path = PathBuf::from(get("--pile")?);
            let signer = public(&get("--public")?)?;
            if !opts.is_empty() {
                bail!("unknown options: {:?}", opts.keys());
            }
            let observed = snapshot(&path, signer)?;
            let mut seeds = Vec::new();
            let config_handle = faculties::config::handle(signer);
            let mut config_report =
                json!({"handle":hex::encode(config_handle.raw),"resident":false});
            if observed.contains_blob(config_handle)? {
                let config = Collection::open(&observed, config_handle)?;
                let view = ConfigView::from_snapshot(observed.clone(), config)?;
                let facts = view.facts()?;
                // Distinguish the default projection from explicit native
                // preferences before an operator prepares a key handoff.
                use crate::node_schema as attrs;
                use faculties::schemas::config::config as config_attrs;
                use triblespace::core::query::register::{ObservationOrder, maximal};
                let has_settings = exists!(
                    pattern!(&facts,[{_?state @ config_attrs::anchor:attrs::NODE_SETTINGS}])
                );
                let order =
                    ObservationOrder::new(&facts, triblespace::core::metadata::supersedes.id());
                let settings_heads=find!(state:Id,and!(
                    pattern!(&facts,[{?state @ config_attrs::anchor:attrs::NODE_SETTINGS}]),
                    maximal(state,&order),
                )).map(|state|json!({
                    "revision":format!("{state:X}"),
                    "ports":find!(port:u16,pattern!(&facts,[{state @ attrs::mcp_port:?port}])).collect::<Vec<_>>(),
                    "sync":find!(value:bool,pattern!(&facts,[{state @ attrs::sync_enabled:?value}])).collect::<Vec<_>>(),
                    "maintenance":find!(value:bool,pattern!(&facts,[{state @ attrs::maintenance_enabled:?value}])).collect::<Vec<_>>(),
                })).collect::<Vec<_>>();
                seeds.extend(find!(root: CollectionHandle, pattern!(&facts, [
                    { _?workspace @ triblespace::core::metadata::tag: crate::node_schema::WORKSPACE,
                        crate::node_schema::workspace_route: _?route },
                    { _?route @ faculties::schemas::config::config::selects: ?root },
                ])));
                let workspaces = view.workspace_ids()?.into_iter().map(|id| match view.workspace(id) {
                    Ok(Some(workspace)) => json!({"revision":format!("{id:X}"),"label":workspace.label,
                        "routes":workspace.routes.into_iter().map(|(role,root)|
                            (faculties::collection_names::name_for(role).unwrap_or("unknown").to_owned(),hex::encode(root.raw)))
                            .collect::<std::collections::BTreeMap<_,_>>() }),
                    Ok(None) => json!({"revision":format!("{id:X}"),"withdrawn":true}),
                    Err(error) => json!({"revision":format!("{id:X}"),"problem":format!("{error:#}")}),
                }).collect::<Vec<_>>();
                config_report = json!({"handle":hex::encode(config_handle.raw),"resident":true,
                    "has_settings":has_settings,"settings_heads":settings_heads,
                    "settings":match view.settings() {Ok(value)=>json!({"port":value.mcp_port,
                        "sync":value.sync_enabled,"maintenance":value.maintenance_enabled}),
                        Err(error)=>json!({"problem":format!("{error:#}")})},
                    "workspaces":workspaces});
            }
            let mut selection = crate::workspace::AllAccessible::discover(
                &observed,
                signer,
                seeds.iter().copied(),
            )?;
            selection.sync = crate::workspace::selected_sync_roots(&observed, signer)?;
            let mut named = Vec::new();
            for handle in crate::workspace::candidates(&observed, seeds)? {
                if let Ok(facts) = observed.get::<TribleSet, _>(handle) {
                    if let Ok(Some(name)) = descriptor::name(&facts) {
                        if let Ok(name) = observed.get::<View<str>, _>(name) {
                            named.push(
                                json!({"handle":hex::encode(handle.raw),"name":name.to_string()}),
                            );
                        }
                    }
                }
            }
            let handles = |values: Vec<CollectionHandle>| {
                values
                    .iter()
                    .map(|handle| hex::encode(handle.raw))
                    .collect::<Vec<_>>()
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"named":named,"config":config_report,
                "sync":handles(selection.sync),"maintenance":handles(selection.maintenance),
                "denied":handles(selection.denied),"pending":selection.pending}))?
            );
        }
        "bind-node" => {
            let layout = Layout::new(get("--data-dir")?);
            let locator = NodeLocator {
                pile: get("--pile")?.into(),
                key: get("--key")?.into(),
                expected_signer: get("--expect-public")?,
            };
            if !opts.is_empty() {
                bail!("unknown options: {:?}", opts.keys());
            }
            bind(&layout, &locator)?;
            println!("Bound existing node; no service started.");
        }
        _ => {
            let request: MigrationRequest =
                serde_json::from_slice(&std::fs::read(get("--request")?)?)?;
            if !opts.is_empty() {
                bail!("unknown options: {:?}", opts.keys());
            }
            let target = request.target.binding()?;
            let target_public = VerifyingKey::from_bytes(&target.signer)?;
            let source = snapshot(&request.source_pile, public(&request.source_public)?)?;
            let target_snapshot = snapshot(&target.pile, target_public)?;
            let accepted = request
                .accepted_workspace_sources
                .iter()
                .map(NodeLocator::binding)
                .collect::<Result<Vec<_>>>()?;
            let defaults = NodeSettings {
                mcp_port: request.mcp_port,
                sync_enabled: request.sync_enabled,
                maintenance_enabled: request.maintenance_enabled,
            };
            let mut plan = crate::migration::MigrationPlan::prepare(
                &source,
                parse_handle(&request.source_config)?,
                request
                    .source_metrics
                    .as_deref()
                    .map(parse_handle)
                    .transpose()?,
                &target_snapshot,
                &target,
                &accepted,
                &defaults,
            )?;
            for workspace in &request.workspaces {
                let mut routes = std::collections::BTreeMap::new();
                for (role, root) in &workspace.routes {
                    let role = faculties::collection_names::table()
                        .into_iter()
                        .find(|(_, name)| *name == role)
                        .map(|(id, _)| id)
                        .context("unknown explicit faculty role")?;
                    routes.insert(role, parse_handle(root)?);
                }
                let anchor = Id::from_hex(&workspace.anchor)
                    .context("workspace anchor must be an explicit minted ID")?;
                plan.add_workspace(
                    anchor,
                    &workspace.label,
                    &routes,
                    &target_snapshot,
                    target_public,
                )?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                "workspace_successors":plan.workspace_successors,"settings_successors":plan.settings_successors,
                "retained":plan.retained,"apply":command=="migrate-settings"}))?
            );
            if command == "migrate-settings" {
                let signer = triblespace::core::signing_key_file::load_existing(&target.key)?;
                if signer.verifying_key() != target_public {
                    bail!("target signing identity mismatch");
                }
                let mut node = NodeStore::open(&target.pile, signer)?;
                let expected = node.config()?;
                plan.apply(&mut node, &expected)?;
                node.close()?;
            }
        }
    }
    Ok(true)
}

/// Explicit preparation input, not persistent app configuration.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationRequest {
    pub source_pile: PathBuf,
    pub source_public: String,
    pub source_config: String,
    pub source_metrics: Option<String>,
    pub target: NodeLocator,
    #[serde(default)]
    pub accepted_workspace_sources: Vec<NodeLocator>,
    pub mcp_port: u16,
    pub sync_enabled: bool,
    pub maintenance_enabled: bool,
    #[serde(default)]
    pub workspaces: Vec<ExistingWorkspace>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExistingWorkspace {
    /// Mint once with trible genid and retain in this explicit request, so
    /// cancelling or retrying a prepared handoff does not invent a new bundle.
    pub anchor: String,
    pub label: String,
    pub routes: std::collections::BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removed_peer_field_is_not_accepted_by_handoff_requests() {
        let mut request = json!({
            "source_pile":"source.pile","source_public":"public","source_config":"config",
            "target":{"pile":"target.pile","key":"target.key","expected_signer":"public"},
            "mcp_port":0,"sync_enabled":false,"maintenance_enabled":false,
        });
        assert!(serde_json::from_value::<MigrationRequest>(request.clone()).is_ok());
        request["peers"] = json!([]);
        assert!(serde_json::from_value::<MigrationRequest>(request).is_err());
    }
    #[test]
    fn locator_is_explicit_persistent_and_never_replaces_an_identity() {
        let dir = tempfile::tempdir().unwrap();
        let app = Layout::new(dir.path().join("app"));
        std::fs::create_dir(&app.dir).unwrap();
        std::fs::write(app.dir.join("self.pile"), b"legacy rollback").unwrap();
        let application = Layout::application(&app.dir).unwrap();
        assert!(
            application.selected_node().is_err(),
            "legacy without locator must refuse"
        );
        let store = Layout::new(dir.path().join("store"));
        std::fs::create_dir(&store.dir).unwrap();
        let signer = crate::init::prepare(&store.bootstrap()).unwrap();
        let locator = NodeLocator {
            pile: store.default_pile(),
            key: store.default_key(),
            expected_signer: hex::encode(signer.verifying_key().to_bytes()),
        };
        let before = std::fs::read(&locator.pile).unwrap();
        bind(&app, &locator).unwrap();
        assert_eq!(
            std::fs::read(&locator.pile).unwrap(),
            before,
            "binding is read only"
        );
        let (selected, expected) = Layout::application(&app.dir)
            .unwrap()
            .selected_node()
            .unwrap();
        assert_eq!(selected.pile, locator.pile);
        assert_eq!(expected, Some(signer.verifying_key().to_bytes()));
        assert_eq!(
            std::fs::read(app.dir.join("self.pile")).unwrap(),
            b"legacy rollback"
        );
        let original = std::fs::read(app.dir.join("node.json")).unwrap();
        let wrong = NodeLocator {
            expected_signer: hex::encode(
                ed25519_dalek::SigningKey::from_bytes(&[1; 32])
                    .verifying_key()
                    .to_bytes(),
            ),
            ..locator
        };
        assert!(bind(&app, &wrong).is_err());
        assert_eq!(std::fs::read(app.dir.join("node.json")).unwrap(), original);
        let missing = NodeLocator {
            pile: dir.path().join("missing.pile"),
            ..wrong
        };
        assert!(bind(&app, &missing).is_err());
        assert!(!missing.pile.exists());
    }
    #[test]
    fn fresh_application_selects_canonical_without_io_and_bad_locator_fails() {
        let dir = tempfile::tempdir().unwrap();
        let app = Layout::application(dir.path()).unwrap();
        let (node, expected) = app.selected_node().unwrap();
        assert!(
            node.pile
                .ends_with(".local/share/triblespace/custody/self.pile")
        );
        assert!(expected.is_none());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
        std::fs::write(dir.path().join("node.json"), b"not JSON").unwrap();
        assert!(app.selected_node().is_err());
    }
    #[test]
    fn missing_public_key_never_generates() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("absent.key");
        assert!(
            run(&[
                OsString::from("public-key"),
                OsString::from("--key"),
                key.clone().into_os_string()
            ])
            .is_err()
        );
        assert!(!key.exists());
    }
}
