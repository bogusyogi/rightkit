mod tests {
    use super::super::*;
    use std::collections::BTreeMap;

    fn leaf_recipe(id: &str) -> Recipe {
        Recipe {
            id: Some(id.to_string()),
            version: "1.0.0".to_string(),
            title: format!("Recipe {id}"),
            description: "A recipe.".to_string(),
            instructions: None,
            prompt: Some("Do {{task}}.".to_string()),
            activities: Vec::new(),
            parameters: vec![RecipeParameter {
                key: "task".to_string(),
                input_type: RecipeInputType::String,
                requirement: RequirementLevel::Required,
                description: "Task".to_string(),
                default: None,
                value: None,
            }],
            required_toolsets: Vec::new(),
            required_mcp_servers: Vec::new(),
            required_plugins: Vec::new(),
            skills: Vec::new(),
            settings: RecipeSettings::default(),
            response_schema: None,
            retry: None,
            sub_recipes: Vec::new(),
        }
    }

    #[test]
    fn resolve_run_plan_threads_params_and_orders_child_before_parent() {
        let child = leaf_recipe("child");
        let mut parent = leaf_recipe("parent");
        parent.sub_recipes = vec![SubRecipeRef {
            id: "child".to_string(),
            params: BTreeMap::from([("task".to_string(), "child-task".to_string())]),
        }];
        let catalog = BTreeMap::from([("child".to_string(), child)]);
        let params = BTreeMap::from([("task".to_string(), "parent-task".to_string())]);

        let plan = resolve_run_plan(&parent, &params, &catalog).unwrap();

        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].recipe_id, "child");
        assert_eq!(plan.steps[0].rendered.rendered_prompt, "Do child-task.");
        assert_eq!(plan.steps[1].recipe_id, "parent");
        assert_eq!(plan.steps[1].rendered.rendered_prompt, "Do parent-task.");
        assert_eq!(plan.root_step().unwrap().recipe_id, "parent");
    }

    #[test]
    fn resolve_run_plan_rejects_self_reference() {
        let mut recipe = leaf_recipe("self");
        recipe.sub_recipes = vec![SubRecipeRef {
            id: "self".to_string(),
            params: BTreeMap::from([("task".to_string(), "x".to_string())]),
        }];
        let catalog = BTreeMap::from([("self".to_string(), recipe.clone())]);
        let params = BTreeMap::from([("task".to_string(), "x".to_string())]);

        let err = resolve_run_plan(&recipe, &params, &catalog).unwrap_err();

        assert!(err.message.contains("cycles"));
    }

    #[test]
    fn resolve_run_plan_rejects_indirect_cycle() {
        let mut root = leaf_recipe("root");
        root.sub_recipes = vec![SubRecipeRef {
            id: "middle".to_string(),
            params: BTreeMap::from([("task".to_string(), "m".to_string())]),
        }];
        let mut middle = leaf_recipe("middle");
        middle.sub_recipes = vec![SubRecipeRef {
            id: "root".to_string(),
            params: BTreeMap::from([("task".to_string(), "r".to_string())]),
        }];
        let catalog = BTreeMap::from([
            ("root".to_string(), root.clone()),
            ("middle".to_string(), middle),
        ]);
        let params = BTreeMap::from([("task".to_string(), "r".to_string())]);

        let err = resolve_run_plan(&root, &params, &catalog).unwrap_err();

        assert!(err.message.contains("cycles"));
        assert!(err.message.contains("root"));
        assert!(err.message.contains("middle"));
    }

    #[test]
    fn resolve_run_plan_rejects_unknown_sub_recipe() {
        let mut root = leaf_recipe("root");
        root.sub_recipes = vec![SubRecipeRef {
            id: "missing".to_string(),
            params: BTreeMap::new(),
        }];
        let catalog: BTreeMap<String, Recipe> = BTreeMap::new();
        let params = BTreeMap::from([("task".to_string(), "x".to_string())]);

        let err = resolve_run_plan(&root, &params, &catalog).unwrap_err();

        assert!(err.message.contains("not present in the recipe catalog"));
    }

    #[test]
    fn resolve_run_plan_rejects_depth_beyond_bound() {
        // Build a chain longer than MAX_SUB_RECIPE_DEPTH with no cycle —
        // depth bound must trip before any cycle check would.
        let depth = MAX_SUB_RECIPE_DEPTH + 3;
        let mut catalog: BTreeMap<String, Recipe> = BTreeMap::new();
        for i in 0..depth {
            let id = format!("r{i}");
            let mut recipe = leaf_recipe(&id);
            if i > 0 {
                let child_id = format!("r{}", i - 1);
                recipe.sub_recipes = vec![SubRecipeRef {
                    id: child_id,
                    params: BTreeMap::from([("task".to_string(), "x".to_string())]),
                }];
            }
            catalog.insert(id, recipe);
        }
        let root_id = format!("r{}", depth - 1);
        let root = catalog.get(&root_id).unwrap().clone();
        let params = BTreeMap::from([("task".to_string(), "x".to_string())]);

        let err = resolve_run_plan(&root, &params, &catalog).unwrap_err();

        assert!(err.message.contains("max depth"));
    }

    #[test]
    fn resolve_run_plan_handles_diamond_without_false_cycle() {
        // root -> a -> leaf, root -> b -> leaf : shared leaf is not a cycle.
        let leaf = leaf_recipe("leaf");
        let mut a = leaf_recipe("a");
        a.sub_recipes = vec![SubRecipeRef {
            id: "leaf".to_string(),
            params: BTreeMap::from([("task".to_string(), "leaf-a".to_string())]),
        }];
        let mut b = leaf_recipe("b");
        b.sub_recipes = vec![SubRecipeRef {
            id: "leaf".to_string(),
            params: BTreeMap::from([("task".to_string(), "leaf-b".to_string())]),
        }];
        let mut root = leaf_recipe("root");
        root.sub_recipes = vec![
            SubRecipeRef {
                id: "a".to_string(),
                params: BTreeMap::from([("task".to_string(), "a".to_string())]),
            },
            SubRecipeRef {
                id: "b".to_string(),
                params: BTreeMap::from([("task".to_string(), "b".to_string())]),
            },
        ];
        let catalog = BTreeMap::from([
            ("leaf".to_string(), leaf),
            ("a".to_string(), a),
            ("b".to_string(), b),
        ]);
        let params = BTreeMap::from([("task".to_string(), "root".to_string())]);

        let plan = resolve_run_plan(&root, &params, &catalog).unwrap();

        // leaf (via a), a, leaf (via b), b, root
        assert_eq!(plan.steps.len(), 5);
        assert_eq!(plan.root_step().unwrap().recipe_id, "root");
    }

    #[test]
    fn resolve_run_plan_leaves_existing_recipes_without_sub_recipes_unaffected() {
        let recipe = leaf_recipe("plain");
        let catalog: BTreeMap<String, Recipe> = BTreeMap::new();
        let params = BTreeMap::from([("task".to_string(), "solo".to_string())]);

        let plan = resolve_run_plan(&recipe, &params, &catalog).unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].rendered.rendered_prompt, "Do solo.");
    }
}
