mod tests {
    use super::super::*;
    use std::collections::BTreeMap;

    #[test]
    fn validation_rejects_unknown_parameter_references() {
        let recipe = Recipe {
            prompt: Some("Ship {{feature}} for {{customer}}".to_string()),
            ..sample_recipe()
        };

        let run = validate_recipe(&recipe, "rv-1", "2026-06-08T00:00:00Z");

        assert_eq!(run.verdict, RecipeVerdict::Block);
        assert!(run
            .findings
            .iter()
            .any(|finding| finding.kind == "unknown_parameter_reference"));
    }

    #[test]
    fn render_applies_defaults_and_supplied_params() {
        let recipe = sample_recipe();
        let mut params = BTreeMap::new();
        params.insert("feature".to_string(), "streaming".to_string());

        let rendered = render_recipe(&recipe, &params).unwrap();

        assert!(rendered.rendered_prompt.contains("Implement streaming"));
        assert_eq!(rendered.params["audience"], "operator");
        assert!(!rendered.rendered_prompt_hash.is_empty());
    }

    #[test]
    fn validation_rejects_self_sub_recipe() {
        let mut recipe = sample_recipe();
        recipe.id = Some("self".to_string());
        recipe.sub_recipes = vec![SubRecipeRef {
            id: "self".to_string(),
            params: BTreeMap::new(),
        }];

        let run = validate_recipe(&recipe, "rv-1", "2026-06-08T00:00:00Z");

        assert_eq!(run.verdict, RecipeVerdict::Block);
        assert!(run
            .findings
            .iter()
            .any(|finding| finding.kind == "sub_recipe_cycle"));
    }

    #[test]
    fn validation_rejects_indirect_sub_recipe_cycle() {
        let mut root = sample_recipe();
        root.id = Some("root".to_string());
        root.sub_recipes = vec![SubRecipeRef {
            id: "middle".to_string(),
            params: BTreeMap::from([("feature".to_string(), "delegation".to_string())]),
        }];
        let mut middle = sample_recipe();
        middle.id = Some("middle".to_string());
        middle.sub_recipes = vec![SubRecipeRef {
            id: "leaf".to_string(),
            params: BTreeMap::from([("feature".to_string(), "delegation".to_string())]),
        }];
        let mut leaf = sample_recipe();
        leaf.id = Some("leaf".to_string());
        leaf.sub_recipes = vec![SubRecipeRef {
            id: "root".to_string(),
            params: BTreeMap::from([("feature".to_string(), "delegation".to_string())]),
        }];
        let catalog = BTreeMap::from([
            ("root".to_string(), root.clone()),
            ("middle".to_string(), middle),
            ("leaf".to_string(), leaf),
        ]);

        let run = validate_recipe_with_catalog(&root, &catalog, "rv-1", "2026-06-08T00:00:00Z");

        assert_eq!(run.verdict, RecipeVerdict::Block);
        assert!(run.findings.iter().any(|finding| {
            finding.kind == "sub_recipe_cycle"
                && finding.message.contains("root -> middle -> leaf -> root")
        }));
    }

    #[test]
    fn validation_rejects_invalid_sub_recipe_params() {
        let mut parent = sample_recipe();
        parent.id = Some("parent".to_string());
        parent.sub_recipes = vec![SubRecipeRef {
            id: "child".to_string(),
            params: BTreeMap::from([("unknown".to_string(), "value".to_string())]),
        }];
        let mut child = sample_recipe();
        child.id = Some("child".to_string());
        let catalog = BTreeMap::from([
            ("parent".to_string(), parent.clone()),
            ("child".to_string(), child),
        ]);

        let run = validate_recipe_with_catalog(&parent, &catalog, "rv-1", "2026-06-08T00:00:00Z");

        assert_eq!(run.verdict, RecipeVerdict::Block);
        assert!(run
            .findings
            .iter()
            .any(|finding| finding.kind == "unknown_sub_recipe_parameter"));
        assert!(run
            .findings
            .iter()
            .any(|finding| finding.kind == "missing_sub_recipe_parameter"));
    }

    #[test]
    fn recipe_store_snapshot_round_trips_installed_recipe_and_runs() {
        let recipe = sample_recipe();
        let validation = validate_recipe(&recipe, "rv-1", "2026-06-08T00:00:00Z");
        let installed = InstalledRecipe {
            id: "ship-feature".to_string(),
            source: RecipeSource::Inline,
            source_uri: None,
            title: recipe.title.clone(),
            version: recipe.version.clone(),
            checksum: recipe_checksum(&recipe),
            trusted: true,
            recipe,
            installed_at: "2026-06-08T00:00:00Z".to_string(),
            latest_validation: Some(validation),
        };
        let run = RecipeRun {
            id: "rr-1".to_string(),
            recipe_id: "ship-feature".to_string(),
            session_id: "session-1".to_string(),
            status: RecipeRunStatus::Completed,
            params: BTreeMap::from([("feature".to_string(), "recipes".to_string())]),
            rendered_prompt_hash: "hash".to_string(),
            result_artifact_id: Some("artifact-1".to_string()),
            created_at: "2026-06-08T00:00:01Z".to_string(),
            completed_at: Some("2026-06-08T00:00:02Z".to_string()),
        };
        let mut store = RecipeStore::new();
        store.install(installed);
        store.add_run(run);

        let reloaded = RecipeStore::from_snapshot(store.snapshot());

        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.runs("ship-feature").len(), 1);
        assert_eq!(reloaded.get("ship-feature").unwrap().title, "Ship feature");
    }

    #[test]
    fn recipe_deeplink_round_trips_action_and_params() {
        let params = BTreeMap::from([
            ("feature".to_string(), "recipe deeplinks".to_string()),
            ("audience".to_string(), "operator".to_string()),
        ]);

        let link = build_recipe_deeplink("ship-feature", "run", &params).unwrap();
        let parsed = parse_recipe_deeplink(&link.url).unwrap();

        assert_eq!(parsed.recipe_id, "ship-feature");
        assert_eq!(parsed.action, "run");
        assert_eq!(parsed.params, params);
        assert_eq!(parsed.url, link.url);
    }

    #[test]
    fn recipe_deeplink_rejects_unsupported_actions() {
        let err = build_recipe_deeplink("ship-feature", "delete", &BTreeMap::new()).unwrap_err();

        assert!(err.message.contains("unsupported"));
    }

    fn sample_recipe() -> Recipe {
        Recipe {
            id: Some("ship-feature".to_string()),
            version: "1.0.0".to_string(),
            title: "Ship feature".to_string(),
            description: "Implement and verify a feature.".to_string(),
            instructions: Some("You are implementing for {{audience}}.".to_string()),
            prompt: Some("Implement {{feature}}.".to_string()),
            activities: Vec::new(),
            parameters: vec![
                RecipeParameter {
                    key: "feature".to_string(),
                    input_type: RecipeInputType::String,
                    requirement: RequirementLevel::Required,
                    description: "Feature name".to_string(),
                    default: None,
                    value: None,
                },
                RecipeParameter {
                    key: "audience".to_string(),
                    input_type: RecipeInputType::String,
                    requirement: RequirementLevel::Optional,
                    description: "Audience".to_string(),
                    default: Some("operator".to_string()),
                    value: None,
                },
            ],
            required_toolsets: vec!["dev-safe".to_string()],
            required_mcp_servers: Vec::new(),
            required_plugins: Vec::new(),
            skills: Vec::new(),
            settings: RecipeSettings::default(),
            response_schema: None,
            retry: None,
            sub_recipes: Vec::new(),
        }
    }
}
