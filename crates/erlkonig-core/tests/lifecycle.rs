use erlkonig_core::{credentials, paths::Layout, runtime::Runtime};
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
    fs::write(layout.config(), "mcp_port = 0\n").unwrap();
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
    assert!(native.get("error").is_none(), "{native}");
    assert_ne!(native["result"]["isError"], true, "{native}");
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
    fs::write(layout.config(), "mcp_port = 0\nkey = 'missing.key'\n").unwrap();
    let error = Runtime::start(&layout).err().unwrap().to_string();
    assert!(error.contains("signing identity"), "{error}");
    assert!(!layout.dir.join("missing.key").exists());
}

// This is a test-harness process, never a shipped daemon/headless app mode.
#[test]
fn force_quit_fixture() {
    let Some(dir) = std::env::var_os("ERLKONIG_FORCE_QUIT_FIXTURE") else {
        return;
    };
    let layout = Layout::new(dir);
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
