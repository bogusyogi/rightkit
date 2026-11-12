fn api_contract_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "ApiEndpoint".to_string(),
        json!({
            "type": "object",
            "required": ["method", "path", "summary"],
            "properties": {
                "method": {"type": "string"},
                "path": {"type": "string"},
                "summary": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ApiContractResponse".to_string(),
        json!({
            "type": "object",
            "required": ["name", "version", "endpoints"],
            "properties": {
                "name": {"type": "string", "const": crate::host_identity::prefixed("daemon")},
                "version": {"type": "integer"},
                "endpoints": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ApiEndpoint"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert("OpenApiDocument".to_string(), json!({"type": "object"}));
    schemas
}

fn api_contract_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ApiContractResponse"})
}

fn openapi_document_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/OpenApiDocument"})
}
