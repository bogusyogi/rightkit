use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `daemon-api` v1. A machine-readable description of the daemon's routes,
/// authorisation classes, resumable upload and compare-and-swap mutations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DaemonApi {
    pub contract: String,
    pub contract_version: String,
    pub api_id: String,
    pub transport: Transport,
    pub auth_classes: AuthClasses,
    pub routes: Vec<Route>,
    pub upload: Upload,
    pub mutations: Mutations,
    pub errors: Vec<ErrorEntry>,
    pub lifecycle_events: Vec<String>,
    #[serde(default)]
    pub exchanges: Vec<Exchange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Transport {
    pub scheme: String,
    pub host: String,
    pub tls: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct AuthClasses {
    pub device_authenticated_mutation: AuthClass,
    pub oauth_callback: AuthClass,
    pub signed_webhook: AuthClass,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthClass {
    pub credentials: Vec<String>,
    pub mutation_allowed: bool,
    pub required_headers: Vec<String>,
    pub summary: String,
}

/// The three authorisation classes a route can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthClassId {
    DeviceAuthenticatedMutation,
    OauthCallback,
    SignedWebhook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Method {
    #[serde(rename = "GET")]
    Get,
    #[serde(rename = "POST")]
    Post,
    #[serde(rename = "PUT")]
    Put,
    #[serde(rename = "PATCH")]
    Patch,
    #[serde(rename = "DELETE")]
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Venture,
    Channel,
    Account,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Route {
    pub route_id: String,
    pub method: Method,
    pub path: String,
    pub auth_class: AuthClassId,
    pub mutation: bool,
    pub cas_required: bool,
    pub scopes: Vec<Scope>,
    pub responses: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Upload {
    pub protocol: String,
    pub digest: Digest,
    pub chunk_bytes: ChunkBytes,
    pub max_object_bytes: u64,
    pub session_ttl_seconds: u64,
    pub durable_ack_required: bool,
    pub routes: UploadRoutes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Digest {
    pub algorithm: String,
    pub mode: String,
    pub key_scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkBytes {
    pub min: u64,
    pub max: u64,
}

/// Route ids for the four upload steps. Each must name a declared route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRoutes {
    pub init: String,
    pub chunk: String,
    pub status: String,
    pub complete: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Mutations {
    pub protocol: String,
    pub precondition: Precondition,
    pub conflict: Conflict,
    pub idempotency: Idempotency,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Precondition {
    pub header: String,
    pub revision_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Conflict {
    pub status: u16,
    pub returns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Idempotency {
    pub header: String,
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorEntry {
    pub code: String,
    pub status: u16,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Exchange {
    pub route_id: String,
    pub request: ExchangeRequest,
    pub response: ExchangeResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeRequest {
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeResponse {
    pub status: u16,
    #[serde(default)]
    pub body: Option<Value>,
}
