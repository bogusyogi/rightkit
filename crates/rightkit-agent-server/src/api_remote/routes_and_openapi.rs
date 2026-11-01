use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;
use rightkit_bot_messaging::RemoteChannelAdapter;

pub fn remote_routes() -> Router<DaemonState> {
    Router::new()
        .route("/remote/channels", get(get_remote_channels_handler))
        .route("/remote/telegram/status", get(get_telegram_status_handler))
        .route("/remote/telegram/test", get(get_telegram_test_handler))
        .route("/remote/telegram/users", get(get_telegram_users_handler))
        .route("/remote/bindings", post(post_remote_binding_handler))
        .route(
            "/remote/channels/{id}/fanout",
            post(post_remote_channel_fanout_handler),
        )
        .route(
            "/remote/channels/{id}/outbound",
            post(post_remote_channel_outbound_handler),
        )
        .route(
            "/remote/channels/{id}/profile",
            post(post_remote_channel_profile_handler),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct TelegramUsersQuery {
    limit: Option<u32>,
}

pub(crate) fn remote_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "RemoteChannelsSnapshot".to_string(),
        json!({"type": "object"}),
    );
    schemas.insert(
        "TelegramRuntimeStatus".to_string(),
        json!({
            "type": ["object", "null"],
            "properties": {
                "startedAt": {"type": ["string", "null"]},
                "lastPollAt": {"type": ["string", "null"]},
                "lastSuccessAt": {"type": ["string", "null"]},
                "lastErrorAt": {"type": ["string", "null"]},
                "lastError": {"type": ["string", "null"]},
                "consecutiveFailures": {"type": "integer", "minimum": 0},
                "nextRetryMs": {"type": ["integer", "null"], "minimum": 0},
                "updateOffset": {"type": ["integer", "null"]},
                "activeChatCount": {"type": "integer", "minimum": 0}
            }
        }),
    );
    schemas.insert(
        "TelegramStatusResponse".to_string(),
        json!({
            "type": "object",
            "required": [
                "state",
                "tokenConfigured",
                "allowedUserCount",
                "longPollRequested",
                "bindingStorePath",
                "bindingStoreReadable",
                "bindingCount",
                "bindingStoreError",
                "runtime",
                "notes"
            ],
            "properties": {
                "state": {"type": "string"},
                "tokenConfigured": {"type": "boolean"},
                "allowedUserCount": {"type": "integer", "minimum": 0},
                "longPollRequested": {"type": "boolean"},
                "bindingStorePath": {"type": "string"},
                "bindingStoreReadable": {"type": "boolean"},
                "bindingCount": {"type": "integer", "minimum": 0},
                "bindingStoreError": {},
                "runtime": {"$ref": "#/components/schemas/TelegramRuntimeStatus"},
                "notes": {"type": "array", "items": {"type": "string"}}
            }
        }),
    );
    schemas.insert(
        "TelegramReadinessCheck".to_string(),
        json!({
            "type": "object",
            "required": ["id", "label", "status", "detail", "fix"],
            "properties": {
                "id": {"type": "string"},
                "label": {"type": "string"},
                "status": {"type": "string", "enum": ["pass", "warn", "block"]},
                "detail": {"type": "string"},
                "fix": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "TelegramTestResponse".to_string(),
        json!({
            "type": "object",
            "required": [
                "verdict",
                "state",
                "tokenConfigured",
                "allowedUserCount",
                "bindingStoreReadable",
                "bindingCount",
                "runtime",
                "checks",
                "notes"
            ],
            "properties": {
                "verdict": {"type": "string", "enum": ["pass", "warn", "block"]},
                "state": {"type": "string"},
                "tokenConfigured": {"type": "boolean"},
                "allowedUserCount": {"type": "integer", "minimum": 0},
                "bindingStoreReadable": {"type": "boolean"},
                "bindingCount": {"type": "integer", "minimum": 0},
                "runtime": {"$ref": "#/components/schemas/TelegramRuntimeStatus"},
                "checks": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/TelegramReadinessCheck"}
                },
                "notes": {"type": "array", "items": {"type": "string"}}
            }
        }),
    );
    schemas.insert(
        "TelegramDiscoveredUser".to_string(),
        json!({
            "type": "object",
            "required": ["userId", "lastUpdateId"],
            "properties": {
                "userId": {"type": "integer", "format": "int64"},
                "username": {"type": ["string", "null"]},
                "firstName": {"type": ["string", "null"]},
                "lastName": {"type": ["string", "null"]},
                "chatId": {"type": ["integer", "null"], "format": "int64"},
                "chatType": {"type": ["string", "null"]},
                "lastUpdateId": {"type": "integer", "format": "int64"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "TelegramUsersResponse".to_string(),
        json!({
            "type": "object",
            "required": ["verdict", "telegramTokenConfigured", "users", "userCount"],
            "properties": {
                "verdict": {"type": "string", "enum": ["pass", "warn", "block"]},
                "telegramTokenConfigured": {"type": "boolean"},
                "detail": {"type": ["string", "null"]},
                "error": {"type": ["string", "null"]},
                "userCount": {"type": "integer", "minimum": 0},
                "users": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/TelegramDiscoveredUser"}
                },
                "notes": {"type": "array", "items": {"type": "string"}}
            }
        }),
    );
    schemas.insert(
        "RemoteBindingRequest".to_string(),
        json!({
            "type": "object",
            "required": ["channelId", "targetKey", "sessionId"],
            "properties": {
                "channelId": {"type": "string"},
                "targetKey": {"type": "string"},
                "sessionId": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RemoteBindingResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok"],
            "properties": {
                "ok": {"type": "boolean"},
                "error": {"type": ["string", "null"]}
            }
        }),
    );
    schemas.insert(
        "RemoteChannelProfileRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "profileId": {"type": ["string", "null"]},
                "toolset": {"type": ["string", "null"]},
                "deliveryPolicy": {"type": ["string", "null"]},
                "trust": {"type": ["string", "null"]},
                "enabled": {"type": ["boolean", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RemoteChannelProfileResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok"],
            "properties": {
                "ok": {"type": "boolean"},
                "channel": {"type": ["object", "null"]},
                "error": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RemoteChannelFanoutRequest".to_string(),
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {"type": "string"},
                "source": {"type": ["string", "null"]},
                "targetKey": {"type": ["string", "null"]},
                "sessionId": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RemoteChannelFanoutResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok", "channelId", "attempted", "delivered", "failed", "results"],
            "properties": {
                "ok": {"type": "boolean"},
                "channelId": {"type": "string"},
                "attempted": {"type": "integer", "minimum": 0},
                "delivered": {"type": "integer", "minimum": 0},
                "failed": {"type": "integer", "minimum": 0},
                "results": {"type": "array", "items": {"type": "object"}},
                "error": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RemoteChannelOutboundTarget".to_string(),
        json!({
            "type": "object",
            "required": ["target"],
            "properties": {
                "target": {"type": "string"},
                "allowPrivateNetwork": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RemoteChannelOutboundRequest".to_string(),
        json!({
            "type": "object",
            "required": ["text", "targets"],
            "properties": {
                "text": {"type": "string"},
                "source": {"type": ["string", "null"]},
                "deliveryKind": {"type": ["string", "null"]},
                "sessionId": {"type": ["string", "null"]},
                "targets": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/RemoteChannelOutboundTarget"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RemoteChannelOutboundResponse".to_string(),
        json!({
            "type": "object",
            "required": ["ok", "channelId", "attempted", "delivered", "failed", "results"],
            "properties": {
                "ok": {"type": "boolean"},
                "channelId": {"type": "string"},
                "attempted": {"type": "integer", "minimum": 0},
                "delivered": {"type": "integer", "minimum": 0},
                "failed": {"type": "integer", "minimum": 0},
                "results": {"type": "array", "items": {"type": "object"}},
                "error": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn remote_channels_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteChannelsSnapshot"})
}

pub(crate) fn telegram_status_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/TelegramStatusResponse"})
}

pub(crate) fn telegram_test_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/TelegramTestResponse"})
}

pub(crate) fn telegram_users_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/TelegramUsersResponse"})
}

pub(crate) fn remote_binding_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteBindingResponse"})
}

pub(crate) fn remote_binding_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteBindingRequest"})
}

pub(crate) fn remote_channel_profile_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteChannelProfileRequest"})
}

pub(crate) fn remote_channel_profile_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteChannelProfileResponse"})
}

pub(crate) fn remote_channel_fanout_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteChannelFanoutRequest"})
}

pub(crate) fn remote_channel_fanout_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteChannelFanoutResponse"})
}

pub(crate) fn remote_channel_outbound_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteChannelOutboundRequest"})
}

pub(crate) fn remote_channel_outbound_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RemoteChannelOutboundResponse"})
}
