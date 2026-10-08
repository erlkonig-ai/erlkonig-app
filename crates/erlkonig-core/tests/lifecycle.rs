use erlkonig_core::{
    config::Bootstrap,
    credentials, init,
    node_store::{ManagedPile, NodeStore},
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
    node.put_mcp_port(&view, 0).unwrap();
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
    let listed: Value = request("tools/call", json!({"name":"list_workspaces","arguments":{}}))
        .json()
        .unwrap();
    assert!(listed.get("error").is_none(), "{listed}");
    assert_ne!(listed["result"]["isError"], true, "{listed}");
    let workspaces: Value = serde_json::from_str(
        listed["result"]["content"][0]["text"].as_str().unwrap(),
    )
    .unwrap();
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

#[test]
fn missing_named_key_is_reported_without_fallback() {
    let (_dir, layout) = fixture();
    let mut app = Runtime::start(&layout).unwrap();
    let mut managed = ManagedPile::new(
        "missing key",
        layout.dir.join("external.pile"),
        layout.dir.join("missing.key"),
    );
    fs::write(&managed.pile, []).unwrap();
    managed.sync_enabled = true;
    app.put_managed(&app.config().unwrap(), &managed).unwrap();
    wait_for(&app, |s| {
        s.managed.iter().any(|p| {
            p.problem
                .as_ref()
                .is_some_and(|e| e.contains("signing identity"))
        })
    });
    assert!(!layout.dir.join("missing.key").exists());
    app.finish().unwrap();
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

fn managed_fixture(dir: &std::path::Path, name: &str) -> ManagedPile {
    use triblespace::core::{collection::CollectionStoreExt, metadata, signing_key_file};
    use triblespace::prelude::*;
    let pile = dir.join(format!("{name}.pile"));
    let key = dir.join(format!("{name}.key"));
    fs::write(&pile, []).unwrap();
    signing_key_file::init(&key).unwrap();
    let signer = signing_key_file::load_existing(&key).unwrap();
    let mut store = faculties::storage::open_pile_strict_as(&pile, signer.verifying_key()).unwrap();
    let collection = store
        .collection(
            name,
            faculties::collection_names::private_policy(signer.verifying_key()),
        )
        .unwrap();
    store
        .commit(
            collection,
            &signer,
            entity! { metadata::name: name.to_owned() },
        )
        .unwrap();
    let mut config = ManagedPile::new(name, pile, key);
    config.sync_collections.push(collection.handle());
    config.maintain_targets.push(collection.handle());
    store.close().unwrap();
    config
}

#[test]
fn two_piles_reconfigure_stop_persist_and_metrics_do_not_restart_workers() {
    let (dir, layout) = fixture();
    let mut first = managed_fixture(dir.path(), "first");
    let mut second = managed_fixture(dir.path(), "second");
    first.sync_enabled = true;
    first.maintenance_enabled = true;
    second.maintenance_enabled = true;
    let mut app = Runtime::start(&layout).unwrap();
    app.put_managed(&app.config().unwrap(), &first).unwrap();
    app.put_managed(&app.config().unwrap(), &second).unwrap();
    wait_for(&app, |s| {
        s.managed.len() == 2
            && s.managed.iter().all(|p| p.maintenance_running)
            && s.managed.iter().any(|p| p.sync_running)
    });
    let before = app.status();
    let sockets: Vec<String> = before
        .managed
        .iter()
        .flat_map(|p| p.bound_sockets.clone())
        .collect();
    assert!(
        !sockets.is_empty(),
        "sync session owns real native UDP sockets"
    );
    thread::sleep(Duration::from_millis(800));
    assert_eq!(
        before.config_generation,
        app.status().config_generation,
        "metrics writes must not invalidate config"
    );
    assert!(app.status().managed.iter().all(|p| p.sessions_started == 1));
    first.sync_enabled = false;
    app.put_managed(&app.config().unwrap(), &first).unwrap();
    wait_for(&app, |s| {
        s.managed.iter().all(|p| !p.sync_running)
            && s.managed.iter().all(|p| p.maintenance_running)
            && s.managed
                .iter()
                .find(|p| p.label == "first")
                .is_some_and(|p| p.sessions_started == 2)
    });
    for socket in sockets {
        drop(
            std::net::UdpSocket::bind(socket)
                .expect("Sync Off releases native UDP sockets while local maintenance continues"),
        );
    }
    second.maintenance_enabled = false;
    app.put_managed(&app.config().unwrap(), &second).unwrap();
    wait_for(&app, |s| {
        s.managed
            .iter()
            .find(|p| p.label == "second")
            .is_some_and(|p| !p.maintenance_running && !p.maintenance_enabled)
    });
    app.finish().unwrap();
    let signer = triblespace::core::signing_key_file::load_existing(&layout.default_key()).unwrap();
    let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
    let reports = node
        .metrics(
            erlkonig_core::node_metrics::now_tai_ns().unwrap(),
            Duration::from_secs(60),
        )
        .unwrap();
    assert!(
        !reports.is_empty(),
        "actual native sessions/maintenance emit metrics in node pile"
    );
    node.close().unwrap();
    let mut app = Runtime::start(&layout).unwrap();
    wait_for(&app, |s| {
        s.managed.len() == 2
            && s.managed
                .iter()
                .any(|p| p.label == "first" && p.maintenance_running)
    });
    assert!(app.status().managed.iter().all(|p| !p.sync_running));
    assert!(
        app.status()
            .managed
            .iter()
            .any(|p| p.label == "second" && !p.maintenance_running && !p.maintenance_enabled)
    );
    app.finish().unwrap();
}

#[test]
fn duplicate_alias_and_signer_activation_is_rejected_without_taking_over() {
    let (dir, layout) = fixture();
    let mut first = managed_fixture(dir.path(), "one");
    first.sync_enabled = true;
    let mut alias = first.clone();
    alias.id = triblespace::core::id::genid().id;
    alias.label = "alias".into();
    alias.pile = dir.path().join("hardlink.pile");
    fs::hard_link(&first.pile, &alias.pile).unwrap();
    let mut app = Runtime::start(&layout).unwrap();
    app.put_managed(&app.config().unwrap(), &first).unwrap();
    app.put_managed(&app.config().unwrap(), &alias).unwrap();
    wait_for(&app, |s| {
        s.managed.len() == 2
            && s.managed.iter().all(|p| {
                p.problem
                    .as_ref()
                    .is_some_and(|e| e.contains("same pile file"))
            })
    });
    assert!(app.status().managed.iter().all(|p| !p.sync_running));
    let other = managed_fixture(dir.path(), "other");
    alias.pile = other.pile;
    alias.sync_collections = other.sync_collections;
    app.put_managed(&app.config().unwrap(), &alias).unwrap();
    wait_for(&app, |s| {
        s.managed.iter().all(|p| {
            p.problem
                .as_ref()
                .is_some_and(|e| e.contains("signing identity"))
        })
    });
    assert!(app.status().managed.iter().all(|p| !p.sync_running));
    app.finish().unwrap();
}

#[test]
fn unreadable_config_stops_existing_managed_sessions() {
    use std::io::Write;
    let (dir, layout) = fixture();
    let mut entry = managed_fixture(dir.path(), "local");
    entry.maintenance_enabled = true;
    let mut app = Runtime::start(&layout).unwrap();
    app.put_managed(&app.config().unwrap(), &entry).unwrap();
    wait_for(&app, |s| s.managed.iter().any(|p| p.maintenance_running));
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(layout.default_pile())
        .unwrap();
    file.write_all(&[0xff; 256]).unwrap();
    file.sync_all().unwrap();
    wait_for(&app, |s| {
        s.config_problem.is_some() && s.managed.iter().all(|p| !p.maintenance_running)
    });
    let _ = app.finish(); // Corrupt control-store shutdown can report its read fault.
}

// This is a test-harness process, never a shipped daemon/headless app mode.
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
    let signer = init::prepare(&Bootstrap::new(&layout.dir)).unwrap();
    let mut node = NodeStore::open(&layout.default_pile(), signer).unwrap();
    let view = node.config().unwrap();
    node.put_mcp_port(&view, 0).unwrap();
    let first = managed_fixture(&root, "Scratch One");
    let second = managed_fixture(&root, "Scratch Two");
    let view = node.config().unwrap();
    node.put_managed(&view, &first).unwrap();
    node.close().unwrap();
    fs::write(root.join("fixture.json"), serde_json::to_vec_pretty(&json!({
        "home": home,
        "first": { "pile": first.pile, "key": first.key, "collection": format!("blake3:{}", hex::encode(first.sync_collections[0].raw)) },
        "second": { "pile": second.pile, "key": second.key, "collection": format!("blake3:{}", hex::encode(second.sync_collections[0].raw)) }
    })).unwrap()).unwrap();
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
        node.put_mcp_port(&expected, 0).unwrap();
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
