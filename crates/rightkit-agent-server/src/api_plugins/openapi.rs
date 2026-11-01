pub(crate) fn plugin_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "PluginListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["pluginDir", "plugins", "extension_execution"],
            "properties": {
                "pluginDir": {"type": "string"},
                "plugins": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PluginManifest"}
                },
                "extension_execution": {"$ref": "#/components/schemas/ExtensionExecutionTrustCatalog"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ExtensionExecutionTrustCatalog".to_string(),
        json!({
            "type": "array",
            "minItems": 4,
            "items": {
                "type": "object",
                "required": [
                    "class",
                    "arbitrary_code",
                    "process_authority",
                    "registration_is_sandbox",
                    "isolation",
                    "available",
                    "detail"
                ],
                "properties": {
                    "class": {
                        "type": "string",
                        "enum": [
                            "declarative_built_in",
                            "trusted_native_in_process",
                            "external_process_mcp",
                            "future_isolated"
                        ]
                    },
                    "arbitrary_code": {"type": "boolean"},
                    "process_authority": {"type": "boolean"},
                    "registration_is_sandbox": {"type": "boolean"},
                    "isolation": {
                        "type": "string",
                        "enum": [
                            "not_applicable",
                            "none_in_process",
                            "process_policy_dependent",
                            "unavailable"
                        ]
                    },
                    "available": {"type": "boolean"},
                    "detail": {"type": "string"}
                },
                "additionalProperties": false
            }
        }),
    );
    schemas.insert(
        "PluginManifest".to_string(),
        json!({
            "type": "object",
            "required": [
                "id",
                "name",
                "version",
                "description",
                "entry",
                "providesTools",
                "providesCommands",
                "tools",
                "commands",
                "prompts",
                "middleware",
                "sandbox",
                "enabled"
            ],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "version": {"type": "string"},
                "description": {"type": "string"},
                "entry": {"type": "string"},
                "providesTools": {"type": "boolean"},
                "providesCommands": {"type": "boolean"},
                "tools": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PluginToolContribution"}
                },
                "commands": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PluginCommandContribution"}
                },
                "prompts": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PluginPromptContribution"}
                },
                "middleware": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PluginMiddlewareContribution"}
                },
                "sandbox": {"$ref": "#/components/schemas/PluginSandbox"},
                "enabled": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PluginToolContribution".to_string(),
        json!({
            "type": "object",
            "required": ["name", "description", "inputSchema"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "inputSchema": {"type": "object"},
                "mutating": {"type": "boolean"},
                "timeoutMs": {"type": "integer", "minimum": 0}
            }
        }),
    );
    schemas.insert(
        "PluginCommandContribution".to_string(),
        json!({
            "type": "object",
            "required": ["name", "description"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "argsSchema": {"type": "object"}
            }
        }),
    );
    schemas.insert(
        "PluginPromptContribution".to_string(),
        json!({
            "type": "object",
            "required": ["name", "description"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "template": {"type": "string"}
            }
        }),
    );
    schemas.insert(
        "PluginMiddlewareContribution".to_string(),
        json!({
            "type": "object",
            "required": ["id", "phase", "effect"],
            "properties": {
                "id": {"type": "string"},
                "title": {"type": "string"},
                "phase": {"type": "string"},
                "effect": {"type": "string"},
                "tool": {"type": "string"},
                "requiresToolset": {"type": "string"},
                "condition": {"type": "object"},
                "mutateArgs": {"type": "object"},
                "reason": {"type": "string"},
                "enabled": {"type": "boolean"},
                "order": {"type": "integer"},
                "description": {"type": "string"}
            }
        }),
    );
    schemas.insert(
        "PluginSandbox".to_string(),
        json!({
            "type": "object",
            "properties": {
                "isolation": {
                    "type": "string",
                    "enum": ["trustedSubprocess", "restrictedSubprocess", "metadataOnly"]
                },
                "allowNetwork": {"type": "boolean"},
                "inheritEnv": {"type": "boolean"},
                "allowEnv": {"type": "array", "items": {"type": "string"}},
                "denyEnv": {"type": "array", "items": {"type": "string"}},
                "timeoutMs": {"type": "integer", "minimum": 0}
            }
        }),
    );
    schemas.insert(
        "PluginReloadResponse".to_string(),
        json!({
            "type": "object",
            "required": ["reloaded", "pluginDir", "count", "plugins", "extension_execution"],
            "properties": {
                "reloaded": {"type": "boolean"},
                "pluginDir": {"type": "string"},
                "count": {"type": "integer"},
                "middlewareCount": {"type": "integer"},
                "plugins": {"type": "array", "items": {"$ref": "#/components/schemas/PluginManifest"}},
                "extension_execution": {"$ref": "#/components/schemas/ExtensionExecutionTrustCatalog"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PluginInstallRequest".to_string(),
        json!({
            "type": "object",
            "required": ["sourcePath", "trustAccepted"],
            "properties": {
                "sourcePath": {"type": "string"},
                "trustAccepted": {"type": "boolean"},
                "overwrite": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PluginInstallResponse".to_string(),
        json!({
            "type": "object",
            "required": ["installed", "overwritten", "pluginDir", "installedPath", "plugin"],
            "properties": {
                "installed": {"type": "boolean"},
                "overwritten": {"type": "boolean"},
                "pluginDir": {"type": "string"},
                "installedPath": {"type": "string"},
                "marketplacePath": {"type": "string"},
                "sourceUrl": {"type": "string"},
                "checksumSha256": {"type": "string"},
                "packageSha256": {"type": "string"},
                "signedBy": {"type": "string"},
                "signingPublicKey": {"type": "string"},
                "signature": {"type": "string"},
                "middlewareCount": {"type": "integer"},
                "plugin": {"$ref": "#/components/schemas/PluginManifest"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PluginMarketplaceEntry".to_string(),
        json!({
            "allOf": [
                {"$ref": "#/components/schemas/PluginManifest"},
                {
                    "type": "object",
                    "required": ["downloads", "rating", "sourceUrl", "sourcePath", "checksumSha256", "signedBy"],
                    "properties": {
                        "downloads": {"type": "integer"},
                        "rating": {"type": "number"},
                        "sourceUrl": {"type": "string"},
                        "sourcePath": {"type": "string"},
                        "checksumSha256": {"type": "string"},
                        "packageSha256": {"type": "string"},
                        "signedBy": {"type": "string"},
                        "signingPublicKey": {"type": "string"},
                        "signature": {"type": "string"}
                    },
                    "additionalProperties": false
                }
            ]
        }),
    );
    schemas.insert(
        "PluginMarketplaceListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["marketplacePath", "plugins", "categories"],
            "properties": {
                "marketplacePath": {"type": "string"},
                "plugins": {"type": "array", "items": {"$ref": "#/components/schemas/PluginMarketplaceEntry"}},
                "categories": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["category", "count"],
                        "properties": {
                            "category": {"type": "string"},
                            "count": {"type": "integer", "minimum": 0}
                        },
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SyncPluginMarketplaceRequest".to_string(),
        json!({
            "type": "object",
            "required": ["catalogUrl", "expectedSha256", "trustAccepted"],
            "properties": {
                "catalogUrl": {"type": "string"},
                "expectedSha256": {"type": "string"},
                "trustAccepted": {"type": "boolean"},
                "requireSigned": {"type": "boolean"},
                "allowFileUrls": {"type": "boolean"},
                "fileUrlTrustAccepted": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PluginMarketplaceSyncResponse".to_string(),
        json!({
            "type": "object",
            "required": ["synced", "marketplacePath", "catalogUrl", "checksumSha256", "pluginCount", "requireSigned"],
            "properties": {
                "synced": {"type": "boolean"},
                "marketplacePath": {"type": "string"},
                "catalogUrl": {"type": "string"},
                "checksumSha256": {"type": "string"},
                "pluginCount": {"type": "integer"},
                "requireSigned": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "InstallMarketplacePluginRequest".to_string(),
        json!({
            "type": "object",
            "required": ["trustAccepted"],
            "properties": {
                "trustAccepted": {"type": "boolean"},
                "expectedSha256": {"type": "string"},
                "overwrite": {"type": "boolean"},
                "allowFileUrls": {"type": "boolean"},
                "fileUrlTrustAccepted": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PluginEnabledResponse".to_string(),
        json!({
            "type": "object",
            "required": ["id", "enabled"],
            "properties": {
                "id": {"type": "string"},
                "enabled": {"type": "boolean"},
                "middlewareCount": {"type": "integer"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

fn plugin_schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

pub(crate) fn plugin_list_response_schema() -> Value {
    plugin_schema_ref("PluginListResponse")
}

pub(crate) fn plugins_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/plugins/install") => Some(plugin_schema_ref("PluginInstallRequest")),
        ("post", "/plugins/marketplace/sync") => {
            Some(plugin_schema_ref("SyncPluginMarketplaceRequest"))
        }
        ("post", "/plugins/marketplace/{id}/install") => {
            Some(plugin_schema_ref("InstallMarketplacePluginRequest"))
        }
        _ => None,
    }
}

pub(crate) fn plugins_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/plugins") => Some(plugin_list_response_schema()),
        ("post", "/plugins/reload") => Some(plugin_schema_ref("PluginReloadResponse")),
        ("post", "/plugins/install") => Some(plugin_schema_ref("PluginInstallResponse")),
        ("get", "/plugins/marketplace") => Some(plugin_schema_ref("PluginMarketplaceListResponse")),
        ("post", "/plugins/marketplace/sync") => {
            Some(plugin_schema_ref("PluginMarketplaceSyncResponse"))
        }
        ("post", "/plugins/marketplace/{id}/install") => {
            Some(plugin_schema_ref("PluginInstallResponse"))
        }
        ("post", "/plugins/{id}/enable") | ("post", "/plugins/{id}/disable") => {
            Some(plugin_schema_ref("PluginEnabledResponse"))
        }
        _ => None,
    }
}
