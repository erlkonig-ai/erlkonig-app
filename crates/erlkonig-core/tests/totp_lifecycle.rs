//! Synthetic native transport/consumer acceptance. No browser, real credential,
//! live node/configuration or desktop UI participates.
use erlkonig_core::{
    config::Bootstrap,
    credentials, init,
    node_store::{NodeSettings, NodeStore},
    paths::Layout,
    runtime::Runtime,
};
use faculties::secrets::{self, item::PrivateText, schema::*};
use faculties::storage::Storage;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command, time::Duration};
use triblespace::core::repo::SnapshotSource;
use triblespace::core::{
    collection::{AdmissionPolicy, CollectionPolicy, CollectionStoreExt},
    signing_key_file,
};
use triblespace::prelude::*;

const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"; // RFC 6238 public test key.

fn input() -> [secrets::item::FieldInput<'static>; 3] {
    use secrets::item::{FieldInput, FieldValue};
    [
        FieldInput {
            role: ROLE_USERNAME,
            label: None,
            value: FieldValue::Text("fixture-user"),
        },
        FieldInput {
            role: ROLE_PASSWORD,
            label: None,
            value: FieldValue::Text("SYNTHETIC-PRIVATE-password"),
        },
        FieldInput {
            role: ROLE_CUSTOM,
            label: Some("SYNTHETIC-PRIVATE-label"),
            value: FieldValue::Text(
                "otpauth://totp/GitHub:fixture-user?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=GitHub",
            ),
        },
    ]
}

struct Fixture {
    _directory: tempfile::TempDir,
    layout: Layout,
    selected: Value,
    omitted: Id,
    denied: Value,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout::new(directory.path().join("data"));
        fs::create_dir(&layout.dir).unwrap();
        let signer = init::prepare(&Bootstrap::new(&layout.dir)).unwrap();
        let mut node = NodeStore::open(&layout.default_pile(), signer.clone()).unwrap();
        let view = node.config().unwrap();
        node.put_settings(
            &view,
            &NodeSettings {
                mcp_port: 0,
                sync_enabled: false,
                maintenance_enabled: false,
            },
        )
        .unwrap();
        let view = node.config().unwrap();
        let id = node
            .create_workspace(&view, "Synthetic TOTP reader")
            .unwrap();
        let workspace = node.config().unwrap().workspace(id).unwrap().unwrap();
        let view = node.config().unwrap();
        let omitted_id = node
            .create_workspace(&view, "Synthetic no Secrets route")
            .unwrap();
        let mut omitted = node
            .config()
            .unwrap()
            .workspace(omitted_id)
            .unwrap()
            .unwrap();
        omitted.routes.remove(&secrets::DEFAULT_SCOPE_ID);
        let view = node.config().unwrap();
        let omitted = node.put_workspace(&view, &omitted).unwrap();
        node.close().unwrap();

        let storage = Storage::shared_local(layout.default_pile(), Some(layout.default_key()))
            .with_collections(workspace.routes.clone());
        let secrets = secrets::Secrets::with_storage(storage.clone());
        let secret = secrets
            .add_item(
                "public synthetic TOTP",
                secrets::item::ItemInput {
                    fields: &input(),
                    attachments: &[],
                },
            )
            .unwrap();
        let bodies = secrets.open_item(secret).unwrap();
        assert_eq!(bodies.len(), 1);
        let body = bodies[0];
        let facts = secrets.open_item_body(secret, body).unwrap();
        let children: Vec<_> = find!(child: PrivateText, pattern!(&facts, [
            { field_role: ROLE_CUSTOM, field_text: ?child },
        ]))
        .collect();
        assert_eq!(children.len(), 1);
        let selected = json!({"workspace":format!("{id:X}"), "secret":format!("{secret:x}"),
            "body":hex::encode(body.raw), "child":hex::encode(children[0].raw)});
        storage.close().unwrap();

        // This is a resident, signer-decryptable version, not an absent-handle
        // lookup. Its source grants WRITE to this node but READ only to another
        // synthetic identity. Native routing must still refuse READ.
        let other = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let mut pile =
            faculties::storage::open_pile_strict_as(&layout.default_pile(), signer.verifying_key())
                .unwrap();
        let restricted = secrets::storage::SecretsCollection::register(
            &mut pile,
            "Synthetic denied TOTP",
            CollectionPolicy::new(
                AdmissionPolicy::direct(other.verifying_key()),
                AdmissionPolicy::direct(signer.verifying_key()),
            ),
        )
        .unwrap();
        let epoch = hifitime::Epoch::from_unix_seconds(0.0);
        let sealed = secrets::item::prepare(
            restricted.handle(),
            &signer,
            "public denied TOTP",
            secrets::item::ItemInput {
                fields: &input(),
                attachments: &[],
            },
            (epoch, epoch).try_to_inline().unwrap(),
        )
        .unwrap();
        let denied_secret = sealed.secret;
        let public = sealed.fragment.facts().clone();
        let bodies: Vec<_> = find!(body: secrets::item::ItemBody, pattern!(&public, [{denied_secret @ structured_body: ?body}])).collect();
        assert_eq!(bodies.len(), 1);
        let denied_body = bodies[0];
        pile.commit(restricted.source(), &signer, sealed.fragment)
            .unwrap();
        let opened = secrets::item::open_exact(
            pile.snapshot().unwrap(),
            &public,
            denied_secret,
            denied_body,
            &signer,
        )
        .unwrap();
        let children: Vec<_> = find!(child: PrivateText, pattern!(opened.facts(), [{field_role: ROLE_CUSTOM, field_text: ?child}])).collect();
        assert_eq!(children.len(), 1);
        assert_eq!(
            opened.totp_at(children[0], 59).unwrap().code.as_str(),
            "287082"
        );
        let denied_child = children[0];
        drop(opened);
        pile.close().unwrap();

        let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
        let view = node.config().unwrap();
        let denied_id = node
            .create_workspace(&view, "Synthetic denied READ route")
            .unwrap();
        let mut denied = node
            .config()
            .unwrap()
            .workspace(denied_id)
            .unwrap()
            .unwrap();
        denied
            .routes
            .insert(secrets::DEFAULT_SCOPE_ID, restricted.handle());
        let view = node.config().unwrap();
        let denied_id = node.put_workspace(&view, &denied).unwrap();
        node.close().unwrap();
        Self {
            _directory: directory,
            layout,
            selected,
            omitted,
            denied: json!({"workspace":format!("{denied_id:X}"), "secret":format!("{denied_secret:x}"),
                "body":hex::encode(denied_body.raw), "child":hex::encode(denied_child.raw)}),
        }
    }
}

struct Native {
    runtime: Runtime,
    client: Client,
    url: String,
    token: String,
    session: String,
}

impl Native {
    fn start(fixture: &Fixture) -> Self {
        let runtime = Runtime::start(&fixture.layout).unwrap();
        let status = runtime.status();
        assert_eq!(status.sync_collections, 0);
        let url = status.mcp_url.unwrap();
        let token = credentials::load(&fixture.layout.dir.join("mcp.token")).unwrap();
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();
        let initialize = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"synthetic-totp-acceptance","version":"1"}}});
        assert_eq!(
            client
                .post(&url)
                .header("Accept", "application/json, text/event-stream")
                .json(&initialize)
                .send()
                .unwrap()
                .status(),
            401
        );
        let response = client
            .post(&url)
            .bearer_auth(&token)
            .header("Accept", "application/json, text/event-stream")
            .json(&initialize)
            .send()
            .unwrap();
        assert!(response.status().is_success());
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let mut native = Self {
            runtime,
            client,
            url,
            token,
            session,
        };
        native.post(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        native
    }
    fn post(&mut self, value: Value) -> Option<Value> {
        let response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json, text/event-stream")
            .header("mcp-session-id", &self.session)
            .header("mcp-protocol-version", "2025-06-18")
            .json(&value)
            .send()
            .unwrap();
        assert!(response.status().is_success());
        let bytes = response.bytes().unwrap();
        (!bytes.is_empty()).then(|| serde_json::from_slice(&bytes).unwrap())
    }
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.post(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}})).unwrap()
    }
}

fn code(response: &Value) -> Value {
    assert!(response.get("error").is_none());
    assert_ne!(response["result"]["isError"], true);
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains(SEED) && !text.contains("otpauth:") && !text.contains("SYNTHETIC-PRIVATE")
    );
    let value: Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["issuer"], "GitHub");
    assert_eq!(value["account"], "fixture-user");
    assert_eq!(value["period"], 30);
    assert_eq!(value["digits"], 6);
    assert_eq!(value["algorithm"], "SHA1");
    let now = value["generated_at_unix"].as_u64().unwrap();
    assert_eq!(value["valid_from_unix"], now / 30 * 30);
    assert_eq!(value["valid_until_unix"], now / 30 * 30 + 30);
    assert_eq!(value["code"].as_str().unwrap().len(), 6);
    value
}

fn refused(response: Value, reason: Option<&str>) {
    assert!(response.get("error").is_some() || response["result"]["isError"] == true);
    let encoded = response.to_string();
    assert!(
        !encoded.contains(SEED)
            && !encoded.contains("otpauth:")
            && !encoded.contains("SYNTHETIC-PRIVATE")
    );
    if let Some(reason) = reason {
        assert!(encoded.contains(reason), "{encoded}");
    }
}

#[test]
fn authenticated_totp_dispatch_preserves_exact_workspace_read_and_signer_boundaries() {
    let fixture = Fixture::new();
    let mut native = Native::start(&fixture);
    let listed = native
        .post(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .unwrap();
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "secrets_item_totp")
        .unwrap();
    assert!(
        tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("workspace"))
    );
    code(&native.call("secrets_item_totp", fixture.selected.clone()));
    let mut no_workspace = fixture.selected.clone();
    no_workspace.as_object_mut().unwrap().remove("workspace");
    refused(native.call("secrets_item_totp", no_workspace), None);
    let mut omitted = fixture.selected.clone();
    omitted["workspace"] = json!(format!("{:X}", fixture.omitted));
    refused(
        native.call("secrets_item_totp", omitted),
        Some("unconfigured"),
    );
    refused(
        native.call("secrets_item_totp", fixture.denied.clone()),
        Some("READ"),
    );
    for name in ["body", "child"] {
        let mut wrong = fixture.selected.clone();
        wrong[name] = json!("00".repeat(32));
        refused(native.call("secrets_item_totp", wrong), None);
    }
    // Replace only this fresh fixture key after the runtime froze its signer.
    let saved = fixture.layout.default_key().with_extension("original");
    fs::rename(fixture.layout.default_key(), &saved).unwrap();
    signing_key_file::init(&fixture.layout.default_key()).unwrap();
    refused(
        native.call("secrets_item_totp", fixture.selected.clone()),
        Some("signing identity changed"),
    );
    fs::remove_file(fixture.layout.default_key()).unwrap();
    fs::rename(saved, fixture.layout.default_key()).unwrap();
    native.runtime.finish().unwrap();
}

#[test]
#[ignore = "manual: ERLKONIG_TOTP_APP_BIN and ERLKONIG_TOTP_ERP_ROOT must name this frozen closure"]
fn private_erp_consumer_uses_actual_candidate_forwarder_and_native_totp() {
    let binary = PathBuf::from(
        std::env::var_os("ERLKONIG_TOTP_APP_BIN").expect("candidate binary is required"),
    );
    let erp = PathBuf::from(
        std::env::var_os("ERLKONIG_TOTP_ERP_ROOT").expect("frozen private ERP root is required"),
    );
    let node = std::env::var_os("ERLKONIG_TOTP_NODE").expect("explicit Node binary is required");
    let fixture = Fixture::new();
    let mut native = Native::start(&fixture);
    let expected = code(&native.call("secrets_item_totp", fixture.selected.clone()));
    let config = json!({"appBinary":binary, "dataDir":fixture.layout.dir, "url":native.url,
        "workspace":fixture.selected["workspace"], "secret":fixture.selected["secret"], "body":fixture.selected["body"]});
    let module = erp.join("collectors/figma/src/secrets.ts");
    let script = format!(
        "import {{getGithubTotpCode}} from {}; const value=await getGithubTotpCode({}, {}, 'Fixture-User'); if(value.issuer!=='GitHub'||value.account!=='fixture-user'||value.algorithm!=='SHA1'||value.digits!==6||value.period!==30||!/^\\d{{6}}$/.test(value.code)) throw Error('synthetic native response mismatch'); value.code=''; process.stdout.write('synthetic_native_consumer_passed\\n');",
        serde_json::to_string(&format!("file://{}", module.display())).unwrap(),
        config,
        fixture.selected["child"]
    );
    let output = Command::new(node)
        .args(["--input-type=module", "-e", &script])
        .env_clear()
        .env("HOME", fixture._directory.path())
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "consumer status={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"synthetic_native_consumer_passed\n");
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(expected["code"].as_str().unwrap()));
    native.runtime.finish().unwrap();
}
