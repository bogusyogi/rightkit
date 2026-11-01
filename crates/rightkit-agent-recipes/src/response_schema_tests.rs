mod tests {
    use super::super::*;
    use serde_json::json;

    fn recipe_with_schema(schema: Option<serde_json::Value>) -> Recipe {
        Recipe {
            id: Some("with-schema".to_string()),
            version: "1.0.0".to_string(),
            title: "With schema".to_string(),
            description: "A recipe with a response schema.".to_string(),
            instructions: None,
            prompt: Some("Return JSON.".to_string()),
            activities: Vec::new(),
            parameters: Vec::new(),
            required_toolsets: Vec::new(),
            required_mcp_servers: Vec::new(),
            required_plugins: Vec::new(),
            skills: Vec::new(),
            settings: RecipeSettings::default(),
            response_schema: schema,
            retry: None,
            sub_recipes: Vec::new(),
        }
    }

    #[test]
    fn validate_recipe_run_output_passes_conforming_output() {
        let schema = json!({
            "type": "object",
            "required": ["status"],
            "properties": { "status": { "type": "string" } }
        });
        let recipe = recipe_with_schema(Some(schema));

        let result = validate_recipe_run_output(&recipe, r#"{"status":"ok"}"#);

        assert!(result.is_ok());
    }

    #[test]
    fn validate_recipe_run_output_fails_non_conforming_output() {
        let schema = json!({
            "type": "object",
            "required": ["status"],
            "properties": { "status": { "type": "string" } }
        });
        let recipe = recipe_with_schema(Some(schema));

        let err = validate_recipe_run_output(&recipe, r#"{"other":"value"}"#).unwrap_err();

        assert!(err.message.contains("response_schema validation"));
    }

    #[test]
    fn validate_recipe_run_output_fails_non_json_output() {
        let schema = json!({ "type": "object" });
        let recipe = recipe_with_schema(Some(schema));

        let err = validate_recipe_run_output(&recipe, "not json").unwrap_err();

        assert!(err.message.contains("not valid JSON"));
    }

    #[test]
    fn validate_recipe_run_output_passes_when_no_schema_declared() {
        let recipe = recipe_with_schema(None);

        let result = validate_recipe_run_output(&recipe, "anything at all, not even json");

        assert!(result.is_ok());
    }

    #[test]
    fn compile_response_schema_rejects_non_object_schema() {
        let recipe = recipe_with_schema(Some(json!("not-an-object")));

        let err = compile_response_schema(&recipe).unwrap_err();

        assert!(err.message.contains("must be a JSON object"));
    }

    #[test]
    fn compile_response_schema_returns_none_when_absent() {
        let recipe = recipe_with_schema(None);

        let compiled = compile_response_schema(&recipe).unwrap();

        assert!(compiled.is_none());
    }
}
