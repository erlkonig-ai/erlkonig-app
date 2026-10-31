use super::*;
use crate::node_store::NodeStore;
use faculties::out::Part;
use std::fs;
use std::sync::{Arc, Mutex};
use triblespace::core::collection::{AdmissionPolicy, CollectionPolicy, CollectionStoreExt};
use triblespace::core::{id::genid, signing_key_file};
use triblespace::prelude::*;

struct Fixture {
    dispatch: WorkspaceDispatch,
    node: Arc<Mutex<Option<NodeStore>>>,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let node_path = dir.path().join("node.pile");
        fs::File::create(&node_path).unwrap();
        let key = dir.path().join("node.key");
        let signer = signing_key_file::init(&key).unwrap();
        let binding = NodeBinding {
            pile: node_path.clone(),
            key,
            signer: signer.verifying_key().to_bytes(),
        };
        let node = Arc::new(Mutex::new(Some(
            NodeStore::open(&node_path, signer).unwrap(),
        )));
        let reader = node.clone();
        let dispatch = WorkspaceDispatch::new(binding, move || {
            reader.lock().unwrap().as_mut().unwrap().config()
        })
        .unwrap();
        Self {
            dispatch,
            node,
            dir,
        }
    }

    fn configure(&self, workspace: &mut Workspace) {
        let mut guard = self.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        workspace.id = node.put_workspace(&view, workspace).unwrap();
    }

    fn workspace(&self, label: &str) -> Workspace {
        let mut guard = self.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        let revision = node.create_workspace(&view, label).unwrap();
        node.config().unwrap().workspace(revision).unwrap().unwrap()
    }

    fn call(&self, name: &str, arguments: Value) -> Result<String> {
        self.raw(name, &serde_json::to_string(&arguments).unwrap())
    }

    fn annotate(&self, fragment: Fragment) {
        let mut guard = self.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let signer = node.signer.clone();
        let config = faculties::collection_names::open(
            &mut node.pile,
            faculties::schemas::config::DEFAULT_SCOPE_ID,
            signer.verifying_key(),
        )
        .unwrap();
        node.pile.commit(config, &signer, fragment).unwrap();
    }

    fn raw(&self, name: &str, arguments: &str) -> Result<String> {
        let mut text = String::new();
        self.dispatch.call(
            name,
            Bytes::from(arguments.as_bytes().to_vec()),
            &mut Out::new(&mut |part| {
                if let Part::Text { text: value } = part {
                    text.push_str(&value);
                }
                Ok(())
            }),
        )?;
        Ok(text)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.dispatch.close().unwrap();
        if let Some(node) = self.node.lock().unwrap().take() {
            node.close().unwrap();
        }
    }
}

fn selection(workspace: &Workspace) -> Value {
    json!({"workspace": format!("{:X}", workspace.id)})
}

fn local_entry_args(workspace: &Workspace) -> Value {
    json!({"workspace": format!("{:X}", workspace.id), "service": "Synthetic service", "name": "fixture login"})
}

fn take_local_prompt(bridge: &SecretEntry) -> crate::secret_entry::SecretPrompt {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(prompt) = bridge.take_prompt() { return prompt; }
        assert!(std::time::Instant::now() < deadline, "local prompt was not delivered");
        std::thread::yield_now();
    }
}

#[test]
fn local_entry_missing_route_rejects_before_key_or_pile_io() {
    let f = Fixture::new();
    let mut workspace = f.workspace("No secrets");
    workspace.routes.remove(&faculties::secrets::DEFAULT_SCOPE_ID);
    f.configure(&mut workspace);
    let view = f.node.lock().unwrap().as_mut().unwrap().config().unwrap();
    let absent = f.dir.path().join("absent");
    let bridge = SecretEntry::default();
    let dispatch = WorkspaceDispatch::new(NodeBinding {
        pile: absent.join("node.pile"), key: absent.join("node.key"), signer: f.dispatch.node.signer,
    }, move || Ok(view.clone())).unwrap().with_secret_entry(bridge.clone());
    let error = rpc_call(&mut session(&dispatch), ENTER_LOCAL.name, local_entry_args(&workspace));
    assert!(error.to_string().contains("secrets is unconfigured"));
    assert!(dispatch.owners.borrow().is_empty());
    assert!(bridge.take_prompt().is_none());
    assert!(!absent.exists());
}

#[test]
fn local_entry_cancel_publishes_nothing_and_never_accepts_secret_arguments() {
    let mut f = Fixture::new();
    let workspace = f.workspace("Cancel fixture");
    let bridge = SecretEntry::default();
    f.dispatch.secret_entry = Some(bridge.clone());
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    let worker = std::thread::spawn(move || {
        let prompt = take_local_prompt(&bridge);
        assert_eq!(prompt.request.kind, EntryKind::Login);
        prompt.answer(EntryAnswer::Cancelled);
    });
    let response = f.call(ENTER_LOCAL.name, local_entry_args(&workspace)).unwrap();
    worker.join().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&response).unwrap(), json!({"status":"cancelled"}));
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
    let mut arguments = local_entry_args(&workspace);
    arguments["value"] = json!("invented input that must be rejected");
    assert!(f.call(ENTER_LOCAL.name, arguments).is_err());
    assert!(f.call("secrets_list", selection(&workspace)).unwrap().contains("(no secrets)"));
}

#[test]
fn local_entry_write_denied_route_never_requests_plaintext() {
    let mut f = Fixture::new();
    let mut workspace = f.workspace("Not writable");
    let other = signing_key_file::init(&f.dir.path().join("synthetic-other.key")).unwrap();
    {
        // Reuse the existing node owner, not another opener alongside it.
        let mut guard = f.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let root = node.pile.collection("Synthetic denied Secrets", CollectionPolicy::new(
            AdmissionPolicy::Open, AdmissionPolicy::direct(other.verifying_key()),
        )).unwrap().handle();
        workspace.routes.insert(faculties::secrets::DEFAULT_SCOPE_ID, root);
    }
    f.configure(&mut workspace);
    let bridge = SecretEntry::default();
    f.dispatch.secret_entry = Some(bridge.clone());
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    let error = f.call(ENTER_LOCAL.name, local_entry_args(&workspace)).unwrap_err();
    assert!(error.to_string().contains("not admitted to WRITE"));
    assert!(bridge.take_prompt().is_none());
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn local_entry_keeps_the_original_workspace_if_configuration_changes_during_input() {
    use crate::secret_entry::EntryValue;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let a = f.workspace("Same label");
    let b = f.workspace("Same label");
    let bridge = SecretEntry::default();
    f.dispatch.secret_entry = Some(bridge.clone());
    let node = f.node.clone();
    let original = a.clone();
    let other = b.clone();
    let worker = std::thread::spawn(move || {
        let prompt = take_local_prompt(&bridge);
        assert_eq!(prompt.request.workspace, format!("{:X}", original.id));
        assert_eq!(prompt.request.workspace_label, original.label);
        assert_eq!(prompt.request.service, "Synthetic service");
        assert_eq!(prompt.request.name, "fixture login");
        let mut node = node.lock().unwrap();
        let node = node.as_mut().unwrap();
        let view = node.config().unwrap();
        let mut changed = original;
        changed.routes = other.routes;
        node.put_workspace(&view, &changed).unwrap();
        drop(view);
        prompt.answer(EntryAnswer::Submit(EntryValue::login(
            Zeroizing::new("invented@example.invalid".into()),
            Zeroizing::new("invented-password".into()),
        )));
    });
    let response = f.call(ENTER_LOCAL.name, local_entry_args(&a)).unwrap();
    worker.join().unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response.as_object().unwrap().len(), 2);
    assert_eq!(response["status"], "saved");
    let secret = Id::from_hex(response["secret"].as_str().unwrap()).unwrap();
    let storage = f.dispatch.owners.borrow()[&a.id].storage.clone();
    let secrets = faculties::secrets::Secrets::with_storage(storage);
    let stored = secrets.get(secret).unwrap();
    assert_eq!(stored.as_slice(), br#"{"username":"invented@example.invalid","password":"invented-password"}"#);
    assert_eq!(secrets.list().unwrap().len(), 1);
    assert!(!f.call("secrets_list", selection(&b)).unwrap().contains(&format!("{secret:x}")));
}

#[test]
fn local_entry_closed_bridge_is_unavailable_and_generic_success_returns_id_only() {
    use crate::secret_entry::EntryValue;
    use zeroize::Zeroizing;
    let mut f = Fixture::new();
    let workspace = f.workspace("Generic fixture");
    let bridge = SecretEntry::default();
    f.dispatch.secret_entry = Some(bridge.clone());
    let worker_bridge = bridge.clone();
    let worker = std::thread::spawn(move || {
        let prompt = take_local_prompt(&worker_bridge);
        assert_eq!(prompt.request.kind, EntryKind::Secret);
        prompt.answer(EntryAnswer::Submit(EntryValue::secret(Zeroizing::new("invented generic value".into()))));
    });
    let mut args = local_entry_args(&workspace);
    args["kind"] = json!("secret");
    let response: Value = serde_json::from_str(&f.call(ENTER_LOCAL.name, args).unwrap()).unwrap();
    worker.join().unwrap();
    assert_eq!(response.as_object().unwrap().len(), 2);
    assert_eq!(response["status"], "saved");
    let id = Id::from_hex(response["secret"].as_str().unwrap()).unwrap();
    let storage = f.dispatch.owners.borrow()[&workspace.id].storage.clone();
    assert_eq!(faculties::secrets::Secrets::with_storage(storage).get(id).unwrap().as_slice(), b"invented generic value");
    bridge.close();
    let response = f.call(ENTER_LOCAL.name, local_entry_args(&workspace)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&response).unwrap(), json!({"status":"unavailable"}));
    assert!(bridge.take_prompt().is_none());
}

#[test]
fn schema_registration_does_not_open_a_pile_key_or_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let pile = dir.path().join("absent.pile");
    let key = dir.path().join("absent.key");
    let dispatch = WorkspaceDispatch::new(
        NodeBinding {
            pile: pile.clone(),
            key: key.clone(),
            signer: [0; 32],
        },
        || panic!("schema registration must not request configuration"),
    )
    .unwrap();
    assert!(dispatch.tools().len() > 100);
    assert!(dispatch.owners.borrow().is_empty());
    dispatch.close().unwrap();
    assert!(!pile.exists());
    assert!(!key.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn discord_auth_uses_only_the_explicit_workspace_secret_route() {
    let f = Fixture::new();
    let mut workspace = f.workspace("Bot fixture");
    let storage = Storage::shared_local(
        f.dispatch.node.pile.clone(),
        Some(f.dispatch.node.key.clone()),
    )
    .with_collections(workspace.routes.clone());
    let secret = faculties::secrets::Secrets::with_storage(storage.clone())
        .add("fixture bot", b"synthetic-workspace-bot-token")
        .unwrap();
    storage.close().unwrap();

    let mut args = selection(&workspace);
    args["secret_version"] = json!(format!("{secret:X}"));
    let bound = f.call("discord_auth_set", args.clone()).unwrap();
    assert!(bound.contains(&format!("{secret:X}")));
    assert!(!bound.contains("synthetic-workspace-bot-token"));
    let status = f
        .call("discord_auth_status", selection(&workspace))
        .unwrap();
    assert!(status.contains("settled"));
    assert!(status.contains(&format!("{secret:X}")));
    assert!(!status.contains("synthetic-workspace-bot-token"));

    // Omission is unavailable, not a fallback to a signer-private Secrets
    // collection containing the same version elsewhere in the physical pile.
    workspace
        .routes
        .remove(&faculties::secrets::DEFAULT_SCOPE_ID);
    f.configure(&mut workspace);
    args["workspace"] = json!(format!("{:X}", workspace.id));
    let error = f.call("discord_auth_set", args).unwrap_err();
    assert!(!format!("{error:#}").contains("synthetic-workspace-bot-token"));
    assert!(
        format!("{error:#}").contains("is unconfigured"),
        "{error:#}"
    );
    // Metadata-only status remains available without opening Secrets.
    assert!(
        f.call("discord_auth_status", selection(&workspace))
            .unwrap()
            .contains("settled")
    );
}

fn session(dispatch: &WorkspaceDispatch) -> mcp::Server<'_> {
    let mut server = mcp::Server::new(&[dispatch]).unwrap();
    let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":mcp::PROTOCOL_VERSION,"capabilities":{},
        "clientInfo":{"name":"workspace test","version":"1"},
    }});
    server
        .dispatch(Bytes::from(serde_json::to_vec(&initialize).unwrap()))
        .unwrap();
    server
        .dispatch(Bytes::from(
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_vec(),
        ))
        .unwrap();
    server
}

fn rpc_call(server: &mut mcp::Server<'_>, name: &str, args: Value) -> Value {
    let request = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":name,"arguments":args,
    }});
    serde_json::from_str(
        &server
            .dispatch(Bytes::from(serde_json::to_vec(&request).unwrap()))
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn every_schema_requires_workspace_except_passive_list_and_protocol_has_no_ambient_reads() {
    let f = Fixture::new();
    assert!(f.dispatch.tools().len() > 100);
    for tool in f.dispatch.tools() {
        let schema: Value = serde_json::from_str(f.dispatch.input_schema(tool)).unwrap();
        if tool.name == "list_workspaces" {
            assert!(schema["properties"].as_object().unwrap().is_empty());
        } else {
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("workspace")),
                "{}",
                tool.name
            );
            assert_eq!(schema["properties"]["workspace"]["type"], "string");
            assert!(
                !tool.input_schema.contains("\"workspace\""),
                "static adapter schema changed"
            );
        }
    }
    let mut server = mcp::Server::new(&[&f.dispatch]).unwrap();
    for method in [
        "resources/read",
        "resources/list",
        "prompts/get",
        "prompts/list",
    ] {
        let request = json!({"jsonrpc":"2.0","id":1,"method":method,"params":{}});
        let response: Value = serde_json::from_str(
            &server
                .dispatch(Bytes::from(serde_json::to_vec(&request).unwrap()))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }
}

#[test]
fn list_is_config_only_and_mutating_faculty_never_runs_without_an_accepted_workspace() {
    let f = Fixture::new();
    let mut configured = Workspace::new("Readable label");
    f.configure(&mut configured);
    let listed: Value =
        serde_json::from_str(&f.call("list_workspaces", json!({})).unwrap()).unwrap();
    assert_eq!(
        listed["workspaces"][0]["workspace"],
        format!("{:X}", configured.id)
    );
    assert_eq!(listed["workspaces"][0]["label"], "Readable label");
    assert!(listed["workspaces"][0].get("key").is_none());
    assert!(f.dispatch.owners.borrow().is_empty());
    let before = fs::read(f.dir.path().join("node.pile")).unwrap();
    for args in [
        json!({"title":"must not exist"}),
        json!({"workspace": format!("{:X}", genid().id), "title":"must not exist"}),
        json!({"workspace":"Readable label", "title":"must not exist"}),
        json!({"workspace":123, "title":"must not exist"}),
        json!({"workspace":null, "title":"must not exist"}),
        json!({"workspace":format!("{:X}", configured.id), "title":"must not exist"}),
    ] {
        assert!(f.call("compass_add", args).is_err());
    }
    assert_eq!(fs::read(f.dir.path().join("node.pile")).unwrap(), before);
    // A failed role lookup may retain its local-only adapter, never an endpoint.
}

#[test]
fn interleaved_calls_in_two_protocol_sessions_do_not_set_a_current_workspace() {
    let f = Fixture::new();
    let a = f.workspace("A");
    let b = f.workspace("B");
    let mut first = session(&f.dispatch);
    let mut second = session(&f.dispatch);
    for (server, workspace, title) in [
        (&mut first, &a, "Only A first"),
        (&mut second, &b, "Only B"),
    ] {
        let response = rpc_call(
            server,
            "compass_add",
            json!({"workspace":format!("{:X}",workspace.id),"title":title}),
        );
        assert!(response.get("error").is_none(), "{response}");
        assert_ne!(response["result"]["isError"], true, "{response}");
    }
    // Deliberately use B's previous session for A, and A's for B.
    let response = rpc_call(
        &mut second,
        "compass_add",
        json!({"workspace":format!("{:X}",a.id),"title":"Only A second"}),
    );
    assert_ne!(response["result"]["isError"], true, "{response}");
    let response = rpc_call(&mut first, "compass_list", selection(&b));
    assert!(response.to_string().contains("Only B"));
    assert!(!response.to_string().contains("Only A"));
    assert_eq!(
        rpc_call(&mut first, "compass_list", json!({}))["error"]["code"],
        -32602
    );
    let a_result = f.call("compass_list", selection(&a)).unwrap();
    let b_result = f.call("compass_list", selection(&b)).unwrap();
    assert!(a_result.contains("Only A first") && a_result.contains("Only A second"));
    assert!(!a_result.contains("Only B"));
    assert!(b_result.contains("Only B") && !b_result.contains("Only A"));
    assert!(f.call("compass_list", json!({})).is_err());
    assert_eq!(f.dispatch.owners.borrow().len(), 2);
    f.dispatch.close().unwrap();
    assert!(f.dispatch.owners.borrow().is_empty());
    // Closing flushes successful writes. Independent request owners can reopen.
    assert!(
        f.call("compass_list", selection(&a))
            .unwrap()
            .contains("Only A first")
    );
}

#[test]
fn same_name_roots_are_exact_and_old_binding_never_retargets() {
    let f = Fixture::new();
    let a = f.workspace("one pile");
    let signer = signing_key_file::load_existing(&f.dispatch.node.key).unwrap();
    let mut pile =
        faculties::storage::open_pile_strict_as(&f.dispatch.node.pile, signer.verifying_key())
            .unwrap();
    let alternate = pile
        .collection(
            "compass",
            CollectionPolicy::new(
                AdmissionPolicy::Open,
                AdmissionPolicy::direct(signer.verifying_key()),
            ),
        )
        .unwrap()
        .handle();
    pile.close().unwrap();
    let mut b = a.clone();
    b.anchor = genid().id;
    b.label = "one pile".into(); // Identical labels never identify a workspace.
    b.routes
        .insert(faculties::schemas::compass::DEFAULT_SCOPE_ID, alternate);
    let mut guard = f.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let view = node.config().unwrap();
    b.id = node.put_workspace(&view, &b).unwrap();
    drop(guard);
    f.call(
        "compass_add",
        json!({"workspace":format!("{:X}", a.id), "title":"First exact root"}),
    )
    .unwrap();
    f.call(
        "compass_add",
        json!({"workspace":format!("{:X}", b.id), "title":"Second exact root"}),
    )
    .unwrap();
    assert!(
        !f.call("compass_list", selection(&a))
            .unwrap()
            .contains("Second exact root")
    );
    assert!(
        !f.call("compass_list", selection(&b))
            .unwrap()
            .contains("First exact root")
    );

    let mut replacement = a.clone();
    replacement.routes = b.routes.clone();
    let mut guard = f.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let view = node.config().unwrap();
    replacement.id = node.put_workspace(&view, &replacement).unwrap();
    drop(guard);
    assert_ne!(replacement.id, a.id);
    assert!(
        f.call(
            "compass_add",
            json!({"workspace":format!("{:X}", a.id),"title":"Must not retarget"})
        )
        .is_err()
    );
    assert!(!f.dispatch.owners.borrow().contains_key(&a.id));
    let after = f.call("compass_list", selection(&replacement)).unwrap();
    assert!(after.contains("Second exact root") && !after.contains("Must not retarget"));
}

#[test]
fn request_cannot_inject_routing_and_duplicate_arguments_are_not_normalized() {
    let f = Fixture::new();
    let a = f.workspace("selected");
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    for field in ["pile", "key", "routes", "root", "collections"] {
        let mut args = selection(&a);
        args["title"] = json!("Never written");
        args[field] = json!("/untrusted/target");
        assert!(f.call("compass_add", args).is_err(), "{field}");
    }
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
    let id = format!("{:X}", a.id);
    for raw in [
        format!(r#"{{"workspace":"{id}","workspace":"{id}","title":"no"}}"#),
        format!(r#"{{"workspace":"{id}","title":"first","title":"second"}}"#),
    ] {
        assert!(f.raw("compass_add", &raw).is_err());
    }
    let nested =
        format!(r#"{{"workspace":"{id}","label":"no","profile":{{"note":"one","note":"two"}}}}"#);
    assert!(f.raw("relations_add", &nested).is_err());
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn missing_role_and_replaced_signer_fail_without_node_or_private_root_fallback() {
    let f = Fixture::new();
    let mut a = f.workspace("no role");
    a.routes
        .remove(&faculties::schemas::compass::DEFAULT_SCOPE_ID);
    let mut guard = f.node.lock().unwrap();
    let node = guard.as_mut().unwrap();
    let view = node.config().unwrap();
    a.id = node.put_workspace(&view, &a).unwrap();
    drop(guard);
    let error = f.call("compass_list", selection(&a)).unwrap_err();
    assert!(format!("{error:#}").contains("role"), "{error:#}");
    let before = fs::read(&f.dispatch.node.pile).unwrap();
    // Replace only the scratch key at the same selected path.
    fs::rename(
        &f.dispatch.node.key,
        f.dispatch.node.key.with_extension("old-key"),
    )
    .unwrap();
    signing_key_file::init(&f.dispatch.node.key).unwrap();
    let error = f
        .call(
            "compass_add",
            json!({"workspace":format!("{:X}",a.id),"title":"no"}),
        )
        .unwrap_err();
    assert!(format!("{error:#}").contains("signing identity changed"));
    assert_eq!(fs::read(&f.dispatch.node.pile).unwrap(), before);
}

#[test]
fn schema_reserves_workspace_instead_of_silently_overwriting_faculty_arguments() {
    for schema in [
        r#"{"type":"array"}"#,
        r#"{"type":"object","properties":{"workspace":{"type":"integer"}}}"#,
        r#"{"type":"object","properties":{},"required":["workspace"]}"#,
        r#"{"type":"object","properties":{},"required":"name"}"#,
    ] {
        assert!(wrapped_schema(schema).is_err());
    }
    let schema: Value = serde_json::from_str(&wrapped_schema(
        r#"{"type":"object","properties":{"title":{"type":"string"}},"required":["title"],"additionalProperties":false}"#,
    ).unwrap()).unwrap();
    assert_eq!(schema["required"], json!(["title", "workspace"]));
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        register_tool(
            &mut Vec::new(),
            &mut BTreeMap::new(),
            &LIST,
            LIST.input_schema
        )
        .is_err()
    );
    let tool = Tool {
        name: "native_tool",
        description: "test",
        input_schema: LIST.input_schema,
    };
    let mut tools = Vec::new();
    let mut schemas = BTreeMap::new();
    register_tool(&mut tools, &mut schemas, &tool, tool.input_schema).unwrap();
    assert!(register_tool(&mut tools, &mut schemas, &tool, tool.input_schema).is_err());
}

#[test]
fn same_id_annotation_replaces_warm_owner_and_matches_a_fresh_request() {
    use crate::node_schema as attrs;
    use faculties::schemas::{compass, config::config};
    let f = Fixture::new();
    let mut workspace = f.workspace("annotated");
    let root = workspace.routes.remove(&compass::DEFAULT_SCOPE_ID).unwrap();
    {
        let mut guard = f.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        workspace.id = node.put_workspace(&view, &workspace).unwrap();
    }
    f.call(
        "relations_add",
        json!({
            "workspace":format!("{:X}", workspace.id),"label":"Retained before annotation",
        }),
    )
    .unwrap();
    assert!(f.call("compass_list", selection(&workspace)).is_err());
    let before = f.node.lock().unwrap().as_mut().unwrap().config().unwrap();
    let revision = workspace.id;
    f.annotate(
        entity! { ExclusiveId::force_ref(&revision) @ attrs::workspace_route*: entity! {
            config::anchor: compass::DEFAULT_SCOPE_ID, config::selects: root,
        } },
    );
    // An in-flight observation keeps the original binding; the later request
    // resolves the additive native facts without treating the ID as a hash.
    assert!(
        !before
            .workspace(revision)
            .unwrap()
            .unwrap()
            .routes
            .contains_key(&compass::DEFAULT_SCOPE_ID)
    );
    f.call(
        "compass_add",
        json!({
            "workspace":format!("{revision:X}"),"title":"Newly configured role",
        }),
    )
    .unwrap();
    assert_eq!(
        f.dispatch.owners.borrow()[&revision]
            .binding
            .routes
            .get(&compass::DEFAULT_SCOPE_ID),
        Some(&root)
    );
    assert!(
        f.call("relations_list", selection(&workspace))
            .unwrap()
            .contains("Retained before annotation")
    );
    let warm = f.call("compass_list", selection(&workspace)).unwrap();
    let node = f.node.clone();
    let fresh = WorkspaceDispatch::new(f.dispatch.node.clone(), move || {
        node.lock().unwrap().as_mut().unwrap().config()
    })
    .unwrap();
    let mut text = String::new();
    fresh
        .call(
            "compass_list",
            Bytes::from(serde_json::to_vec(&selection(&workspace)).unwrap()),
            &mut Out::new(&mut |part| {
                if let Part::Text { text: value } = part {
                    text.push_str(&value);
                }
                Ok(())
            }),
        )
        .unwrap();
    assert_eq!(warm, text);
    fresh.close().unwrap();
}

#[test]
fn unavailable_revision_does_not_hide_healthy_workspaces_and_can_be_withdrawn() {
    use crate::node_schema as attrs;
    use faculties::schemas::{compass, config::config};
    let f = Fixture::new();
    let healthy = f.workspace("healthy");
    let bad = f.workspace("conflicted");
    f.call("compass_list", selection(&bad)).unwrap();
    let revision = bad.id;
    f.annotate(
        entity! { ExclusiveId::force_ref(&revision) @ attrs::workspace_route*: entity! {
            config::anchor: compass::DEFAULT_SCOPE_ID,
            config::selects: triblespace::core::collection::CollectionHandle::new([31; 32]),
        } },
    );
    let listed: Value =
        serde_json::from_str(&f.call("list_workspaces", json!({})).unwrap()).unwrap();
    assert_eq!(listed["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed["workspaces"][0]["workspace"],
        format!("{:X}", healthy.id)
    );
    assert_eq!(listed["node_signer"], hex::encode(f.dispatch.node.signer));
    assert_eq!(
        listed["unavailable"][0]["workspace"],
        format!("{revision:X}")
    );
    assert!(
        listed["unavailable"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("conflicting exact roots")
    );
    assert!(!f.dispatch.owners.borrow().contains_key(&revision));
    {
        let mut guard = f.node.lock().unwrap();
        let node = guard.as_mut().unwrap();
        let view = node.config().unwrap();
        assert!(view.workspace_revision(revision).is_err());
        node.remove_workspace_revision(&view, revision).unwrap();
    }
    assert!(f.call("compass_list", selection(&bad)).is_err());
    let listed: Value =
        serde_json::from_str(&f.call("list_workspaces", json!({})).unwrap()).unwrap();
    assert_eq!(listed["unavailable"], json!([]));
    assert_eq!(listed["workspaces"].as_array().unwrap().len(), 1);
}
