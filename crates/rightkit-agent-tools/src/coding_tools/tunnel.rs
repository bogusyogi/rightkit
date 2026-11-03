//! Tunnel provider registry — optional, guarded exposure of a local
//! `host:port` over an external tunnel provider (ENG-102, Dispatch M3 Lane A5).
//!
//! EXTERNAL ARTIFACT GAP: exercising [`CloudflareTunnelProvider`] against the
//! live API requires a real Cloudflare account with Tunnel (`cfd_tunnel`)
//! access and an API token scoped to `Account.Cloudflare Tunnel:Edit`. No
//! such account is configured in this workspace; private account proof stays
//! external, and this edit-only lane runs no checks.
//!
//! Provider chosen: **Cloudflare Tunnel**, imitating
//! `crate::coding_tools::cloudflare::CloudflareTool`'s credentialed-external-service
//! pattern exactly: 15s request timeout, redirects disabled, 512 KiB bounded
//! response, a `PermissionEngine::new().evaluate(..., CapabilityClass::Network,
//! ...)` gate on every mutating action, and the token is never included in
//! any `ToolResult` content or log line. This file manages only the
//! Cloudflare Tunnel *control plane* (create / route / delete a `cfd_tunnel`
//! resource and configure its ingress) over the documented Cloudflare API v4
//! endpoints (`accounts/{account}/cfd_tunnel`,
//! `.../cfd_tunnel/{id}/configurations`). Connector deployment remains an
//! execution-target concern for `dev_tunnel_proxy.rs`.
//!
//! ## Frozen for ENG-103
//!
//! The [`TunnelProvider`] trait, [`TunnelRegistry`], [`TunnelStartRequest`],
//! [`TunnelHandle`], [`TunnelStatus`], and the session-scoped credential/
//! exposure helpers below are the frozen surface ENG-103 consumes. Adding a
//! second provider (e.g. Tailscale Serve) only requires a new
//! `impl TunnelProvider` and a `registry.register(...)` call — the trait
//! shape does not change.
//!
//! ## Session-bound exposure lifetime
//!
//! An exposure never survives the session that opened it unless the caller
//! passes `keep_alive: true` to the `start` action. [`retire_tunnel_session`]
//! must be called on session teardown (the same place
//! `crate::coding_tools::context::retire_cloudflare_credential` is called today, in
//! `engine/crates/engine/src/engine_session_controls.rs`) — it stops every
//! exposure opened by that session that was not marked `keep_alive`, and
//! always discards the session's stored provider tokens.
//!
//! ## Integration this lane does NOT own
//!
//! - `tools/src/lib.rs` needs `pub mod tunnel;` alongside the other `pub mod`
//!   declarations (see `cloudflare` at line 37).
//! - `tools/src/registry/registry_impl.rs` needs
//!   `reg.register(Box::new(tunnel::TunnelTool::new()));` next to the
//!   `CloudflareTool` registration (line 207).
//! - `engine/crates/engine/src/engine_session_controls.rs` needs a call to
//!   `crate::coding_tools::tunnel::retire_tunnel_session(session_id)` alongside its
//!   existing `retire_cloudflare_credential(session_id)` call, so an exposure
//!   is force-stopped and its token dropped when the owning session ends.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
#[cfg(feature = "coding_tools-host")]
use rightkit_sandbox::execution::{
    CommandLaunchSpec, ManagedChild, ProcessLaunchAdapter, ProcessLaunchContext,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 512 * 1024;

/// A single request to expose `local_host:local_port` publicly.
#[derive(Debug, Clone)]
pub struct TunnelStartRequest {
    pub local_host: String,
    pub local_port: u16,
    /// Human-readable label for the exposure (e.g. shown in provider UI).
    pub label: Option<String>,
    /// DNS hostname already mapped to this tunnel in provider account.
    pub public_hostname: Option<String>,
}

/// the host app only exposes its own loopback listener. This is deliberately a
/// narrow host-exposure seam, not generic port forwarding or a way to publish
/// an arbitrary LAN service.
pub fn validate_local_target(host: &str, port: u16) -> Result<(), String> {
    if port == 0 {
        return Err("local_port must be between 1 and 65535.".to_string());
    }
    match host.trim() {
        "127.0.0.1" | "localhost" | "::1" => Ok(()),
        _ => Err(
            "Only a Agent loopback endpoint (127.0.0.1, ::1, or localhost) may be exposed."
                .to_string(),
        ),
    }
}

/// Prove a listener exists before creating any external provider resource.
/// This is intentionally a bounded TCP connect only: it validates reachability
/// without speaking HTTP, reading developer bytes, or changing server state.
pub fn probe_local_target(host: &str, port: u16) -> Result<(), String> {
    validate_local_target(host, port)?;
    let address = match host.trim() {
        "127.0.0.1" | "localhost" => SocketAddr::from(([127, 0, 0, 1], port)),
        "::1" => SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
        _ => unreachable!("validate_local_target accepted an unknown host"),
    };
    TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .map_err(|_| format!("No Agent endpoint is listening at {host}:{port}."))?;
    Ok(())
}

fn local_http_origin(host: &str, port: u16) -> String {
    let host = if host == "::1" { "[::1]" } else { host };
    format!("http://{host}:{port}")
}

/// Opaque, provider-specific handle to a live exposure. Callers persist this
/// (never the token) to later `stop`/`status` the same exposure.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TunnelHandle {
    pub provider: String,
    /// Provider-assigned identifier for the exposed resource (e.g. a
    /// Cloudflare Tunnel UUID).
    pub external_id: String,
    /// Publicly reachable hostname once the provider has routed it, if known.
    pub public_hostname: Option<String>,
    /// Connector credential minted by provider. This field is never
    /// serialized into tool results or persisted session records.
    #[serde(skip_serializing, skip_deserializing, default)]
    pub(crate) connector_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelStatus {
    Active,
    Stopped,
    Unknown,
}

/// Every concrete tunnel provider implements this trait. Implementations
/// MUST NOT log or echo the token back in any `Result::Err` string.
#[async_trait::async_trait]
pub trait TunnelProvider: Send + Sync {
    /// Stable registry key, e.g. `"cloudflare_tunnel"`.
    fn name(&self) -> &'static str;

    /// Human-readable description shown by the `tunnel` tool's
    /// `list_providers` action.
    fn description(&self) -> &'static str;

    /// Begin exposing `request.local_host:request.local_port`. Must be
    /// idempotent-safe to call again after a failed prior attempt.
    async fn start(
        &self,
        config: &TunnelProviderConfig,
        request: &TunnelStartRequest,
    ) -> Result<TunnelHandle, String>;

    /// Tear down a previously started exposure. Must succeed (no-op) if the
    /// exposure is already gone on the provider side.
    async fn stop(
        &self,
        config: &TunnelProviderConfig,
        handle: &TunnelHandle,
    ) -> Result<(), String>;

    /// Query the current state of a previously started exposure.
    async fn status(
        &self,
        config: &TunnelProviderConfig,
        handle: &TunnelHandle,
    ) -> Result<TunnelStatus, String>;
}

/// Registry of named tunnel providers. Construct with [`TunnelRegistry::new`]
/// to get the built-in Cloudflare Tunnel provider pre-registered; call
/// [`TunnelRegistry::register`] to add more (ENG-103's Tailscale-style
/// follow-on, or a test double).
pub struct TunnelRegistry {
    providers: HashMap<&'static str, Arc<dyn TunnelProvider>>,
}

impl TunnelRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            providers: HashMap::new(),
        };
        registry.register(Arc::new(CloudflareTunnelProvider));
        registry
    }

    pub fn register(&mut self, provider: Arc<dyn TunnelProvider>) {
        self.providers.insert(provider.name(), provider);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn TunnelProvider>> {
        self.providers.get(name).cloned()
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.providers.keys().copied().collect();
        names.sort_unstable();
        names
    }
}

impl Default for TunnelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Managed token configuration for one provider, scoped to one session.
/// Never persisted beyond the session; never included in tool output.
#[derive(Debug, Clone)]
pub struct TunnelProviderConfig {
    pub token: String,
    /// Provider-specific account scope (Cloudflare Tunnel needs an account id).
    pub account_id: Option<String>,
}

struct SessionExposure {
    handle: TunnelHandle,
    keep_alive: bool,
}

static SESSION_CREDENTIALS: LazyLock<
    Mutex<HashMap<String, HashMap<String, TunnelProviderConfig>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
static SESSION_EXPOSURES: LazyLock<Mutex<HashMap<String, HashMap<String, SessionExposure>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[cfg(feature = "coding_tools-host")]
#[allow(clippy::type_complexity)]
static DEV_TUNNEL_CONNECTORS: LazyLock<Mutex<HashMap<(String, String), Box<dyn ManagedChild>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Start one local cloudflared connector for a provider-minted handle. Child
/// stdio is detached so connector output cannot leak credentials into daemon
/// logs. Only Cloudflare handles contain a connector token; other providers
/// retain their own transport lifecycle.
#[cfg(feature = "coding_tools-host")]
pub(crate) async fn start_dev_tunnel_connector(
    session_id: &str,
    provider: &str,
    handle: &TunnelHandle,
    workspace_root: &std::path::Path,
) -> Result<(), String> {
    let token = handle
        .connector_token
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Tunnel provider did not return a connector credential.".to_string())?;
    let key = (session_id.to_string(), provider.to_string());
    {
        let connectors = DEV_TUNNEL_CONNECTORS
            .lock()
            .expect("dev tunnel connector lock");
        if connectors.contains_key(&key) {
            return Err("A tunnel connector is already active for this session.".to_string());
        }
    }
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("Could not resolve tunnel connector workspace: {error}"))?;
    let adapter = crate::coding_tools::shell::NativeCommandSandboxAdapter;
    let spec = CommandLaunchSpec {
        program: "cloudflared".to_string(),
        argv: vec!["tunnel".into(), "run".into()],
        cwd: workspace_root.to_string_lossy().into_owned(),
        context: ProcessLaunchContext {
            workspace_root: workspace_root.to_string_lossy().into_owned(),
            sandbox_profile: Some("workspace".to_string()),
            sandbox_required: true,
            network_required: true,
        },
        // `--token <secret>` would expose a bearer in process inspection, so
        // the token goes through cloudflared's documented environment input
        // instead. Both stdio streams stay drained below so provider
        // diagnostics cannot echo a credential into daemon output.
        env: vec![("TUNNEL_TOKEN".to_string(), token.to_string())],
    };
    let child = adapter.spawn(spec).await.map_err(|error| {
        format!(
            "Could not start cloudflared connector ({}): {error}",
            error.code()
        )
    })?;
    // The adapter owns piped streams; drain both continuously so a verbose
    // connector cannot deadlock on a full pipe, while keeping provider output
    // out of daemon logs (tokens may appear in diagnostics).
    let mut child = child;
    let _ = child.take_stdin();
    if let Some(mut stdout) = child.take_stdout() {
        tokio::spawn(async move {
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut stdout, &mut sink).await;
        });
    }
    if let Some(mut stderr) = child.take_stderr() {
        tokio::spawn(async move {
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut stderr, &mut sink).await;
        });
    }
    let duplicate_child = {
        let mut connectors = DEV_TUNNEL_CONNECTORS
            .lock()
            .expect("dev tunnel connector lock");
        if let std::collections::hash_map::Entry::Vacant(entry) = connectors.entry(key) {
            entry.insert(child);
            None
        } else {
            Some(child)
        }
    };
    if let Some(mut child) = duplicate_child {
        let _ = child.cancel().await;
        return Err("A tunnel connector is already active for this session.".to_string());
    }
    Ok(())
}

#[cfg(not(feature = "coding_tools-host"))]
pub(crate) async fn start_dev_tunnel_connector(
    _session_id: &str,
    _provider: &str,
    _handle: &TunnelHandle,
    _workspace_root: &std::path::Path,
) -> Result<(), String> {
    Err("cloudflared connector is unavailable on this execution target.".to_string())
}

#[cfg(feature = "coding_tools-host")]
pub(crate) async fn stop_dev_tunnel_connector(
    session_id: &str,
    provider: &str,
) -> Result<(), String> {
    let key = (session_id.to_string(), provider.to_string());
    let Some(mut child) = ({
        let mut connectors = DEV_TUNNEL_CONNECTORS
            .lock()
            .expect("dev tunnel connector lock");
        connectors.remove(&key)
    }) else {
        return Ok(());
    };
    child.cancel().await.map_err(|error| {
        format!(
            "Could not stop cloudflared connector ({}): {error}",
            error.code()
        )
    })?;
    child.wait().await.map_err(|error| {
        format!(
            "Could not reap cloudflared connector ({}): {error}",
            error.code()
        )
    })?;
    Ok(())
}

#[cfg(not(feature = "coding_tools-host"))]
pub(crate) async fn stop_dev_tunnel_connector(
    _session_id: &str,
    _provider: &str,
) -> Result<(), String> {
    Ok(())
}

#[cfg(feature = "coding_tools-host")]
pub(crate) fn dev_tunnel_connector_running(
    session_id: &str,
    provider: &str,
) -> Result<Option<bool>, String> {
    let key = (session_id.to_string(), provider.to_string());
    let connectors = DEV_TUNNEL_CONNECTORS
        .lock()
        .expect("dev tunnel connector lock");
    let Some(child) = connectors.get(&key) else {
        return Ok(None);
    };
    Ok(Some(child.pid().is_some()))
}

#[cfg(not(feature = "coding_tools-host"))]
pub(crate) fn dev_tunnel_connector_running(
    _session_id: &str,
    _provider: &str,
) -> Result<Option<bool>, String> {
    Ok(None)
}

/// Store (or replace) the managed token for `provider` within `session_id`.
/// Mutating callers must gate this through `PermissionEngine` first — see
/// `TunnelTool::execute`'s `configure` action.
pub fn configure_tunnel_credential(session_id: &str, provider: &str, config: TunnelProviderConfig) {
    let mut registry = SESSION_CREDENTIALS.lock().expect("tunnel credential lock");
    registry
        .entry(session_id.to_string())
        .or_default()
        .insert(provider.to_string(), config);
}

pub fn tunnel_credential(session_id: &str, provider: &str) -> Option<TunnelProviderConfig> {
    let registry = SESSION_CREDENTIALS.lock().expect("tunnel credential lock");
    registry.get(session_id)?.get(provider).cloned()
}

/// Called on session teardown, alongside `context::retire_cloudflare_credential`.
/// Stops every exposure opened by this session that was not marked
/// `keep_alive`, then drops every stored token for the session unconditionally
/// — a lingering token must never survive the session that supplied it.
pub fn retire_tunnel_session(session_id: &str) -> Vec<String> {
    retire_tunnel_session_with(session_id, &TunnelRegistry::new())
}

/// [`retire_tunnel_session`] against an explicit provider registry. Returns one
/// message per resource that could not be stopped (each is also logged), so the
/// caller can record the leaked exposure instead of assuming a clean teardown.
pub(crate) fn retire_tunnel_session_with(
    session_id: &str,
    registry: &TunnelRegistry,
) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    // Dev proxy connectors are always session-scoped; kill them before their
    // control-plane resource is retired, even if a generic exposure requested
    // keep_alive elsewhere.
    #[cfg(feature = "coding_tools-host")]
    {
        let provider_names: Vec<String> = DEV_TUNNEL_CONNECTORS
            .lock()
            .expect("dev tunnel connector lock")
            .keys()
            .filter(|(owner, _)| owner == session_id)
            .map(|(_, provider)| provider.clone())
            .collect();
        for provider in provider_names {
            if let Err(error) =
                futures::executor::block_on(stop_dev_tunnel_connector(session_id, &provider))
            {
                tracing::warn!(session_id, provider = %provider, %error, "tunnel connector stop failed during session teardown");
                failures.push(format!("{provider} connector: {error}"));
            }
        }
    }
    let exposures = {
        let mut exposures = SESSION_EXPOSURES.lock().expect("tunnel exposure lock");
        exposures.remove(session_id)
    };
    if let Some(exposures) = exposures {
        for (provider_name, exposure) in exposures {
            if exposure.keep_alive {
                continue;
            }
            if let (Some(provider), Some(config)) = (
                registry.get(&provider_name),
                tunnel_credential(session_id, &provider_name),
            ) {
                // The session is ending regardless of outcome, so a failure
                // cannot be retried here; it is logged and returned so the
                // leaked exposure is on record.
                if let Err(error) =
                    futures::executor::block_on(provider.stop(&config, &exposure.handle))
                {
                    tracing::warn!(
                        session_id,
                        provider = %provider_name,
                        external_id = %exposure.handle.external_id,
                        %error,
                        "tunnel stop failed during session teardown; the exposure may still be live"
                    );
                    failures.push(format!(
                        "{provider_name} {}: {error}",
                        exposure.handle.external_id
                    ));
                }
            }
        }
    }
    let mut credentials = SESSION_CREDENTIALS.lock().expect("tunnel credential lock");
    credentials.remove(session_id);
    failures
}

/// Roll back a provider resource after its local connector failed to start.
/// Returns the message for the caller's error result and whether the resource
/// is still exposed (the provider stop failed); in that case the failure is
/// logged and named in the message so the caller keeps the exposure tracked.
pub(crate) async fn rollback_failed_start(
    provider: &dyn TunnelProvider,
    config: &TunnelProviderConfig,
    session_id: &str,
    provider_name: &str,
    handle: &TunnelHandle,
    error: String,
) -> (String, bool) {
    match provider.stop(config, handle).await {
        Ok(()) => (error, false),
        Err(stop_error) => {
            tracing::warn!(
                session_id,
                provider = provider_name,
                external_id = %handle.external_id,
                %stop_error,
                "tunnel rollback stop failed; the exposure may still be live"
            );
            (
                format!(
                    "{error}; additionally the provider resource {} could not be stopped and may still be exposed ({stop_error}); it stays tracked so a later stop can retry",
                    handle.external_id
                ),
                true,
            )
        }
    }
}

pub(crate) fn record_exposure(
    session_id: &str,
    provider: &str,
    handle: TunnelHandle,
    keep_alive: bool,
) {
    let mut registry = SESSION_EXPOSURES.lock().expect("tunnel exposure lock");
    registry
        .entry(session_id.to_string())
        .or_default()
        .insert(provider.to_string(), SessionExposure { handle, keep_alive });
}

pub(crate) fn take_exposure(session_id: &str, provider: &str) -> Option<TunnelHandle> {
    let mut registry = SESSION_EXPOSURES.lock().expect("tunnel exposure lock");
    registry
        .get_mut(session_id)?
        .remove(provider)
        .map(|e| e.handle)
}

fn peek_exposure(session_id: &str, provider: &str) -> Option<TunnelHandle> {
    let registry = SESSION_EXPOSURES.lock().expect("tunnel exposure lock");
    registry
        .get(session_id)?
        .get(provider)
        .map(|e| e.handle.clone())
}

/// Concrete provider against the Cloudflare API v4 `cfd_tunnel` endpoints.
/// Mirrors `crate::coding_tools::cloudflare::CloudflareTool`'s request shape exactly.
pub struct CloudflareTunnelProvider;

impl CloudflareTunnelProvider {
    fn endpoint(account_id: &str, suffix: &str) -> Result<url::Url, String> {
        let mut url = url::Url::parse("https://api.cloudflare.com/client/v4/")
            .map_err(|error| format!("Could not construct Cloudflare Tunnel URL: {error}"))?;
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Cloudflare Tunnel base URL cannot carry path segments.".to_string())?;
        segments.pop_if_empty();
        segments.push("accounts");
        segments.push(account_id);
        for segment in suffix.split('/') {
            segments.push(segment);
        }
        drop(segments);
        Ok(url)
    }

    async fn request(
        method: rightkit_http::Method,
        config: &TunnelProviderConfig,
        suffix: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let account_id = config
            .account_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "cloudflare_tunnel requires account_id when configured.".to_string())?;
        let url = Self::endpoint(account_id, suffix)?;
        let client =
            crate::coding_tools::http_client::client_for(url.as_str(), Duration::from_secs(15))
                .map_err(|error| format!("Could not create Cloudflare Tunnel client: {error}"))?;
        let mut request = rightkit_http::Request::new(method, url.as_str())
            .bearer(&config.token)
            .max_response_bytes(MAX_RESPONSE_BYTES)
            .header("Accept", "application/json");
        if let Some(body) = body {
            request = request
                .json(&body)
                .map_err(|error| format!("Cloudflare Tunnel request failed: {error}"))?;
        }
        let response = client
            .send_any(&request)
            .await
            .map_err(|error| match error {
                rightkit_http::HttpError::BodyTooLarge { .. } => {
                    "Cloudflare Tunnel response exceeds 512 KiB limit.".to_string()
                }
                other => format!("Cloudflare Tunnel request failed: {other}"),
            })?;
        let status = response.status;
        let raw = String::from_utf8(response.body)
            .map_err(|_| "Cloudflare Tunnel returned invalid UTF-8.".to_string())?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "Cloudflare Tunnel returned HTTP {}: {}",
                status,
                bounded(&raw)
            ));
        }
        let value: Value = serde_json::from_str(&raw)
            .map_err(|_| "Cloudflare Tunnel returned invalid JSON.".to_string())?;
        if value.get("success").and_then(Value::as_bool) == Some(false) {
            return Err(format!(
                "Cloudflare Tunnel rejected request: {}",
                bounded(&raw)
            ));
        }
        Ok(value)
    }
}

#[async_trait::async_trait]
impl TunnelProvider for CloudflareTunnelProvider {
    fn name(&self) -> &'static str {
        "cloudflare_tunnel"
    }

    fn description(&self) -> &'static str {
        "Cloudflare Tunnel (cfd_tunnel API v4): creates a named tunnel resource and mints a connector token for a local host:port. Running the local `cloudflared` connector is handled by the dev-tunnel proxy consumer, not this provider."
    }

    async fn start(
        &self,
        config: &TunnelProviderConfig,
        request: &TunnelStartRequest,
    ) -> Result<TunnelHandle, String> {
        let label = request
            .label
            .as_deref()
            .unwrap_or("rightkit-agent-dev-tunnel");
        let created = Self::request(
            rightkit_http::Method::Post,
            config,
            "cfd_tunnel",
            Some(json!({"name": label, "config_src": "cloudflare"})),
        )
        .await?;
        let tunnel_id = created
            .pointer("/result/id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Cloudflare Tunnel create response lacked result.id.".to_string())?;
        let configuration = json!({
            "config": {
                "ingress": [
                    if let Some(hostname) = request.public_hostname.as_deref() {
                        json!({"hostname": hostname, "service": local_http_origin(&request.local_host, request.local_port)})
                    } else {
                        json!({"service": local_http_origin(&request.local_host, request.local_port)})
                    },
                    {"service": "http_status:404"}
                ]
            }
        });
        if let Err(error) = Self::request(
            rightkit_http::Method::Put,
            config,
            &format!("cfd_tunnel/{tunnel_id}/configurations"),
            Some(configuration),
        )
        .await
        {
            let _ = Self::request(
                rightkit_http::Method::Delete,
                config,
                &format!("cfd_tunnel/{tunnel_id}"),
                None,
            )
            .await;
            return Err(error);
        }
        let connector = Self::request(
            rightkit_http::Method::Get,
            config,
            &format!("cfd_tunnel/{tunnel_id}/token"),
            None,
        )
        .await;
        let connector_token = match connector {
            Ok(value) => value
                .pointer("/result")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    "Cloudflare Tunnel connector response lacked result token.".to_string()
                })?,
            Err(error) => {
                let _ = Self::request(
                    rightkit_http::Method::Delete,
                    config,
                    &format!("cfd_tunnel/{tunnel_id}"),
                    None,
                )
                .await;
                return Err(error);
            }
        };
        Ok(TunnelHandle {
            provider: self.name().to_string(),
            external_id: tunnel_id.to_string(),
            public_hostname: request.public_hostname.clone(),
            connector_token: Some(connector_token),
        })
    }

    async fn stop(
        &self,
        config: &TunnelProviderConfig,
        handle: &TunnelHandle,
    ) -> Result<(), String> {
        let response = Self::request(
            rightkit_http::Method::Delete,
            config,
            &format!("cfd_tunnel/{}", handle.external_id),
            None,
        )
        .await;
        match response {
            Ok(_) => Ok(()),
            Err(error) if error.contains("HTTP 404") => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn status(
        &self,
        config: &TunnelProviderConfig,
        handle: &TunnelHandle,
    ) -> Result<TunnelStatus, String> {
        let response = Self::request(
            rightkit_http::Method::Get,
            config,
            &format!("cfd_tunnel/{}", handle.external_id),
            None,
        )
        .await?;
        let status = response
            .pointer("/result/status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(match status.to_ascii_lowercase().as_str() {
            "healthy" | "active" | "running" => TunnelStatus::Active,
            "inactive" | "down" | "stopped" => TunnelStatus::Stopped,
            _ => TunnelStatus::Unknown,
        })
    }
}

/// The `tunnel` Tool: lists providers, configures managed tokens, and
/// starts/stops/status-checks an exposure. Every mutating action
/// (`configure`, `start`, `stop`) is gated through `PermissionEngine` on
/// `CapabilityClass::Network`; `list_providers` and `status` are read-only
/// and ungated (status makes an outbound call but discloses nothing new that
/// `start`'s response didn't already disclose to this same session).
pub struct TunnelTool {
    registry: TunnelRegistry,
}

impl TunnelTool {
    pub fn new() -> Self {
        Self {
            registry: TunnelRegistry::new(),
        }
    }
}

impl Default for TunnelTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for TunnelTool {
    fn name(&self) -> &str {
        "tunnel"
    }

    fn description(&self) -> &str {
        "Optional, guarded exposure of a local host:port over an external tunnel provider. Lists providers, stores a managed token, and starts/stops/checks a public exposure. Exposure never survives the session that opened it unless keep_alive is set."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {"type":"string", "enum":[
                    "list_providers", "configure", "start", "stop", "status"
                ]},
                "provider": {"type":"string", "description":"Registry key, e.g. cloudflare_tunnel"},
                "token": {"type":"string", "description":"Optional provider API token (configure only, never echoed back). Cloudflare uses its host-managed credential when omitted."},
                "account_id": {"type":"string", "description":"Provider account scope, e.g. Cloudflare account id (configure only)"},
                "local_host": {"type":"string", "default":"127.0.0.1"},
                "local_port": {"type":"integer"},
                "label": {"type":"string"},
                "public_hostname": {"type":"string", "description":"Already configured public DNS hostname for this tunnel"},
                "keep_alive": {"type":"boolean", "default": false, "description":"Survive session end instead of being force-stopped"}
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::Network)
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let action = match args.get("action").and_then(Value::as_str) {
            Some(value) => value,
            None => return ToolResult::error("Missing required argument: action"),
        };

        if action == "list_providers" {
            let providers: Vec<Value> = self
                .registry
                .names()
                .into_iter()
                .filter_map(|name| self.registry.get(name))
                .map(|provider| json!({"name": provider.name(), "description": provider.description()}))
                .collect();
            return ToolResult::ok(json!({"providers": providers}).to_string());
        }

        let provider_name = match required(&args, "provider") {
            Ok(value) => value.to_string(),
            Err(error) => return error,
        };
        let provider = match self.registry.get(&provider_name) {
            Some(value) => value,
            None => return ToolResult::error(format!("Unknown tunnel provider: {provider_name}")),
        };

        // Every remaining action mutates session-scoped state or reaches an
        // external network endpoint — gate uniformly, same as CloudflareTool.
        let verdict = PermissionEngine::new().evaluate(
            CapabilityClass::Network,
            ctx.permission_mode,
            ctx.workspace_trust,
            None,
            false,
        );
        match verdict.decision {
            PermissionDecision::Allow => {}
            PermissionDecision::Ask => {
                return ToolResult::error(format!("Tool requires approval: {}", verdict.reason))
            }
            PermissionDecision::Deny => {
                return ToolResult::error(format!("Permission denied: {}", verdict.reason))
            }
        }

        match action {
            "configure" => {
                // Cloudflare's normal path never moves its credential through
                // model arguments or transcript: reuse the host-owned
                // session config. Explicit tokens remain available to other
                // registry providers, but are never returned or logged.
                let managed_cloudflare = if provider_name == "cloudflare_tunnel" {
                    crate::coding_tools::context::cloudflare_credential(&ctx.session_id)
                        .or_else(|| ctx.cloudflare_config.clone())
                } else {
                    None
                };
                let token = match args.get("token").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) {
                    Some(value) => value.to_string(),
                    None => match managed_cloudflare.as_ref() {
                        Some(config) => config.api_token.clone(),
                        None => return ToolResult::error(format!(
                            "No managed credential is connected for {provider_name}; connect its account or provide token."
                        )),
                    },
                };
                let account_id = args
                    .get("account_id")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
                    .or_else(|| managed_cloudflare.map(|config| config.account_id));
                configure_tunnel_credential(
                    &ctx.session_id,
                    &provider_name,
                    TunnelProviderConfig { token, account_id },
                );
                // Never echo the token back, even truncated.
                ToolResult::ok(format!("Configured managed token for {provider_name}."))
            }
            "start" => {
                let config = match tunnel_credential(&ctx.session_id, &provider_name) {
                    Some(value) => value,
                    None => {
                        return ToolResult::error(format!(
                            "No managed token configured for {provider_name} in this session; run action=configure first."
                        ))
                    }
                };
                let local_port = match args.get("local_port").and_then(Value::as_u64) {
                    Some(value) if value > 0 && value <= u16::MAX as u64 => value as u16,
                    _ => {
                        return ToolResult::error(
                            "Missing or invalid required argument: local_port",
                        )
                    }
                };
                let local_host = args
                    .get("local_host")
                    .and_then(Value::as_str)
                    .unwrap_or("127.0.0.1")
                    .to_string();
                // Prove a local listener exists before creating any external
                // provider resource (upstream ENG-102 hardening).
                if let Err(error) = probe_local_target(&local_host, local_port) {
                    return ToolResult::error(error);
                }
                let label = args
                    .get("label")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let keep_alive = args
                    .get("keep_alive")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if keep_alive && provider_name == "cloudflare_tunnel" {
                    return ToolResult::error(
                        "cloudflare_tunnel exposures are session-bound; keep_alive is unsupported so its local connector always has a bounded owner."
                    );
                }
                let request = TunnelStartRequest {
                    local_host,
                    local_port,
                    label,
                    public_hostname: args
                        .get("public_hostname")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                };
                match provider.start(&config, &request).await {
                    Ok(handle) => {
                        // Upstream ENG-102: a cloudflare exposure is useless
                        // without its local connector, so a connector failure
                        // tears the provider resource back down instead of
                        // leaving an orphaned tunnel. The connector spawn is
                        // async & workspace-scoped here (CG-B02 governed
                        // launch), so it is awaited with the workspace root.
                        if provider_name == "cloudflare_tunnel" {
                            if let Err(error) = start_dev_tunnel_connector(
                                &ctx.session_id,
                                &provider_name,
                                &handle,
                                &ctx.workspace_root,
                            )
                            .await
                            {
                                let (message, still_exposed) = rollback_failed_start(
                                    &*provider,
                                    &config,
                                    &ctx.session_id,
                                    &provider_name,
                                    &handle,
                                    error,
                                )
                                .await;
                                if still_exposed {
                                    record_exposure(&ctx.session_id, &provider_name, handle, false);
                                }
                                return ToolResult::error(message);
                            }
                        }
                        record_exposure(
                            &ctx.session_id,
                            &provider_name,
                            handle.clone(),
                            keep_alive,
                        );
                        ToolResult::ok(
                            json!({
                                "event": "tunnel_started",
                                "provider": handle.provider,
                                "external_id": handle.external_id,
                                "public_hostname": handle.public_hostname,
                                "target": format!("{}:{}", request.local_host, request.local_port),
                                "keep_alive": keep_alive,
                            })
                            .to_string(),
                        )
                    }
                    Err(error) => ToolResult::error(error),
                }
            }
            "stop" => {
                let config = match tunnel_credential(&ctx.session_id, &provider_name) {
                    Some(value) => value,
                    None => {
                        return ToolResult::error(format!(
                            "No managed token configured for {provider_name} in this session."
                        ))
                    }
                };
                let handle = match take_exposure(&ctx.session_id, &provider_name) {
                    Some(value) => value,
                    None => {
                        return ToolResult::error(format!(
                            "No active {provider_name} exposure to stop in this session."
                        ))
                    }
                };
                if provider_name == "cloudflare_tunnel" {
                    if let Err(error) =
                        stop_dev_tunnel_connector(&ctx.session_id, &provider_name).await
                    {
                        record_exposure(&ctx.session_id, &provider_name, handle, false);
                        return ToolResult::error(error);
                    }
                }
                match provider.stop(&config, &handle).await {
                    Ok(()) => ToolResult::ok(
                        json!({
                            "event": "tunnel_stopped",
                            "provider": provider_name,
                            "status": "stopped"
                        })
                        .to_string(),
                    ),
                    Err(error) => {
                        // Put the exposure back so status/stop can be retried.
                        let mut message = error;
                        if provider_name == "cloudflare_tunnel" {
                            if let Err(restart_error) = start_dev_tunnel_connector(
                                &ctx.session_id,
                                &provider_name,
                                &handle,
                                &ctx.workspace_root,
                            )
                            .await
                            {
                                tracing::warn!(session_id = %ctx.session_id, provider = %provider_name, error = %restart_error, "tunnel connector restart failed after a failed stop");
                                message = format!(
                                    "{message}; additionally the local connector could not be restarted ({restart_error})"
                                );
                            }
                        }
                        record_exposure(&ctx.session_id, &provider_name, handle, false);
                        ToolResult::error(message)
                    }
                }
            }
            "status" => {
                let config = match tunnel_credential(&ctx.session_id, &provider_name) {
                    Some(value) => value,
                    None => {
                        return ToolResult::error(format!(
                            "No managed token configured for {provider_name} in this session."
                        ))
                    }
                };
                let handle = match peek_exposure(&ctx.session_id, &provider_name) {
                    Some(value) => value,
                    None => {
                        return ToolResult::ok(
                            json!({
                                "event": "tunnel_status",
                                "provider": provider_name,
                                "status": "stopped"
                            })
                            .to_string(),
                        )
                    }
                };
                match provider.status(&config, &handle).await {
                    Ok(status) => {
                        match dev_tunnel_connector_running(&ctx.session_id, &provider_name) {
                            Ok(connector_running) => ToolResult::ok(
                                json!({
                                    "event": "tunnel_status",
                                    "provider": provider_name,
                                    "status": status,
                                    "connector_running": connector_running
                                })
                                .to_string(),
                            ),
                            Err(error) => ToolResult::error(error),
                        }
                    }
                    Err(error) => ToolResult::error(error),
                }
            }
            other => ToolResult::error(format!("Unsupported tunnel action: {other}")),
        }
    }
}

fn required<'a>(args: &'a Value, name: &str) -> Result<&'a str, ToolResult> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ToolResult::error(format!("Missing required argument: {name}")))
}

fn bounded(value: &str) -> String {
    const LIMIT: usize = 8_192;
    let mut result = value.chars().take(LIMIT).collect::<String>();
    if value.chars().count() > LIMIT {
        result.push('…');
    }
    result
}
