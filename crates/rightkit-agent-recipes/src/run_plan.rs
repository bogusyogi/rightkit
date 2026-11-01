use crate::checksum::stable_recipe_id;
use crate::render::render_recipe;
use crate::types::{Recipe, RenderedRecipe};
use std::collections::BTreeMap;

/// Maximum sub-recipe nesting depth the runner will resolve before erroring.
/// Bounds worst-case expansion even when a catalog somehow evades the DFS
/// cycle check (e.g. a caller skips `validate_recipe_with_catalog` first).
pub const MAX_SUB_RECIPE_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRunPlanError {
    pub message: String,
}

impl std::fmt::Display for RecipeRunPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecipeRunPlanError {}

fn plan_error(message: impl Into<String>) -> RecipeRunPlanError {
    RecipeRunPlanError {
        message: message.into(),
    }
}

/// One resolved step in a recipe run plan: a rendered recipe (root or
/// sub-recipe) plus the id path from the root that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRunPlanStep {
    pub recipe_id: String,
    pub path: Vec<String>,
    pub rendered: RenderedRecipe,
}

/// A fully resolved recipe run plan: sub-recipe steps in dependency order
/// (children before the parent that invoked them), followed by the root
/// recipe's own render as the final step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRunPlan {
    pub steps: Vec<RecipeRunPlanStep>,
}

impl RecipeRunPlan {
    /// The root recipe's rendered step — always the last entry.
    pub fn root_step(&self) -> Option<&RecipeRunPlanStep> {
        self.steps.last()
    }
}

/// Resolve a recipe (and, transitively, its `sub_recipes`) into an ordered
/// run plan, threading each `SubRecipeRef`'s declared params into the child
/// recipe's own params before rendering it.
///
/// `catalog` must contain every recipe referenced (directly or transitively)
/// by `recipe.sub_recipes`, keyed by id. Missing entries, self-reference,
/// transitive cycles, and depth beyond [`MAX_SUB_RECIPE_DEPTH`] all error
/// rather than recursing unboundedly.
///
/// This performs its own cycle/depth guard independent of
/// `validate_recipe_with_catalog` — callers that already validated a recipe
/// still get a safe resolution, and callers that skipped validation cannot
/// drive this function into infinite recursion.
pub fn resolve_run_plan(
    recipe: &Recipe,
    params: &BTreeMap<String, String>,
    catalog: &BTreeMap<String, Recipe>,
) -> Result<RecipeRunPlan, RecipeRunPlanError> {
    let root_id = stable_recipe_id(recipe);
    let mut steps = Vec::new();
    let mut stack = vec![root_id.clone()];
    resolve_step(recipe, params, catalog, &mut stack, &mut steps)?;
    Ok(RecipeRunPlan { steps })
}

fn resolve_step(
    recipe: &Recipe,
    params: &BTreeMap<String, String>,
    catalog: &BTreeMap<String, Recipe>,
    stack: &mut Vec<String>,
    steps: &mut Vec<RecipeRunPlanStep>,
) -> Result<(), RecipeRunPlanError> {
    if stack.len() > MAX_SUB_RECIPE_DEPTH {
        return Err(plan_error(format!(
            "sub-recipe nesting exceeds max depth {} at `{}`",
            MAX_SUB_RECIPE_DEPTH,
            stack.join(" -> ")
        )));
    }

    for sub_recipe in &recipe.sub_recipes {
        let sub_id = sub_recipe.id.trim();
        if sub_id.is_empty() {
            return Err(plan_error("sub-recipe reference has an empty id"));
        }
        if stack.iter().any(|id| id == sub_id) {
            let mut cycle = stack.clone();
            cycle.push(sub_id.to_string());
            return Err(plan_error(format!(
                "sub-recipe graph cycles through `{}`: {}",
                sub_id,
                cycle.join(" -> ")
            )));
        }
        let child = catalog.get(sub_id).ok_or_else(|| {
            plan_error(format!(
                "sub-recipe `{sub_id}` is not present in the recipe catalog"
            ))
        })?;

        let mut child_params = child_default_params(child);
        for (key, value) in &sub_recipe.params {
            child_params.insert(key.clone(), value.clone());
        }

        stack.push(sub_id.to_string());
        resolve_step(child, &child_params, catalog, stack, steps)?;
        stack.pop();
    }

    let rendered = render_recipe(recipe, params)
        .map_err(|err| plan_error(format!("failed to render `{}`: {err}", stack.join(" -> "))))?;
    steps.push(RecipeRunPlanStep {
        recipe_id: stable_recipe_id(recipe),
        path: stack.clone(),
        rendered,
    });
    Ok(())
}

fn child_default_params(child: &Recipe) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for param in &child.parameters {
        if let Some(value) = param.value.clone().or_else(|| param.default.clone()) {
            values.insert(param.key.clone(), value);
        }
    }
    values
}
