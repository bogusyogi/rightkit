use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post, put},
    Router,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path as FsPath, PathBuf},
    time::UNIX_EPOCH,
};

use crate::api::{lock_recover, now_iso, DaemonState, WorkspaceQuery};
use crate::api_permissions::enforce_trusted_source_authority;
use crate::api_toolsets::toolset_profiles_for_config;
use serde_json::{json, Map, Value};

pub fn settings_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/settings/orchestration",
            get(get_settings).put(save_settings),
        )
        .route("/settings/toolsets", get(list_settings_toolsets))
        .route(
            "/settings/toolsets/{id}",
            put(save_settings_toolset).delete(delete_settings_toolset),
        )
        .route("/settings/config-status", get(get_config_status))
        .route("/settings/reload", post(reload_settings))
        .route(
            "/provider-keys/{provider}",
            get(get_provider_key_status)
                .put(save_provider_key)
                .delete(delete_provider_key),
        )
        .route("/provider-keys/{provider}/test", post(test_provider_key))
        .route("/inference/connections", get(list_inference_connections))
        .route(
            "/inference/connections/direct",
            post(create_direct_connection),
        )
        .route(
            "/inference/connections/direct/{id}",
            put(update_direct_connection).delete(delete_direct_connection),
        )
        .route(
            "/inference/connections/direct/{id}/secret",
            put(put_direct_connection_secret).delete(delete_direct_connection_secret),
        )
        .route(
            "/inference/connections/{authority}/{id}/probe",
            post(probe_inference_connection),
        )
        .route("/inference/catalog", get(get_inference_catalog))
        .route("/inference/load", get(get_inference_load))
}

include!("api_settings/openapi.rs");

include!("api_settings/types.rs");

include!("api_settings/handlers.rs");

include!("api_settings/provider_keys.rs");

include!("api_settings/toolsets.rs");

include!("api_settings/config_files.rs");

include!("api_settings/tests.rs");
