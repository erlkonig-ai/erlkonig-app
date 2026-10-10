use super::*;
use crate::{
    config::Bootstrap, init, node_store::NodeStore, workspace_dispatch::WorkspaceDispatch,
};
use faculties::out::Part;
use std::sync::{Arc, Mutex};
use triblespace::core::collection::{CollectionRecord, CollectionStore, CollectionStoreExt};
use triblespace::core::repo::{BlobStorePut, CapabilityProofRead, SnapshotSource, StorageFlush};

struct Fixture {
    config: ConfigMcp,
    dispatch: WorkspaceDispatch,
    _dir: tempfile::TempDir,
}

#[test]
fn runtime_inspection_redacts_errors_and_retry_does_not_edit_config() {
    use crate::status::{CleanupFailure, ServiceState, Status};
    let mut f = Fixture::new();
    let status = Arc::new(Mutex::new(Status::default()));
    {
        let mut state = status.lock().unwrap();
        state.node.services = ServiceState::Stopped;
        state.node.stop_reason = Some("service settings changed".into());
        state.node.problem = Some("private path /fixture/self.pile and blake3:CAPABILITY".into());
        state.node.cleanup_failures = vec![CleanupFailure::EndpointShutdownTimeout];
    }
    let (stop, _) = tokio::sync::watch::channel(false);
    let (control, requests) = ServiceControl::new(status.clone(), stop.clone());
    f.config.services = control;
    let before = f.inspect();
    assert_eq!(before["runtime"]["services"], "stopped");
    assert_eq!(before["runtime"]["cleanup_failures"], json!(["endpoint_shutdown_timeout"]));
    assert_eq!(before["runtime"]["session_failed"], true);
    assert!(!before.to_string().contains("CAPABILITY"));
    assert!(!before.to_string().contains("/fixture"));
    assert_eq!(f.call("config_retry_services", json!({})).unwrap()["queued"], true);
    assert_eq!(f.call("config_retry_services", json!({})).unwrap()["queued"], true);
    assert!(requests.try_recv().is_ok());
    assert!(requests.try_recv().is_err(), "repeated retry coalesces");
    assert_eq!(f.inspect()["observation"], before["observation"]);
    {
        let mut state = status.lock().unwrap();
        state.node.services = ServiceState::Running;
        state.node.retry_pending = false;
    }
    assert_eq!(f.call("config_retry_services", json!({})).unwrap()["queued"], false);
    assert!(requests.try_recv().is_err());
    stop.send_replace(true);
    assert!(f.call("config_retry_services", json!({})).is_err());
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = Bootstrap::new(dir.path());
        let signer = init::prepare(&bootstrap).unwrap();
        let binding = NodeBinding {
            pile: bootstrap.pile.clone(),
            key: bootstrap.key,
            signer: signer.verifying_key().to_bytes(),
        };
        let node = Arc::new(Mutex::new(Some(
            NodeStore::open(&bootstrap.pile, signer).unwrap(),
        )));
        let reader = node.clone();
        let dispatch = WorkspaceDispatch::new(binding.clone(), move || {
            reader.lock().unwrap().as_mut().unwrap().config()
        })
        .unwrap();
        let (stop, _) = tokio::sync::watch::channel(false);
        let (services, _) = ServiceControl::new(Default::default(), stop);
        Self {
            config: ConfigMcp::new(binding, node, "http://127.0.0.1:41773/".into(), 41773, services),
            dispatch,
            _dir: dir,
        }
    }

    fn call(&self, name: &str, args: Value) -> Result<Value> {
        serde_json::from_str(&call(&self.config, name, args)?).map_err(Into::into)
    }

    fn inspect(&self) -> Value {
        self.call("config_inspect", json!({})).unwrap()
    }

    fn create(&self, label: &str) -> Value {
        self.call(
            "config_create_workspace",
            json!({"observation":self.inspect()["observation"],"label":label}),
        )
        .unwrap()["workspace"]
            .clone()
    }

    fn annotate(&self, fragment: Fragment) {
        let mut guard = self.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        node.pile
            .commit(node.config, &node.signer, fragment)
            .unwrap();
        node.pile.flush().unwrap();
    }

    fn proofs(&self) -> usize {
        self.config
            .node
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .pile
            .snapshot()
            .unwrap()
            .proofs()
            .unwrap()
            .count()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.dispatch.close().unwrap();
        self.config
            .node
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .close()
            .unwrap();
    }
}

fn call(faculty: &impl Faculty, name: &str, args: Value) -> Result<String> {
    let mut text = String::new();
    faculty.call(
        name,
        Bytes::from(serde_json::to_vec(&args)?),
        &mut Out::new(&mut |part| {
            if let Part::Text { text: value } = part {
                text.push_str(&value);
            }
            Ok(())
        }),
    )?;
    Ok(text)
}

#[test]
fn zero_workspaces_bootstrap_without_paths_keys_tokens_or_grants() {
    let f = Fixture::new();
    let before = f.inspect();
    assert_eq!(before["workspaces"], json!([]));
    assert_eq!(before["node_id"], hex::encode(f.config.binding.signer));
    let encoded = before.to_string();
    assert!(!encoded.contains(f.config.binding.key.to_str().unwrap()));
    assert!(!encoded.contains(f.config.binding.pile.to_str().unwrap()));
    let key = triblespace::core::signing_key_file::load_existing(&f.config.binding.key).unwrap();
    assert!(!encoded.contains(&hex::encode(key.to_bytes())));
    assert!(!encoded.contains("token"));
    let view = f
        .config
        .node
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .config()
        .unwrap();
    for member in view.observed.support().unwrap().members() {
        assert!(
            !encoded.contains(&hex::encode(member.raw)),
            "no Config blob bearer handles"
        );
    }
    let proofs = f.proofs();
    let id = f.create("First");
    assert_eq!(
        f.proofs(),
        proofs,
        "workspace creation issues no sharing grant"
    );
    assert!(call(&f.dispatch, "compass_list", json!({})).is_err());
    call(
        &f.dispatch,
        "compass_add",
        json!({"workspace":id,"title":"first task"}),
    )
    .unwrap();
    assert!(
        call(&f.dispatch, "compass_list", json!({"workspace":id}))
            .unwrap()
            .contains("first task")
    );
    assert_eq!(f.inspect()["workspaces"][0]["workspace"], id);
    for tool in f.config.tools() {
        let schema: Value = serde_json::from_str(tool.input_schema).unwrap();
        assert_eq!(schema["additionalProperties"], false);
        for forbidden in ["key", "token", "path", "pile", "secret"] {
            assert!(schema["properties"].get(forbidden).is_none());
        }
    }
}

#[test]
fn missing_label_is_redacted_and_arrival_invalidates_stateless_observation() {
    let f = Fixture::new();
    let mut source = MemoryRepo::default();
    let label = source
        .put::<blobencodings::UTF8String, _>("Arrived later")
        .unwrap();
    let item = genid();
    f.annotate(
        entity! { &item @ metadata::tag:attrs::WORKSPACE, config::anchor:item.id,
        attrs::enrolled:true, metadata::name:label },
    );
    let before = f.inspect();
    assert_eq!(before["unavailable"].as_array().unwrap().len(), 1);
    assert!(!before.to_string().contains(&hex::encode(label.raw)));
    assert!(
        !call(&f.dispatch, "list_workspaces", json!({}))
            .unwrap()
            .contains(&hex::encode(label.raw))
    );
    let error = call(
        &f.dispatch,
        "compass_list",
        json!({"workspace":format!("{:X}",item.id)}),
    )
    .unwrap_err();
    assert!(!format!("{error:#}").contains(&hex::encode(label.raw)));
    {
        let mut guard = f.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        node.pile
            .put::<blobencodings::UTF8String, _>("Arrived later")
            .unwrap();
        node.pile.flush().unwrap();
    }
    let bytes = std::fs::read(&f.config.binding.pile).unwrap();
    let error = f
        .call(
            "config_remove_workspace",
            json!({"observation":before["observation"],"workspace":format!("{:X}",item.id)}),
        )
        .unwrap_err();
    assert!(error.to_string().contains("changed since inspection"));
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), bytes);
    assert_ne!(f.inspect()["observation"], before["observation"]);
}

#[test]
fn missing_config_payload_error_never_exposes_a_bearer_handle() {
    let f = Fixture::new();
    let handle = {
        let mut guard = f.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let mut source = MemoryRepo::default();
        let commit = source
            .commit(
                node.config,
                &node.signer,
                entity! {
                    config::anchor:attrs::NODE_SETTINGS,attrs::mcp_port:0_u16,
                    attrs::sync_enabled:false,attrs::maintenance_enabled:false,
                },
            )
            .unwrap();
        node.pile.insert(CollectionRecord::Commit(commit)).unwrap();
        commit.data()
    };
    let error = f.call("config_inspect", json!({})).unwrap_err();
    assert!(error.to_string().contains("configuration is unavailable"));
    assert!(!format!("{error:#}").contains(&hex::encode(handle.raw)));
    let error = call(&f.dispatch, "list_workspaces", json!({})).unwrap_err();
    assert!(!format!("{error:#}").contains(&hex::encode(handle.raw)));
}

#[test]
fn stale_invalid_edits_append_nothing_and_old_revisions_never_retarget() {
    let f = Fixture::new();
    let old = f.inspect()["observation"].clone();
    let id = f.create("Before");
    let before = std::fs::read(&f.config.binding.pile).unwrap();
    assert!(
        f.call(
            "config_edit_workspace",
            json!({"observation":old,"workspace":id,"label":"stale"})
        )
        .is_err()
    );
    let observation = f.inspect()["observation"].clone();
    for args in [
        json!({"observation":observation,"workspace":id,"label":" "}),
        json!({"observation":observation,"workspace":"Before","label":"invalid"}),
        json!({"observation":observation,"workspace":id,"routes":[]}),
        json!({"observation":observation,"workspace":id,"key":"untrusted"}),
    ] {
        assert!(f.call("config_edit_workspace", args).is_err());
    }
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), before);
    let next = f
        .call(
            "config_edit_workspace",
            json!({"observation":observation,"workspace":id,"label":"After"}),
        )
        .unwrap()["workspace"]
        .clone();
    assert_ne!(next, id);
    assert!(call(&f.dispatch, "compass_list", json!({"workspace":id})).is_err());
    call(&f.dispatch, "compass_list", json!({"workspace":next})).unwrap();
    let proofs = f.proofs();
    f.call(
        "config_remove_workspace",
        json!({"observation":f.inspect()["observation"],"workspace":next}),
    )
    .unwrap();
    assert!(call(&f.dispatch, "compass_list", json!({"workspace":next})).is_err());
    assert_eq!(f.inspect()["workspaces"], json!([]));
    assert_eq!(f.proofs(), proofs);
}

#[test]
fn existing_roots_use_explicit_roles_admission_and_never_grant_access() {
    let f = Fixture::new();
    let first = f.create("Origin");
    let inspected = f.inspect();
    let roles = inspected["workspaces"][0]["roles"].as_array().unwrap();
    let root = roles.iter().find(|role| role["role"] == "compass").unwrap()["root"].clone();
    let listed = f.call("config_list_collections", json!({})).unwrap();
    assert!(
        listed["collections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|choice| choice["root"] == root
                && choice["read"] == true
                && choice["write"] == true)
    );
    let proofs = f.proofs();
    let shared = f.call("config_create_workspace",json!({"observation":inspected["observation"],"label":"Same goals","routes":[{"role":"compass","root":root}]})).unwrap()["workspace"].clone();
    call(
        &f.dispatch,
        "compass_add",
        json!({"workspace":shared,"title":"same exact root"}),
    )
    .unwrap();
    assert!(
        call(&f.dispatch, "compass_list", json!({"workspace":first}))
            .unwrap()
            .contains("same exact root")
    );
    assert!(call(&f.dispatch, "wiki_list", json!({"workspace":shared})).is_err());
    assert_eq!(f.proofs(), proofs);
    let denied = {
        let mut guard = f.config.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[72; 32]).verifying_key();
        let collection = node
            .pile
            .collection(
                "not yours",
                faculties::collection_names::private_policy(stranger),
            )
            .unwrap();
        node.pile.flush().unwrap();
        format!("blake3:{}", hex::encode(collection.handle().raw))
    };
    let before = std::fs::read(&f.config.binding.pile).unwrap();
    assert!(f.call("config_create_workspace",json!({"observation":f.inspect()["observation"],"label":"Denied","routes":[{"role":"compass","root":denied}]})).is_err());
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), before);
}

#[test]
fn settings_conflicts_are_inspected_and_explicitly_reconciled() {
    let f = Fixture::new();
    for port in [12123_u16, 12124] {
        f.annotate(
            entity! { config::anchor:attrs::NODE_SETTINGS, attrs::mcp_port:port,
            attrs::sync_enabled:true, attrs::maintenance_enabled:false },
        );
    }
    let inspected = f.inspect();
    assert!(inspected["settings"].is_null());
    assert!(
        inspected["settings_problem"]
            .as_str()
            .unwrap()
            .contains("conflicting")
    );
    assert_eq!(inspected["settings_revisions"].as_array().unwrap().len(), 2);
    let before = std::fs::read(&f.config.binding.pile).unwrap();
    let mut args = json!({"observation":inspected["observation"],"mcp_port":0,"sync_enabled":false,"maintenance_enabled":true,"peers":["invalid"]});
    assert!(f.call("config_set_settings", args.clone()).is_err());
    assert_eq!(std::fs::read(&f.config.binding.pile).unwrap(), before);
    args["peers"] = json!([hex::encode(
        ed25519_dalek::SigningKey::from_bytes(&[45; 32])
            .verifying_key()
            .to_bytes()
    )]);
    let saved = f.call("config_set_settings", args.clone()).unwrap();
    assert_eq!(saved["mcp_url"], "http://127.0.0.1:41773/");
    assert_eq!(saved["port_restart_required"], true);
    let now = f.inspect();
    assert!(now["settings_problem"].is_null());
    assert_eq!(now["settings"]["sync_enabled"], false);
    assert_eq!(now["settings"]["maintenance_enabled"], true);
    assert_eq!(now["settings"]["peers"], args["peers"]);
    assert!(
        f.call("config_set_settings", args).is_err(),
        "stale complete settings rejected"
    );
}

#[test]
fn conflicting_workspace_is_not_edited_and_exact_withdrawal_stays_available() {
    let f = Fixture::new();
    let workspace = f.create("Before");
    let id = revision(workspace.as_str().unwrap()).unwrap();
    let anchor = f
        .config
        .node
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .config()
        .unwrap()
        .workspace(id)
        .unwrap()
        .unwrap()
        .anchor;
    f.annotate(
        entity! { metadata::tag:attrs::WORKSPACE, config::anchor:anchor,
        attrs::enrolled:true, metadata::name:"Concurrent" },
    );
    let inspected = f.inspect();
    assert_eq!(inspected["unavailable"].as_array().unwrap().len(), 2);
    assert!(
        f.call(
            "config_edit_workspace",
            json!({"observation":inspected["observation"],"workspace":workspace,"label":"guess"})
        )
        .is_err()
    );
    f.call(
        "config_remove_workspace",
        json!({"observation":inspected["observation"],"workspace":workspace}),
    )
    .unwrap();
    let now = f.inspect();
    assert_eq!(
        now["unavailable"].as_array().unwrap().len(),
        1,
        "withdrawal does not elect another fork"
    );
}

#[test]
fn independent_concurrent_clients_never_share_a_workspace_selection() {
    let f = Fixture::new();
    let a = f.create("A");
    let b = f.create("B");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers = [(a.clone(), "only alpha"), (b.clone(), "only beta")]
        .into_iter()
        .map(|(workspace, title)| {
            let node = f.config.node.clone();
            let binding = f.config.binding.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let dispatch = WorkspaceDispatch::new(binding, move || {
                    node.lock().unwrap().as_mut().unwrap().config()
                })
                .unwrap();
                barrier.wait();
                call(
                    &dispatch,
                    "compass_add",
                    json!({"workspace":workspace,"title":title}),
                )
                .unwrap();
                dispatch.close().unwrap();
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    let a = call(&f.dispatch, "compass_list", json!({"workspace":a})).unwrap();
    let b = call(&f.dispatch, "compass_list", json!({"workspace":b})).unwrap();
    assert!(a.contains("only alpha") && !a.contains("only beta"));
    assert!(b.contains("only beta") && !b.contains("only alpha"));
}
