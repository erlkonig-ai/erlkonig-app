//! Client-owned stdio framing for the already-running native application.
//!
//! This is deliberately the adapter for `faculties::mcp::http`, not a generic
//! MCP proxy. That backend has sequential dispatch, JSON POST responses and
//! empty 202 responses for notifications. It advertises no server requests,
//! subscriptions, SSE or list-change notifications. All input messages,
//! including cancellation notifications, are forwarded unchanged. The backend
//! currently ignores cancellation; disconnecting never undoes an admitted call.
//! An unexpected streaming response fails closed instead of dropping events.
//!
//! One TCP connection pins one owner. There is no DNS, proxy, redirect, retry,
//! reconnect, app launch, token creation, pile access, or service ownership.
//! Its connection driver is polled even while stdin/stdout or a call is idle.
//! App shutdown drains admitted work and closes this connection; Force Quit
//! closes it immediately. A replacement owner cannot inherit this transport.

mod unix_io;

use crate::{credentials, paths::Layout};
use anyhow::{Result, anyhow, bail};
use faculties::mcp::{Limits, PROTOCOL_VERSION};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::client::conn::http1::{self, SendRequest};
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST, HeaderValue};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

const SESSION_HEADER: &str = "mcp-session-id";
const VERSION_HEADER: &str = "mcp-protocol-version";
const EOF_DRAIN: Duration = Duration::from_millis(250);
const CLOSE_DRAIN: Duration = Duration::from_millis(25);

/// The command-line entrypoint. Only inherited stdio and the existing private
/// credential are opened; in particular, this never calls `Runtime::start`.
pub fn stdio(layout: &Layout, url: &str) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // Tokio's io::stdin uses an uncancellable blocking thread, which can
        // prevent runtime shutdown after the application disappears.
        let input = unix_io::Stdio::duplicate(libc::STDIN_FILENO)?;
        let output = unix_io::Stdio::duplicate(libc::STDOUT_FILENO)?;
        forward(layout, url, input, output).await
    })
}

fn destination(url: &str) -> Result<SocketAddr> {
    let address = url
        .strip_prefix("http://")
        .and_then(|s| s.strip_suffix('/'))
        .and_then(|s| s.parse::<SocketAddr>().ok())
        .filter(|a| a.ip().is_loopback() && a.port() != 0)
        .filter(|a| format!("http://{a}/") == url)
        .ok_or_else(|| {
            anyhow!("transport requires an exact HTTP loopback IP URL with a port and root path")
        })?;
    Ok(address)
}

/// Forward one client lifetime; generic I/O permits isolated fixture tests.
/// An orderly stdin EOF gets a bounded opportunity to flush pending replies
/// and DELETE its session. Neither EOF nor any network fault retries work.
pub async fn forward(
    layout: &Layout,
    url: &str,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> Result<()> {
    let address = destination(url)?;
    let token = credentials::load(&layout.dir.join("mcp.token")).map_err(anyhow::Error::msg)?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| anyhow!("invalid local credential"))?;
    authorization.set_sensitive(true);
    drop(token);
    let socket = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(address))
        .await
        .map_err(|_| anyhow!("Erlkönig is unavailable; open the app first"))?
        .map_err(|_| anyhow!("Erlkönig is unavailable; open the app first"))?;
    let (sender, connection) = http1::Builder::new()
        .max_headers(32)
        .max_buf_size(32 * 1024)
        .handshake(TokioIo::new(socket))
        .await
        .map_err(|_| anyhow!("cannot establish the application connection"))?;
    let (frames, incoming) = mpsc::channel(16);
    let reader = read_input(input, frames);
    let relay = exchange(sender, incoming, output, address, authorization);
    tokio::pin!(reader, relay, connection);

    tokio::select! {
        biased;
        result = &mut relay => result,
        result = &mut reader => {
            result?;
            // Reader EOF closes the queue. Do not leave a transport hanging
            // behind an admitted native call after its client has gone away.
            tokio::select! {
                biased;
                result = &mut relay => result,
                _ = &mut connection => {
                    let _ = tokio::time::timeout(CLOSE_DRAIN, &mut relay).await;
                    Ok(())
                }
                _ = tokio::time::sleep(EOF_DRAIN) => Ok(()),
            }
        }
        _ = &mut connection => {
            // The driver may just have delivered a final complete response.
            // Give that response a bounded chance to reach stdout, but never
            // wait for another client message after the pinned owner closes.
            let _ = tokio::time::timeout(CLOSE_DRAIN, &mut relay).await;
            bail!("Erlkönig's connection closed; an admitted operation may have completed; nothing was retried")
        }
    }
}

async fn read_input(input: impl AsyncRead + Unpin, frames: mpsc::Sender<Vec<u8>>) -> Result<()> {
    let mut input = BufReader::new(input);
    let limit = Limits::default().request_bytes;
    loop {
        let mut frame = Vec::new();
        loop {
            let bytes = input.fill_buf().await?;
            if bytes.is_empty() {
                if !frame.is_empty() {
                    let _ = frames.send(frame).await;
                }
                return Ok(());
            }
            let newline = bytes.iter().position(|b| *b == b'\n');
            let count = newline.unwrap_or(bytes.len());
            if frame.len() + count > limit {
                bail!("stdio request exceeds the native MCP input limit");
            }
            frame.extend_from_slice(&bytes[..count]);
            input.consume(count + usize::from(newline.is_some()));
            if newline.is_some() {
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                break;
            }
        }
        if frames.send(frame).await.is_err() {
            return Ok(());
        }
    }
}

struct Session {
    id: HeaderValue,
    version: HeaderValue,
}

fn request(
    method: Method,
    body: Vec<u8>,
    address: SocketAddr,
    authorization: &HeaderValue,
    session: Option<&Session>,
) -> Result<Request<Full<Bytes>>> {
    let mut request = Request::builder()
        .method(method)
        .uri("/")
        .header(HOST, address.to_string())
        .header(AUTHORIZATION, authorization)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json, text/event-stream");
    if let Some(session) = session {
        request = request
            .header(SESSION_HEADER, &session.id)
            .header(VERSION_HEADER, &session.version);
    }
    request
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| anyhow!("cannot construct the local MCP request"))
}

async fn exchange(
    mut sender: SendRequest<Full<Bytes>>,
    mut incoming: mpsc::Receiver<Vec<u8>>,
    mut output: impl AsyncWrite + Unpin,
    address: SocketAddr,
    authorization: HeaderValue,
) -> Result<()> {
    let mut session: Option<Session> = None;
    while let Some(frame) = incoming.recv().await {
        sender
            .ready()
            .await
            .map_err(|_| anyhow!("application connection closed; request was not retried"))?;
        let response = sender
            .send_request(request(Method::POST, frame, address, &authorization, session.as_ref())?)
            .await
            .map_err(|_| anyhow!("application connection failed; operation outcome may be unknown; nothing was retried"))?;
        let (parts, body) = response.into_parts();
        if !matches!(parts.status, StatusCode::OK | StatusCode::ACCEPTED) {
            // Do not print the body or any headers: either could contain a
            // credential, and HTTP error text is not a stdio MCP message.
            bail!(
                "local MCP returned HTTP {}; transport stopped without retry",
                parts.status.as_u16()
            );
        }
        if parts.status == StatusCode::OK
            && parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .map(str::trim)
                != Some("application/json")
        {
            bail!("unsupported local MCP response type; this backend must return JSON, not SSE");
        }
        let body = Limited::new(body, Limits::default().response_bytes)
            .collect().await
            .map_err(|_| anyhow!("local MCP response was interrupted or exceeded its byte limit; nothing was retried"))?
            .to_bytes();
        if parts.status == StatusCode::ACCEPTED {
            if !body.is_empty() {
                bail!("notification acknowledgement unexpectedly contained a body");
            }
            continue;
        }
        if parts.headers.get_all(SESSION_HEADER).iter().count() > 1 {
            bail!("ambiguous local MCP session");
        }
        if let Some(id) = parts.headers.get(SESSION_HEADER) {
            if session.as_ref().is_some_and(|s| s.id != *id) {
                bail!("application changed MCP session; refusing replacement owner");
            }
            if session.is_none() {
                #[derive(Deserialize)]
                struct Initialized {
                    result: Protocol,
                }
                #[derive(Deserialize)]
                struct Protocol {
                    #[serde(rename = "protocolVersion")]
                    version: String,
                }
                let initialization: Initialized = serde_json::from_slice(&body)
                    .map_err(|_| anyhow!("invalid local MCP initialization response"))?;
                if initialization.result.version != PROTOCOL_VERSION
                    || id.is_empty()
                    || id.as_bytes().len() > 128
                    || !id.as_bytes().iter().all(|b| (0x21..=0x7e).contains(b))
                {
                    bail!("unsupported native MCP session or protocol version");
                }
                session = Some(Session {
                    id: id.clone(),
                    version: HeaderValue::from_static(PROTOCOL_VERSION),
                });
            }
        }
        let line = response_line(&body)?;
        output.write_all(&line).await?;
        output.write_all(b"\n").await?;
        output.flush().await?;
    }
    if session.is_some() {
        // The outer EOF deadline also bounds this best-effort DELETE. Session
        // cleanup is not cancellation, and must never reconnect after a fault.
        if sender.ready().await.is_ok() {
            if let Ok(response) = sender
                .send_request(request(
                    Method::DELETE,
                    Vec::new(),
                    address,
                    &authorization,
                    session.as_ref(),
                )?)
                .await
            {
                let _ = Limited::new(response.into_body(), Limits::default().response_bytes)
                    .collect()
                    .await;
            }
        }
    }
    Ok(())
}

fn response_line(body: &[u8]) -> Result<Vec<u8>> {
    // Validate without decoding/re-encoding IDs or numbers (which could lose
    // precision). Remove only JSON whitespace outside string literals.
    let raw: &RawValue = serde_json::from_slice(body)
        .map_err(|_| anyhow!("local MCP response is not valid JSON"))?;
    if !raw.get().starts_with('{') {
        bail!("local MCP response is not a JSON-RPC object");
    }
    let mut line = Vec::with_capacity(body.len());
    let (mut quoted, mut escaped) = (false, false);
    for &byte in raw.get().as_bytes() {
        if quoted {
            line.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
            line.push(byte);
        } else if !byte.is_ascii_whitespace() {
            line.push(byte);
        }
    }
    Ok(line)
}

#[cfg(test)]
mod tests;
