use erlkonig_core::{
    config::Bootstrap,
    credentials, init,
    node_store::{NodeSettings, NodeStore},
    paths::Layout,
    runtime::Runtime,
};
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn fixture() -> (tempfile::TempDir, Layout) {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().join("data"));
    fs::create_dir(&layout.dir).unwrap();
    let signer = init::prepare(&Bootstrap::new(&layout.dir)).unwrap();
    let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
    let view = node.config().unwrap();
    node.put_settings(
        &view,
        &NodeSettings {
            mcp_port: 0,
            sync_enabled: false,
            maintenance_enabled: false,
            peers: Vec::new(),
        },
    )
    .unwrap();
    node.close().unwrap();
    (dir, layout)
}

fn initialize() -> Value {
    json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
        "protocolVersion":"2025-06-18", "capabilities":{}, "clientInfo":{"name":"erlkonig-native-test", "version":"1"}
    }})
}

#[test]
fn authenticated_native_mcp_quit_and_restart() {
    let (_dir, layout) = fixture();
    let mut app = Runtime::start(&layout).unwrap();
    let first = app.status();
    let url = first.mcp_url.unwrap();
    assert_eq!(first.sync_collections, 0);
    assert!(
        Runtime::start(&layout).is_err(),
        "one owner per data directory"
    );
    let token = credentials::load(&layout.dir.join("mcp.token")).unwrap();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        "application/json, text/event-stream".parse().unwrap(),
    );
    let client = reqwest::blocking::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    assert_eq!(
        client
            .post(&url)
            .json(&initialize())
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("wrong")
            .json(&initialize())
            .send()
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(&token)
            .header("Origin", "https://untrusted.invalid")
            .json(&initialize())
            .send()
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .json(&initialize())
        .send()
        .unwrap();
    assert!(response.status().is_success(), "HTTP {}", response.status());
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let initialized: Value = response.json().unwrap();
    assert!(initialized["result"]["serverInfo"].is_object());
    let notification = client
        .post(&url)
        .bearer_auth(&token)
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", "2025-06-18")
        .json(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
        .send()
        .unwrap();
    assert!(notification.status().is_success());
    let request = |method: &str, params: Value| {
        client
            .post(&url)
            .bearer_auth(&token)
            .header("mcp-session-id", &session)
            .header("mcp-protocol-version", "2025-06-18")
            .json(&json!({"jsonrpc":"2.0","id":2,"method":method,"params":params}))
            .send()
            .unwrap()
    };
    let tools: Value = request("tools/list", json!({})).json().unwrap();
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "compass_list")
    );
    let native: Value = request("tools/call", json!({"name":"compass_list","arguments":{}}))
        .json()
        .unwrap();
    assert_eq!(
        native["error"]["code"], -32602,
        "no implicit node workspace: {native}"
    );
    let listed: Value = request(
        "tools/call",
        json!({"name":"list_workspaces","arguments":{}}),
    )
    .json()
    .unwrap();
    assert!(listed.get("error").is_none(), "{listed}");
    assert_ne!(listed["result"]["isError"], true, "{listed}");
    let workspaces: Value =
        serde_json::from_str(listed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(workspaces["workspaces"], json!([]));
    let before = Instant::now();
    app.begin_shutdown();
    while !app.is_finished() && before.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(app.is_finished(), "idle native owners must stop promptly");
    app.finish().unwrap();
    assert!(
        client
            .post(&url)
            .bearer_auth(&token)
            .json(&initialize())
            .send()
            .is_err()
    );
    let addr = url.strip_prefix("http://").unwrap().trim_end_matches('/');
    drop(TcpListener::bind(addr).unwrap());
    let mut restarted = Runtime::start(&layout).unwrap();
    assert_eq!(restarted.status().node_id, first.node_id);
    restarted.finish().unwrap();
}

#[test]
fn quit_stops_accepting_and_bounds_a_stalled_http_body() {
    use std::io::Write;
    let (_dir, layout) = fixture();
    let mut app = Runtime::start(&layout).unwrap();
    let url = app.status().mcp_url.unwrap();
    let addr = url.strip_prefix("http://").unwrap().trim_end_matches('/');
    let token = credentials::load(&layout.dir.join("mcp.token")).unwrap();
    let mut connection = std::net::TcpStream::connect(addr).unwrap();
    write!(connection, "POST / HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{{").unwrap();
    thread::sleep(Duration::from_millis(50));
    let start = Instant::now();
    app.begin_shutdown();
    thread::sleep(Duration::from_millis(50));
    assert!(
        std::net::TcpStream::connect(addr).is_err(),
        "shutdown must stop accepting new connections"
    );
    while !app.is_finished() && start.elapsed() < Duration::from_secs(7) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        app.is_finished(),
        "an incomplete HTTP body must not hold Quit indefinitely"
    );
    app.finish().unwrap();
    drop(connection);
    drop(TcpListener::bind(addr).unwrap());
}

fn wait_for(app: &Runtime, predicate: impl Fn(&erlkonig_core::status::Status) -> bool) {
    let began = Instant::now();
    loop {
        let status = app.status();
        if predicate(&status) {
            return;
        }
        assert!(
            began.elapsed() < Duration::from_secs(15),
            "timed out: {status:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn one_store_services_ignore_workspace_edits_allow_concurrent_writers_and_close_own_only() {
    use triblespace::core::{collection::CollectionStoreExt, metadata, signing_key_file};
    use triblespace::prelude::*;
    let (_dir, layout) = fixture();
    let mut app = Runtime::start(&layout).unwrap();
    let expected = app.config().unwrap();
    app.put_settings(
        &expected,
        &NodeSettings {
            mcp_port: 0,
            ..NodeSettings::default()
        },
    )
    .unwrap();
    wait_for(&app, |s| {
        s.node.sync_running && s.node.maintenance_running && s.sync_collections > 0
    });
    let before = app.status();
    assert_eq!(
        triblespace::core::collection::held::held_walker_threads(),
        1
    );
    assert!(!before.node.bound_sockets.is_empty());
    let view = app.config().unwrap();
    app.create_workspace(&view, "First bundle").unwrap();
    wait_for(&app, |s| s.sync_collections >= before.sync_collections + 30);
    assert_eq!(
        app.status().node.sessions_started,
        1,
        "bundle is not a subscription or service owner"
    );
    let signer = signing_key_file::load_existing(&layout.default_key()).unwrap();
    let mut external =
        faculties::storage::open_pile_strict_as(&layout.default_pile(), signer.verifying_key())
            .unwrap();
    let root = external
        .collection(
            "new arrival",
            faculties::collection_names::private_policy(signer.verifying_key()),
        )
        .unwrap();
    let prior = app.status().sync_collections;
    external
        .commit(
            root,
            &signer,
            entity! {metadata::name:"concurrent external writer"},
        )
        .unwrap();
    wait_for(&app, |s| s.sync_collections > prior);
    app.finish().unwrap();
    external
        .commit(
            root,
            &signer,
            entity! {metadata::name:"external writer survives app shutdown"},
        )
        .unwrap();
    assert_eq!(
        triblespace::core::collection::held::held_walker_threads(),
        0
    );
    external.close().unwrap();
    for socket in before.node.bound_sockets {
        drop(std::net::UdpSocket::bind(socket).unwrap());
    }
}

#[test]
fn pause_changes_do_not_change_identity_or_store_and_restart_preserves_settings() {
    let (_dir, layout) = fixture();
    let mut app = Runtime::start(&layout).unwrap();
    let identity = app.status().node_id;
    app.put_settings(
        &app.config().unwrap(),
        &NodeSettings {
            mcp_port: 0,
            sync_enabled: false,
            maintenance_enabled: true,
            peers: Vec::new(),
        },
    )
    .unwrap();
    wait_for(&app, |s| s.node.maintenance_running);
    assert!(!app.status().node.sync_running);
    app.put_settings(
        &app.config().unwrap(),
        &NodeSettings {
            mcp_port: 0,
            sync_enabled: false,
            maintenance_enabled: false,
            peers: Vec::new(),
        },
    )
    .unwrap();
    wait_for(&app, |s| {
        !s.node.maintenance_running && !s.node.maintenance_enabled
    });
    app.finish().unwrap();
    let mut app = Runtime::start(&layout).unwrap();
    assert_eq!(app.status().node_id, identity);
    assert!(
        !app.config()
            .unwrap()
            .settings()
            .unwrap()
            .maintenance_enabled
    );
    app.finish().unwrap();
}

#[test]
fn two_app_directories_cannot_start_two_endpoints_for_one_identity() {
    let (dir, layout) = fixture();
    let mut first = Runtime::start(&layout).unwrap();
    first
        .put_settings(
            &first.config().unwrap(),
            &NodeSettings {
                mcp_port: 0,
                sync_enabled: true,
                maintenance_enabled: false,
                peers: Vec::new(),
            },
        )
        .unwrap();
    wait_for(&first, |s| s.node.sync_running);
    let other = Layout::new(dir.path().join("another-app"))
        .with_node(layout.default_pile(), layout.default_key());
    let mut second = Runtime::start(&other).unwrap();
    wait_for(&second, |s| s.node.problem.is_some());
    assert!(!second.status().node.sync_running);
    assert_eq!(second.status().node.sessions_started, 0);
    second.finish().unwrap();
    assert!(
        first.status().node.sync_running,
        "failed contender never closes the first endpoint"
    );
    first.finish().unwrap();
}

#[test]
fn unreadable_config_stops_the_owned_session() {
    use std::io::Write;
    let (_dir, layout) = fixture();
    let mut app = Runtime::start(&layout).unwrap();
    app.put_settings(
        &app.config().unwrap(),
        &NodeSettings {
            mcp_port: 0,
            sync_enabled: false,
            maintenance_enabled: true,
            peers: Vec::new(),
        },
    )
    .unwrap();
    wait_for(&app, |s| s.node.maintenance_running);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(layout.default_pile())
        .unwrap();
    file.write_all(&[0xff; 256]).unwrap();
    file.sync_all().unwrap();
    wait_for(&app, |s| {
        s.config_problem.is_some() && !s.node.maintenance_running
    });
    let _ = app.finish();
}

// Opt-in isolated GUI preparation, never an application daemon mode.
#[test]
fn gui_fixture() {
    let Some(root) = std::env::var_os("ERLKONIG_GUI_FIXTURE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    assert!(!root.exists(), "refuse to reuse a GUI fixture");
    let home = root.join("home");
    let layout = Layout::new(erlkonig_core::paths::default_data_dir_for(&home));
    fs::create_dir_all(&layout.dir).unwrap();
    let signer = init::prepare(&layout.bootstrap()).unwrap();
    let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
    let view = node.config().unwrap();
    node.put_settings(
        &view,
        &NodeSettings {
            mcp_port: 0,
            sync_enabled: false,
            maintenance_enabled: false,
            peers: Vec::new(),
        },
    )
    .unwrap();
    let view = node.config().unwrap();
    node.create_workspace(&view, "Personal").unwrap();
    let view = node.config().unwrap();
    node.create_workspace(&view, "Research").unwrap();
    node.close().unwrap();
    let signer = triblespace::core::signing_key_file::load_existing(&layout.default_key()).unwrap();
    erlkonig_core::node_cli::bind(
        &layout,
        &erlkonig_core::config::NodeLocator {
            pile: layout.default_pile(),
            key: layout.default_key(),
            expected_signer: hex::encode(signer.verifying_key().to_bytes()),
        },
    )
    .unwrap();
    fs::write(
        root.join("fixture.json"),
        serde_json::to_vec_pretty(
            &json!({"home":home,"pile":layout.default_pile(),"key":layout.default_key()}),
        )
        .unwrap(),
    )
    .unwrap();
}

// This is a test-harness process, never a shipped daemon/headless app mode.
#[test]
fn force_quit_fixture() {
    let Some(dir) = std::env::var_os("ERLKONIG_FORCE_QUIT_FIXTURE") else {
        return;
    };
    let layout = Layout::new(dir);
    if !layout.default_pile().exists() {
        let signer = init::prepare(&Bootstrap::new(&layout.dir)).unwrap();
        let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
        let expected = node.config().unwrap();
        node.put_settings(
            &expected,
            &NodeSettings {
                mcp_port: 0,
                sync_enabled: false,
                maintenance_enabled: false,
                peers: Vec::new(),
            },
        )
        .unwrap();
        node.close().unwrap();
    }
    let _app = Runtime::start(&layout).unwrap();
    fs::write(
        layout.dir.join("fixture-ready"),
        _app.status().mcp_url.unwrap(),
    )
    .unwrap();
    loop {
        thread::park();
    }
}

#[test]
fn force_quit_releases_listener_and_owner_lock() {
    let (_dir, layout) = fixture();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "force_quit_fixture", "--nocapture"])
        .env("ERLKONIG_FORCE_QUIT_FIXTURE", &layout.dir)
        .spawn()
        .unwrap();
    let before = Instant::now();
    while !layout.dir.join("fixture-ready").exists() && before.elapsed() < Duration::from_secs(10) {
        thread::sleep(Duration::from_millis(20));
    }
    if !layout.dir.join("fixture-ready").exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("fixture did not start");
    }
    let url = fs::read_to_string(layout.dir.join("fixture-ready")).unwrap();
    // The fixture owns no helper processes; SIGKILL removes its native threads too.
    let output = Command::new("/bin/ps")
        .args(["-axo", "ppid="])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|l| l.trim() == child.id().to_string())
    );
    child.kill().unwrap();
    child.wait().unwrap();
    drop(TcpListener::bind(url.strip_prefix("http://").unwrap().trim_end_matches('/')).unwrap());
    let mut restarted = Runtime::start(&layout).unwrap();
    restarted.finish().unwrap();
}
