pub(crate) fn permission_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "SetPermissionMode".to_string(),
        json!({
            "type": "object",
            "required": ["mode"],
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["plan", "ask", "accept-edits", "auto-safe", "bypass"]
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetPermissionModeResponse".to_string(),
        json!({
            "type": "object",
            "required": ["mode"],
            "properties": {
                "mode": {"type": "string"}
            }
        }),
    );
    schemas.insert(
        "AddPermissionRule".to_string(),
        json!({
            "type": "object",
            "required": ["pattern", "effect"],
            "properties": {
                "pattern": {"type": "string"},
                "effect": {"$ref": "#/components/schemas/PermissionRuleEffect"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "AddPersistentPermissionRule".to_string(),
        json!({
            "type": "object",
            "required": ["scope", "pattern", "effect"],
            "properties": {
                "scope": {"type": "string", "enum": ["user", "project"]},
                "pattern": {"type": "string"},
                "effect": {"$ref": "#/components/schemas/PermissionRuleEffect"},
                "workspaceRoot": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetWorkspaceTrust".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "trust"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "trust": {"type": "string", "enum": ["trusted", "untrusted", "sandboxed", "locked"]},
                "approvedBy": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "PermissionRuleEffect".to_string(),
        json!({"type": "string", "enum": ["allow", "ask", "deny"]}),
    );
    schemas.insert(
        "PermissionRuleSource".to_string(),
        json!({"type": "string", "enum": ["builtin", "project", "user", "session"]}),
    );
    schemas.insert(
        "PermissionRuleRow".to_string(),
        json!({
            "type": "object",
            "required": ["index", "pattern", "effect", "source"],
            "properties": {
                "index": {"type": "integer", "minimum": 0},
                "pattern": {"type": "string"},
                "effect": {"$ref": "#/components/schemas/PermissionRuleEffect"},
                "source": {"$ref": "#/components/schemas/PermissionRuleSource"}
            }
        }),
    );
    schemas.insert(
        "PermissionRulesResponse".to_string(),
        json!({
            "type": "object",
            "required": ["rules"],
            "properties": {
                "rules": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PermissionRuleRow"}
                }
            }
        }),
    );
    schemas.insert(
        "PermissionRuleMutationResponse".to_string(),
        json!({
            "type": "object",
            "required": ["rule"],
            "properties": {
                "rule": {"$ref": "#/components/schemas/PermissionRuleRow"}
            }
        }),
    );
    schemas.insert(
        "DeletePermissionRuleResponse".to_string(),
        json!({
            "type": "object",
            "required": ["removed", "rule"],
            "properties": {
                "removed": {"type": "boolean"},
                "rule": {"$ref": "#/components/schemas/PermissionRuleRow"}
            }
        }),
    );
    schemas.insert(
        "PersistentPermissionRule".to_string(),
        json!({
            "type": "object",
            "required": ["pattern", "effect"],
            "properties": {
                "pattern": {"type": "string"},
                "effect": {"$ref": "#/components/schemas/PermissionRuleEffect"}
            }
        }),
    );
    schemas.insert(
        "PersistentPermissionRuleRow".to_string(),
        json!({
            "type": "object",
            "required": ["index", "pattern", "effect", "scope", "workspaceRoot"],
            "properties": {
                "index": {"type": "integer", "minimum": 0},
                "pattern": {"type": "string"},
                "effect": {"$ref": "#/components/schemas/PermissionRuleEffect"},
                "scope": {"type": "string", "enum": ["user", "project"]},
                "workspaceRoot": {"type": ["string", "null"]}
            }
        }),
    );
    schemas.insert(
        "PersistentPermissionRulesResponse".to_string(),
        json!({
            "type": "object",
            "required": ["permissionRuleStorePath"],
            "properties": {
                "permissionRuleStorePath": {"type": "string"},
                "scope": {"type": "string", "enum": ["user", "project"]},
                "workspaceRoot": {"type": "string"},
                "loadError": {
                    "type": "object",
                    "required": ["code", "message", "path"],
                    "properties": {
                        "code": {"type": "string", "enum": ["permission_rules_corrupt", "permission_rules_unreadable"]},
                        "message": {"type": "string"},
                        "path": {"type": "string"}
                    }
                },
                "rules": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PersistentPermissionRuleRow"}
                },
                "user": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PersistentPermissionRuleRow"}
                },
                "projects": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "array",
                        "items": {"$ref": "#/components/schemas/PersistentPermissionRule"}
                    }
                }
            }
        }),
    );
    schemas.insert(
        "PersistentPermissionRuleMutationResponse".to_string(),
        json!({
            "type": "object",
            "required": ["rule"],
            "properties": {
                "rule": {"$ref": "#/components/schemas/PersistentPermissionRuleRow"}
            }
        }),
    );
    schemas.insert(
        "DeletePersistentPermissionRuleResponse".to_string(),
        json!({
            "type": "object",
            "required": ["removed", "rule"],
            "properties": {
                "removed": {"type": "boolean"},
                "rule": {"$ref": "#/components/schemas/PersistentPermissionRuleRow"}
            }
        }),
    );
    schemas.insert(
        "PersistentPermissionRuleStore".to_string(),
        json!({
            "type": "object",
            "required": ["user", "projects"],
            "properties": {
                "user": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PersistentPermissionRule"}
                },
                "projects": {
                    "type": "object",
                    "additionalProperties": {
                        "type": "array",
                        "items": {"$ref": "#/components/schemas/PersistentPermissionRule"}
                    }
                }
            }
        }),
    );
    schemas.insert(
        "ImportPermissionRulesResponse".to_string(),
        json!({
            "type": "object",
            "required": ["imported", "userRules", "projectScopes"],
            "properties": {
                "imported": {"type": "boolean"},
                "userRules": {"type": "integer", "minimum": 0},
                "projectScopes": {"type": "integer", "minimum": 0}
            }
        }),
    );
    schemas.insert(
        "WorkspaceTrustResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "trust", "trusted", "requiresTrust", "approvedBy", "updatedAt", "storePath"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "trust": {"type": "string", "enum": ["trusted", "untrusted", "sandboxed", "locked"]},
                "trusted": {"type": "boolean"},
                "requiresTrust": {"type": "boolean"},
                "approvedBy": {"type": ["string", "null"]},
                "updatedAt": {"type": ["string", "null"]},
                "storePath": {"type": "string"}
            }
        }),
    );
    schemas.insert(
        "PermissionDenialRow".to_string(),
        json!({
            "type": "object",
            "required": ["id", "createdAt", "sessionId", "toolCallId", "correlation", "reason"],
            "properties": {
                "id": {"type": "string"},
                "createdAt": {"type": "string"},
                "sessionId": {"type": "string"},
                "toolCallId": {"type": "string"},
                "correlation": {"type": "string"},
                "reason": {"type": "string"}
            }
        }),
    );
    schemas.insert(
        "PermissionDenialsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["permissionDenialStorePath", "denials"],
            "properties": {
                "permissionDenialStorePath": {"type": "string"},
                "denials": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/PermissionDenialRow"}
                }
            }
        }),
    );
    schemas
}

pub(crate) fn set_permission_mode_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SetPermissionMode"})
}

pub(crate) fn set_permission_mode_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/SetPermissionModeResponse"})
}

pub(crate) fn add_permission_rule_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/AddPermissionRule"})
}

pub(crate) fn permission_rules_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/PermissionRulesResponse"})
}

pub(crate) fn permission_rule_mutation_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/PermissionRuleMutationResponse"})
}

pub(crate) fn delete_permission_rule_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/DeletePermissionRuleResponse"})
}

pub(crate) fn add_persistent_permission_rule_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/AddPersistentPermissionRule"})
}

pub(crate) fn persistent_permission_rules_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/PersistentPermissionRulesResponse"})
}

pub(crate) fn persistent_permission_rule_mutation_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/PersistentPermissionRuleMutationResponse"})
}

pub(crate) fn delete_persistent_permission_rule_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/DeletePersistentPermissionRuleResponse"})
}

pub(crate) fn permission_rule_store_schema() -> Value {
    json!({"$ref": "#/components/schemas/PersistentPermissionRuleStore"})
}

pub(crate) fn import_permission_rules_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ImportPermissionRulesResponse"})
}

pub(crate) fn workspace_trust_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/SetWorkspaceTrust"})
}

pub(crate) fn workspace_trust_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/WorkspaceTrustResponse"})
}

pub(crate) fn permission_denials_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/PermissionDenialsResponse"})
}
