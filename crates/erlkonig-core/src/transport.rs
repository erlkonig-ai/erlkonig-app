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
//! An explicit HTTP 404 for an established session renews only that protocol
//! session on the pinned connection. The rejected request receives an error;
//! it is never replayed. Future requests may use the renewed session.
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
        .map_err(|_| anyhow!("Erlkonig is unavailable; open the app first"))?
        .map_err(|_| anyhow!("Erlkonig is unavailable; open the app first"))?;
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
            bail!("Erlkonig's connection closed; an admitted operation may have completed; nothing was retried")
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
    initialize: Vec<u8>,
    initialized: bool,
}

impl Session {
    fn from_initialize(
        parts: &hyper::http::response::Parts,
        body: &[u8],
        initialize: Vec<u8>,
    ) -> Result<Self> {
        #[derive(Deserialize)]
        struct Initialized {
            result: Protocol,
        }
        #[derive(Deserialize)]
        struct Protocol {
            #[serde(rename = "protocolVersion")]
            version: String,
        }
        let initialization: Initialized = serde_json::from_slice(body)
            .map_err(|_| anyhow!("invalid local MCP initialization response"))?;
        let id = parts
            .headers
            .get(SESSION_HEADER)
            .ok_or_else(|| anyhow!("local MCP initialization omitted its session"))?;
        if parts.status != StatusCode::OK
            || initialization.result.version != PROTOCOL_VERSION
            || id.is_empty()
            || id.as_bytes().len() > 128
            || !id.as_bytes().iter().all(|b| (0x21..=0x7e).contains(b))
        {
            bail!("unsupported native MCP session or protocol version");
        }
        Ok(Self {
            id: id.clone(),
            version: HeaderValue::from_static(PROTOCOL_VERSION),
            initialize,
            initialized: false,
        })
    }
}

#[derive(Deserialize)]
struct Message<'a> {
    jsonrpc: String,
    method: Option<String>,
    #[serde(borrow, default, deserialize_with = "present")]
    id: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    params: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    result: Option<&'a RawValue>,
    #[serde(borrow, default, deserialize_with = "present")]
    error: Option<&'a RawValue>,
}

// JSON null is present, not an omitted ID/params/result/error. In particular,
// an invalid initialized notification with params:null must not make us ready.
fn present<'de, D: serde::Deserializer<'de>>(
    value: D,
) -> std::result::Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(value).map(Some)
}

impl Message<'_> {
    fn initialized(&self) -> bool {
        self.jsonrpc == "2.0"
            && self.method.as_deref() == Some("notifications/initialized")
            && self.id.is_none()
            && self.result.is_none()
            && self.error.is_none()
            && self
                .params
                .is_none_or(|params| params.get().starts_with('{'))
    }
}

fn expired_response(frame: &[u8]) -> Result<Option<Vec<u8>>> {
    let message: Message<'_> = serde_json::from_slice(frame)
        .map_err(|_| anyhow!("expired session received an invalid MCP envelope"))?;
    // No responses to notifications or to unsolicited responses. The native
    // backend cannot dispatch an operation for either of those message kinds.
    if message.method.is_none() || message.id.is_none() {
        return Ok(None);
    }
    let id = message.id.unwrap().get();
    if message.jsonrpc != "2.0"
        || message.result.is_some()
        || message.error.is_some()
        || !(id.starts_with('"') || id.starts_with('-') || id.as_bytes()[0].is_ascii_digit())
    {
        bail!("expired session received an invalid MCP request");
    }
    // RawValue preserves request IDs, including large integers, exactly.
    response_line(format!(r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32000,"message":"MCP session expired; this request was not executed. The session has been renewed for future requests."}}}}"#).as_bytes()).map(Some)
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
        let (parts, body) = post(
            &mut sender,
            frame.clone(),
            address,
            &authorization,
            session.as_ref(),
        )
        .await?;
        if parts.status == StatusCode::NOT_FOUND {
            // This exact backend rejects a missing/expired session before RPC
            // dispatch. Consume that bounded response, renew on the SAME socket,
            // and reject the request to our client. Never resubmit the frame.
            let rejected = expired_response(&frame)?;
            let old = session
                .take()
                .expect("post accepts 404 only with a session");
            let (parts, body) = post(
                &mut sender,
                old.initialize.clone(),
                address,
                &authorization,
                None,
            )
            .await?;
            let mut renewed = Session::from_initialize(&parts, &body, old.initialize)?;
            let initialized = old.initialized
                || serde_json::from_slice::<Message<'_>>(&frame).is_ok_and(|m| m.initialized());
            if initialized {
                let (parts, _) = post(
                    &mut sender,
                    br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_vec(),
                    address,
                    &authorization,
                    Some(&renewed),
                )
                .await?;
                if parts.status != StatusCode::ACCEPTED {
                    bail!("local MCP session renewal was not acknowledged");
                }
                renewed.initialized = true;
            }
            session = Some(renewed);
            if let Some(line) = rejected {
                output.write_all(&line).await?;
                output.write_all(b"\n").await?;
                output.flush().await?;
            }
            continue;
        }
        if parts.status == StatusCode::ACCEPTED {
            if let Some(session) = session.as_mut() {
                session.initialized |= serde_json::from_slice::<Message<'_>>(&frame)
                    .is_ok_and(|message| message.initialized());
            }
            continue;
        }
        if let Some(id) = parts.headers.get(SESSION_HEADER) {
            if session.as_ref().is_some_and(|s| s.id != *id) {
                bail!("application changed MCP session; refusing replacement owner");
            }
            if session.is_none() {
                session = Some(Session::from_initialize(&parts, &body, frame)?);
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

// A 404 is meaningful only for a session already established with this pinned
// backend. Other HTTP errors, all I/O failures, and malformed responses remain
// terminal. No credential, header, or error body is written to stdout/stderr.
async fn post(
    sender: &mut SendRequest<Full<Bytes>>,
    frame: Vec<u8>,
    address: SocketAddr,
    authorization: &HeaderValue,
    session: Option<&Session>,
) -> Result<(hyper::http::response::Parts, Bytes)> {
    sender
        .ready()
        .await
        .map_err(|_| anyhow!("application connection closed; request was not retried"))?;
    let response = sender.send_request(request(Method::POST, frame, address, authorization, session)?)
        .await
        .map_err(|_| anyhow!("application connection failed; operation outcome may be unknown; nothing was retried"))?;
    let (parts, body) = response.into_parts();
    if !matches!(parts.status, StatusCode::OK | StatusCode::ACCEPTED)
        && !(parts.status == StatusCode::NOT_FOUND && session.is_some())
    {
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
        .collect()
        .await
        .map_err(|_| {
            anyhow!(
                "local MCP response was interrupted or exceeded its byte limit; nothing was retried"
            )
        })?
        .to_bytes();
    if parts.status == StatusCode::ACCEPTED && !body.is_empty() {
        bail!("notification acknowledgement unexpectedly contained a body");
    }
    if parts.headers.get_all(SESSION_HEADER).iter().count() > 1 {
        bail!("ambiguous local MCP session");
    }
    Ok((parts, body))
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
