use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};

use crate::api::{now_iso, now_millis, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;
#[cfg(test)]
pub(crate) use rightkit_agent_engine_api_integrations::plugin_manifest::plugin_manifest_json;
pub(crate) use rightkit_agent_engine_api_integrations::plugin_manifest::{
    default_plugin_dir, default_plugin_marketplace_path, plugin_manifest_json_with_provenance,
};
