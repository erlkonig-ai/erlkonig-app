//! One-shot typed client on one existing owner's connection. No pile access,
//! discovery scan, launch, reconnect, session renewal, or mutation replay.
use super::*;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Deserialize, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

pub struct Client {
    sender: SendRequest<Full<Bytes>>,
    driver: tokio::task::JoinHandle<()>,
    address: SocketAddr,
    authorization: HeaderValue,
    session: Session,
    next_id: u64,
}

impl Client {
    pub async fn connect(layout: &Layout, url: &str) -> Result<Self> {
        let address = destination(url)?;
        let token = credentials::load(&layout.dir.join("mcp.token")).map_err(anyhow::Error::msg)?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| anyhow!("invalid local credential"))?;
        authorization.set_sensitive(true);
        drop(token);
        let socket = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(address))
            .await
            .map_err(|_| {
                anyhow!("Erlkonig is unavailable; start serve explicitly or open the app")
            })?
            .map_err(|_| {
                anyhow!("Erlkonig is unavailable; start serve explicitly or open the app")
            })?;
        let (mut sender, connection) = http1::Builder::new()
            .max_headers(32)
            .max_buf_size(32 * 1024)
            .handshake(TokioIo::new(socket))
            .await
            .map_err(|_| anyhow!("cannot establish the local owner connection"))?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let initialize = serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":0,
            "method":"initialize", "params":{"protocolVersion":PROTOCOL_VERSION,
            "capabilities":{}, "clientInfo":{"name":"erlkonig-cli", "version":env!("CARGO_PKG_VERSION")}}}))?;
        let initialized = async {
            let (parts, body) = post(
                &mut sender,
                initialize.clone(),
                address,
                &authorization,
                None,
            )
            .await?;
            rpc_result(&body, 0)?;
            let mut session = Session::from_initialize(&parts, &body, initialize)?;
            let (parts, _) = post(
                &mut sender,
                br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_vec(),
                address,
                &authorization,
                Some(&session),
            )
            .await?;
            if parts.status != StatusCode::ACCEPTED {
                bail!("local owner did not acknowledge initialization");
            }
            session.initialized = true;
            Ok(session)
        }
        .await;
        match initialized {
            Ok(session) => Ok(Self {
                sender,
                driver,
                address,
                authorization,
                session,
                next_id: 1,
            }),
            Err(error) => {
                driver.abort();
                Err(error)
            }
        }
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = serde_json::to_vec(
            &json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
        )?;
        if frame.len() > Limits::default().request_bytes {
            bail!("request exceeds the local MCP input limit");
        }
        let (parts, body) = post(
            &mut self.sender,
            frame,
            self.address,
            &self.authorization,
            Some(&self.session),
        )
        .await?;
        if parts.status == StatusCode::NOT_FOUND {
            bail!("MCP session expired; request was not executed and was not retried");
        }
        if parts.status != StatusCode::OK {
            bail!("local owner omitted the requested response; nothing was retried");
        }
        if parts
            .headers
            .get(SESSION_HEADER)
            .is_some_and(|id| *id != self.session.id)
        {
            bail!("local owner changed the MCP session; nothing was retried");
        }
        rpc_result(&body, id)
    }

    pub async fn tools(&mut self) -> Result<Vec<Tool>> {
        let result = self.rpc("tools/list", json!({})).await?;
        if result.get("nextCursor").is_some_and(|v| !v.is_null()) {
            bail!("local tool catalog unexpectedly requires pagination");
        }
        serde_json::from_value(
            result
                .get("tools")
                .cloned()
                .ok_or_else(|| anyhow!("missing tool catalog"))?,
        )
        .map_err(|_| anyhow!("invalid local tool catalog"))
    }

    /// Returns the complete MCP tool result, including isError and content.
    /// Caller errors remain data; a failed operation is never retried here.
    pub async fn call(&mut self, name: &str, arguments: Value) -> Result<Value> {
        if !arguments.is_object() {
            bail!("tool arguments must be a JSON object");
        }
        self.rpc("tools/call", json!({"name":name, "arguments":arguments}))
            .await
    }

    pub async fn close(mut self) {
        let cleanup = async {
            self.sender.ready().await.ok()?;
            let request = request(
                Method::DELETE,
                Vec::new(),
                self.address,
                &self.authorization,
                Some(&self.session),
            )
            .ok()?;
            let response = self.sender.send_request(request).await.ok()?;
            Limited::new(response.into_body(), Limits::default().response_bytes)
                .collect()
                .await
                .ok()
        };
        let _ = tokio::time::timeout(EOF_DRAIN, cleanup).await;
        self.driver.abort();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

fn rpc_result(body: &[u8], id: u64) -> Result<Value> {
    let response: Value =
        serde_json::from_slice(body).map_err(|_| anyhow!("invalid local MCP response"))?;
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || response.get("id").and_then(Value::as_u64) != Some(id)
        || response.get("result").is_some() == response.get("error").is_some()
    {
        bail!("invalid local MCP response envelope");
    }
    if response.get("error").is_some() {
        // Never reflect arbitrary server error bodies or credentials in diagnostics.
        bail!("local MCP rejected the request; nothing was retried");
    }
    Ok(response["result"].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_identity_and_exclusive_result_are_required() {
        assert_eq!(
            rpc_result(br#"{"jsonrpc":"2.0","id":4,"result":{}}"#, 4).unwrap(),
            json!({})
        );
        for body in [
            br#"{"jsonrpc":"2.0","id":3,"result":{}}"#.as_slice(),
            br#"{"jsonrpc":"2.0","id":4,"result":{},"error":{}}"#,
            br#"{"jsonrpc":"2.0","id":4,"error":{"message":"private"}}"#,
        ] {
            assert!(
                !rpc_result(body, 4)
                    .unwrap_err()
                    .to_string()
                    .contains("private")
            );
        }
    }
}
