struct HttpDaemonClient {
    client: Client,
    base_url: String,
    /// The daemon's bearer token.
    token: String,
}

impl HttpDaemonClient {
    fn new(port: u16, token: String) -> Self {
        Self {
            client: rightkit_http::http_client::daemon_client(&format!("http://127.0.0.1:{port}"))
                .expect("rightkit-http client builds with the platform verifier"),
            base_url: format!("http://127.0.0.1:{port}"),
            token,
        }
    }

    /// One daemon call: POST (`body`) or GET (`None`) on `url` with the daemon bearer
    /// token. Any status is a response; transport failures are `DaemonError::Http`.
    async fn call(
        &self,
        url: &str,
        body: Option<&Value>,
    ) -> Result<rightkit_http::Response, DaemonError> {
        let request = match body {
            Some(body) => rightkit_http::Request::post(url)
                .bearer(&self.token)
                .json(body)
                .map_err(|e| DaemonError::Http(e.to_string()))?,
            None => rightkit_http::Request::get(url).bearer(&self.token),
        };
        self.client
            .send_any(&request)
            .await
            .map_err(|e| DaemonError::Http(e.to_string()))
    }

    async fn ensure_session(
        &self,
        session_id: &str,
        title: &str,
        toolset: &str,
        mcp_channel: &str,
    ) -> Result<(), DaemonError> {
        let url = format!("{}/sessions", self.base_url);
        let body = serde_json::json!({
            "id": session_id,
            "title": title,
            "toolset": toolset,
            "mcpChannel": mcp_channel,
        });
        let resp = self.call(&url, Some(&body)).await?;

        match resp.status {
            409 => Ok(()),
            401 => Err(DaemonError::Unauthorized),
            s if !rightkit_http::http_client::is_success(s) => Err(DaemonError::Http(format!("HTTP {s}"))),
            _ => Ok(()),
        }
    }
}

fn approval_resolution_body(tool_call_id: &str, approved: bool) -> Value {
    serde_json::json!({
        "toolCallId": tool_call_id,
        "approved": approved,
    })
}

#[async_trait]
impl DaemonClient for HttpDaemonClient {
    async fn submit_message(&self, session_id: &str, text: &str) -> Result<String, DaemonError> {
        let url = format!("{}/sessions/{}/messages", self.base_url, session_id);
        let body = serde_json::json!({ "text": text });

        let resp = self.call(&url, Some(&body)).await?;

        match resp.status {
            404 => Err(DaemonError::SessionNotFound(session_id.to_string())),
            401 => Err(DaemonError::Unauthorized),
            s if !rightkit_http::http_client::is_success(s) => Err(DaemonError::Http(format!("HTTP {s}"))),
            _ => {
                let v: Value = resp.json().map_err(|e| DaemonError::Http(e.to_string()))?;
                // The daemon returns `{"accepted":true,"seq":N}`.
                let seq = v.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
                Ok(seq.to_string())
            }
        }
    }

    async fn submit_channel_message(
        &self,
        channel_id: &str,
        target_key: &str,
        session_id: &str,
        text: &str,
    ) -> Result<String, DaemonError> {
        let url = format!("{}/remote/channels/{}/fanout", self.base_url, channel_id);
        let body = serde_json::json!({
            "text": text,
            "source": "telegram-long-poll",
            "targetKey": target_key,
            "sessionId": session_id,
        });

        let resp = self.call(&url, Some(&body)).await?;

        match resp.status {
            404 => Err(DaemonError::SessionNotFound(session_id.to_string())),
            401 => Err(DaemonError::Unauthorized),
            s if !rightkit_http::http_client::is_success(s) => Err(DaemonError::Http(format!("HTTP {s}"))),
            _ => {
                let v: Value = resp.json().map_err(|e| DaemonError::Http(e.to_string()))?;
                if !v.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                    return Err(DaemonError::Http(
                        v.get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("remote fanout failed")
                            .to_string(),
                    ));
                }
                let seq = v
                    .get("results")
                    .and_then(Value::as_array)
                    .filter(|results| !results.is_empty())
                    .ok_or_else(|| {
                        DaemonError::Http("remote fanout matched no bindings".to_string())
                    })?
                    .first()
                    .and_then(|result| result.get("seq"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                Ok(seq.to_string())
            }
        }
    }

    async fn resolve_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        approved: bool,
    ) -> Result<(), DaemonError> {
        let url = format!("{}/sessions/{}/tool-approval", self.base_url, session_id);
        let body = approval_resolution_body(tool_call_id, approved);
        let resp = self.call(&url, Some(&body)).await?;
        match resp.status {
            404 => Err(DaemonError::SessionNotFound(session_id.to_string())),
            401 => Err(DaemonError::Unauthorized),
            s if !rightkit_http::http_client::is_success(s) => Err(DaemonError::Http(format!("HTTP {s}"))),
            _ => Ok(()),
        }
    }

    async fn events_after(
        &self,
        session_id: &str,
        after_seq: i64,
    ) -> Result<Vec<ProtocolEvent>, DaemonError> {
        let url = format!(
            "{}/sessions/{}/events/history?after={}",
            self.base_url, session_id, after_seq
        );

        let resp = self.call(&url, None).await?;

        match resp.status {
            404 => Err(DaemonError::SessionNotFound(session_id.to_string())),
            401 => Err(DaemonError::Unauthorized),
            s if !rightkit_http::http_client::is_success(s) => Err(DaemonError::Http(format!("HTTP {s}"))),
            _ => {
                let events: Vec<ProtocolEvent> =
                    resp.json().map_err(|e| DaemonError::Http(e.to_string()))?;
                Ok(events)
            }
        }
    }

    async fn record_binding(
        &self,
        channel_id: &str,
        target_key: &str,
        session_id: &str,
    ) -> Result<(), DaemonError> {
        let url = format!("{}/remote/bindings", self.base_url);
        let body = serde_json::json!({
            "channelId": channel_id,
            "targetKey": target_key,
            "sessionId": session_id,
        });
        let resp = self.call(&url, Some(&body)).await?;

        match resp.status {
            401 => Err(DaemonError::Unauthorized),
            s if !rightkit_http::http_client::is_success(s) => Err(DaemonError::Http(format!("HTTP {s}"))),
            _ => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Telegram Bot API helpers
// ---------------------------------------------------------------------------
