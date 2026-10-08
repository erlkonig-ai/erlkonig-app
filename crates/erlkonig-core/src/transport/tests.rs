use super::*;
use faculties::mcp::http;
use std::collections::HashMap;
use std::fs;
use std::os::fd::AsRawFd;
use tokio::io::{AsyncReadExt, DuplexStream};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":"init\nquoted","method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}"#;
const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
const CANCELLED: &str = r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"test"}}"#;
const INIT_RESPONSE: &str = r#"{"jsonrpc":"2.0","id":"init\nquoted","result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}}"#;

fn scratch() -> (tempfile::TempDir, Layout) {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path());
    credentials::load_or_create(&layout.dir.join("mcp.token")).unwrap();
    (dir, layout)
}

fn bridge(
    layout: Layout,
    url: String,
) -> (
    DuplexStream,
    BufReader<DuplexStream>,
    tokio::task::JoinHandle<Result<()>>,
) {
    let (client, input) = tokio::io::duplex(2 * 1024 * 1024);
    let (output, result) = tokio::io::duplex(16 * 1024 * 1024);
    let task = tokio::spawn(async move { forward(&layout, &url, input, output).await });
    (client, BufReader::new(result), task)
}

async fn send(client: &mut DuplexStream, frame: &str) {
    client.write_all(frame.as_bytes()).await.unwrap();
    client.write_all(b"\n").await.unwrap();
}

async fn line(output: &mut BufReader<DuplexStream>) -> serde_json::Value {
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(2), output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).expect("stdout contains only complete JSON-RPC lines")
}

async fn finished(task: tokio::task::JoinHandle<Result<()>>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
}

struct Post {
    method: String,
    headers: HashMap<String, String>,
    body: String,
}

async fn post(socket: &mut BufReader<TcpStream>) -> Post {
    let mut first = String::new();
    socket.read_line(&mut first).await.unwrap();
    let method = first.split_whitespace().next().unwrap().to_owned();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        socket.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .map_or(0, |n| n.parse().unwrap());
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    Post {
        method,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}

async fn reply(socket: &mut BufReader<TcpStream>, status: &str, extra: &str, body: &str) {
    socket
        .get_mut()
        .write_all(
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}\r\n{body}",
                body.len(),
            )
            .as_bytes(),
        )
        .await
        .unwrap();
}

async fn initialize(socket: &mut BufReader<TcpStream>) {
    assert_eq!(post(socket).await.body, INITIALIZE);
    reply(
        socket,
        "200 OK",
        "Content-Type: application/json\r\nMcp-Session-Id: fixture\r\n",
        INIT_RESPONSE,
    )
    .await;
}

#[test]
fn only_exact_loopback_destinations() {
    assert!(destination("http://127.0.0.1:1234/").is_ok());
    assert!(destination("http://[::1]:1234/").is_ok());
    for url in [
        "https://127.0.0.1:1234/",
        "http://localhost:1234/",
        "http://example.com/",
        "http://192.168.1.1:1234/",
        "http://0.0.0.0:1234/",
        "http://127.0.0.1:0/",
        "http://user:secret@127.0.0.1:1234/",
        "http://127.0.0.1:1234/redirect",
        "http://127.0.0.1:1234/?token=secret",
        "http://127.0.0.1:1234/#fragment",
        "http://127.0.0.1:1234",
        "http://[::ffff:127.0.0.1]:1234/",
    ] {
        assert!(destination(url).is_err(), "accepted {url}");
    }
}

#[test]
fn newline_framing_preserves_escaped_strings_and_exact_numbers() {
    let body = b"{\n \"jsonrpc\": \"2.0\", \"id\": 1234567890123456789012345678901234567890,\n \"result\": {\"text\":\"two\\nlines \\\"quoted\\\" \\\\ end\"}}\n";
    let compact = response_line(body).unwrap();
    assert!(!compact.contains(&b'\n'));
    assert!(
        compact
            .windows(40)
            .any(|b| b == b"1234567890123456789012345678901234567890")
    );
    assert!(compact.windows(10).any(|b| b == b"two\\nlines"));
    assert!(response_line(b"not json").is_err());
    assert!(response_line(b"[]").is_err());
}

#[tokio::test]
async fn real_native_protocol_session_and_eof_delete_without_pile() {
    let (_dir, layout) = scratch();
    let token = credentials::load(&layout.dir.join("mcp.token")).unwrap();
    let mut config = http::Config::new(
        "127.0.0.1:0".parse().unwrap(),
        http::BearerToken::new(&token).unwrap(),
    );
    config.max_sessions = 1;
    let endpoint = http::Endpoint::bind(config).unwrap();
    let url = format!("http://{}/", endpoint.local_addr().unwrap());
    let (stop, stopped) = oneshot::channel();
    let native = std::thread::spawn(move || {
        endpoint.serve_until(&[], async {
            let _ = stopped.await;
        })
    });
    let (mut client, mut output, task) = bridge(layout.clone(), url.clone());
    send(&mut client, INITIALIZE).await;
    assert_eq!(line(&mut output).await["id"], "init\nquoted");
    send(&mut client, INITIALIZED).await;
    send(&mut client, CANCELLED).await;
    send(
        &mut client,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )
    .await;
    let tools = line(&mut output).await;
    assert_eq!(tools["id"], 2); // Neither notification produced a stdout line.
    assert_eq!(tools["result"]["tools"], serde_json::json!([]));
    send(&mut client, r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"missing","arguments":{}}}"#).await;
    assert_eq!(line(&mut output).await["error"]["code"], -32602);
    send(&mut client, "{broken").await;
    assert_eq!(line(&mut output).await["error"]["code"], -32700);
    drop(client);
    finished(task).await.unwrap();
    // max_sessions=1: a new initialization succeeds only if EOF DELETE freed
    // the old session. No fresh session is created behind the first client.
    let (mut client, mut output, task) = bridge(layout.clone(), url);
    send(&mut client, INITIALIZE).await;
    assert!(line(&mut output).await.get("result").is_some());
    drop(client);
    finished(task).await.unwrap();
    stop.send(()).unwrap();
    native.join().unwrap().unwrap();
    assert!(!layout.default_pile().exists());
    assert!(!layout.default_key().exists());
    assert!(!layout.lock().exists());
}

#[tokio::test]
async fn messages_are_forwarded_unchanged_with_auth_session_and_version() {
    let (_dir, layout) = scratch();
    let token = credentials::load(&layout.dir.join("mcp.token")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let native = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        let init = post(&mut socket).await;
        assert!(
            init.headers["authorization"] == format!("Bearer {token}"),
            "missing private authorization"
        );
        assert_eq!(init.body, INITIALIZE);
        reply(
            &mut socket,
            "200 OK",
            "Content-Type: application/json\r\nMcp-Session-Id: fixture\r\n",
            INIT_RESPONSE,
        )
        .await;
        for body in [INITIALIZED, CANCELLED] {
            let request = post(&mut socket).await;
            assert_eq!(request.body, body);
            assert_eq!(request.headers[SESSION_HEADER], "fixture");
            assert_eq!(request.headers[VERSION_HEADER], PROTOCOL_VERSION);
            reply(&mut socket, "202 Accepted", "", "").await;
        }
        let delete = post(&mut socket).await;
        assert_eq!(delete.method, "DELETE");
        assert_eq!(delete.headers[SESSION_HEADER], "fixture");
        reply(&mut socket, "204 No Content", "", "").await;
    });
    let (mut client, mut output, task) = bridge(layout, url);
    send(&mut client, INITIALIZE).await;
    line(&mut output).await;
    send(&mut client, INITIALIZED).await;
    send(&mut client, CANCELLED).await;
    drop(client);
    finished(task).await.unwrap();
    native.await.unwrap();
}

#[tokio::test]
async fn idle_owner_loss_exits_with_stdin_open_and_never_reconnects() {
    let (_dir, layout) = scratch();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (close, closed) = oneshot::channel();
    let native = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        initialize(&mut socket).await;
        closed.await.unwrap();
        drop(socket);
        // The endpoint remains available to a replacement owner, but the
        // transport must never make a second connection to it.
        assert!(
            tokio::time::timeout(Duration::from_millis(300), listener.accept())
                .await
                .is_err()
        );
    });
    let (mut client, mut output, task) = bridge(layout, url);
    send(&mut client, INITIALIZE).await;
    line(&mut output).await;
    close.send(()).unwrap();
    assert!(finished(task).await.is_err());
    let mut remaining = String::new();
    output.read_to_string(&mut remaining).await.unwrap();
    assert!(remaining.is_empty());
    drop(client);
    native.await.unwrap();
}

#[tokio::test]
async fn inflight_loss_does_not_retry_or_forge_a_response() {
    let (_dir, layout) = scratch();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let native = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        initialize(&mut socket).await;
        assert_eq!(
            post(&mut socket).await.body,
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#
        );
        // Drop before the response: effects may exist, but there is no retry.
    });
    let (mut client, mut output, task) = bridge(layout, url);
    send(&mut client, INITIALIZE).await;
    line(&mut output).await;
    send(&mut client, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).await;
    assert!(finished(task).await.is_err());
    let mut remaining = String::new();
    output.read_to_string(&mut remaining).await.unwrap();
    assert!(remaining.is_empty());
    drop(client);
    native.await.unwrap();
}

#[tokio::test]
async fn client_eof_does_not_leave_transport_waiting_on_admitted_work() {
    let (_dir, layout) = scratch();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let (admitted, admission) = oneshot::channel();
    let native = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        initialize(&mut socket).await;
        post(&mut socket).await;
        admitted.send(()).unwrap();
        // No reply: the admitted operation could still be running. Client EOF
        // ends its transport, not the native operation or the backend owner.
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
    });
    let (mut client, mut output, task) = bridge(layout, url);
    send(&mut client, INITIALIZE).await;
    line(&mut output).await;
    send(&mut client, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).await;
    admission.await.unwrap();
    drop(client);
    finished(task).await.unwrap();
    let mut remaining = String::new();
    output.read_to_string(&mut remaining).await.unwrap();
    assert!(remaining.is_empty());
    tokio::time::timeout(Duration::from_secs(2), native)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn real_native_shutdown_closes_idle_transport_with_stdin_open() {
    let (_dir, layout) = scratch();
    let token = credentials::load(&layout.dir.join("mcp.token")).unwrap();
    let config = http::Config::new(
        "127.0.0.1:0".parse().unwrap(),
        http::BearerToken::new(&token).unwrap(),
    );
    let endpoint = http::Endpoint::bind(config).unwrap();
    let url = format!("http://{}/", endpoint.local_addr().unwrap());
    let (stop, stopped) = oneshot::channel();
    let native = std::thread::spawn(move || {
        endpoint.serve_until(&[], async {
            let _ = stopped.await;
        })
    });
    let (mut client, mut output, task) = bridge(layout, url);
    send(&mut client, INITIALIZE).await;
    line(&mut output).await;
    stop.send(()).unwrap();
    assert!(finished(task).await.is_err());
    drop(client);
    native.join().unwrap().unwrap();
}

#[tokio::test]
async fn final_json_response_is_flushed_before_owner_close() {
    let (_dir, layout) = scratch();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let native = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = BufReader::new(socket);
        post(&mut socket).await;
        reply(
            &mut socket,
            "200 OK",
            "Content-Type: application/json\r\nConnection: close\r\n",
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"bad init"}}"#,
        )
        .await;
    });
    let (mut client, mut output, task) = bridge(layout, url);
    send(&mut client, INITIALIZE).await;
    assert_eq!(line(&mut output).await["error"]["code"], -32600);
    assert!(finished(task).await.is_err());
    drop(client);
    native.await.unwrap();
}

#[tokio::test]
async fn redirects_sse_and_auth_errors_never_leak_body_or_follow_urls() {
    for (status, extra, body) in [
        (
            "302 Found",
            "Location: http://192.0.2.1/secret\r\n",
            "secret",
        ),
        (
            "200 OK",
            "Content-Type: text/event-stream\r\n",
            "data: secret\n\n",
        ),
        (
            "401 Unauthorized",
            "Content-Type: application/json\r\n",
            "secret",
        ),
    ] {
        let (_dir, layout) = scratch();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let native = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = BufReader::new(socket);
            post(&mut socket).await;
            reply(&mut socket, status, extra, body).await;
        });
        let (mut client, mut output, task) = bridge(layout, url);
        send(&mut client, INITIALIZE).await;
        let error = finished(task).await.unwrap_err().to_string();
        assert!(!error.contains("secret"));
        let mut stdout = String::new();
        output.read_to_string(&mut stdout).await.unwrap();
        assert!(stdout.is_empty());
        native.await.unwrap();
    }
}

#[tokio::test]
async fn absent_owner_or_token_never_creates_backend_state() {
    let dir = tempfile::tempdir().unwrap();
    let layout = Layout::new(dir.path().join("absent"));
    assert!(
        forward(
            &layout,
            "http://127.0.0.1:1234/",
            tokio::io::empty(),
            tokio::io::sink()
        )
        .await
        .is_err()
    );
    assert!(!layout.dir.exists());
    let (_dir, layout) = scratch();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    drop(listener);
    assert!(
        forward(&layout, &url, tokio::io::empty(), tokio::io::sink())
            .await
            .is_err()
    );
    assert_eq!(fs::read_dir(&layout.dir).unwrap().count(), 1); // Existing token only.
}

#[tokio::test]
async fn frame_limit_is_bounded_before_http_dispatch() {
    let input = vec![b'x'; Limits::default().request_bytes + 1];
    let (tx, mut rx) = mpsc::channel(1);
    assert!(read_input(input.as_slice(), tx).await.is_err());
    assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn inherited_pipe_readiness_is_cancellable_and_restores_flags() {
    let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
    let original = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
    let mut input = unix_io::Stdio::duplicate(reader.as_raw_fd()).unwrap();
    let mut byte = [0];
    assert!(
        tokio::time::timeout(Duration::from_millis(10), input.read_exact(&mut byte))
            .await
            .is_err()
    );
    std::io::Write::write_all(&mut writer, b"x").unwrap();
    input.read_exact(&mut byte).await.unwrap();
    assert_eq!(byte, [b'x']);
    drop(input);
    assert_eq!(
        unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) },
        original
    );
}
