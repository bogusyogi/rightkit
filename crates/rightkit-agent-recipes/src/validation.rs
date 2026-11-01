use crate::checksum::stable_recipe_id;
use crate::types::{
    Recipe, RecipeFinding, RecipeFindingSeverity, RecipeValidationRun, RecipeVerdict,
    RequirementLevel, SubRecipeRef,
};
use std::collections::{BTreeMap, BTreeSet};

pub fn validate_recipe(
    recipe: &Recipe,
    run_id: impl Into<String>,
    created_at: impl Into<String>,
) -> RecipeValidationRun {
    validate_recipe_with_catalog(recipe, &BTreeMap::new(), run_id, created_at)
}

pub fn validate_recipe_with_catalog(
    recipe: &Recipe,
    catalog: &BTreeMap<String, Recipe>,
    run_id: impl Into<String>,
    created_at: impl Into<String>,
) -> RecipeValidationRun {
    let mut findings = Vec::new();

    if recipe.title.trim().is_empty() {
        findings.push(finding(
            findings.len(),
            RecipeFindingSeverity::Critical,
            "missing_title",
            "Recipe title is required.",
        ));
    }
    if recipe.description.trim().is_empty() {
        findings.push(finding(
            findings.len(),
            RecipeFindingSeverity::Critical,
            "missing_description",
            "Recipe description is required.",
        ));
    }
    if recipe
        .prompt
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
        && recipe
            .instructions
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    {
        findings.push(finding(
            findings.len(),
            RecipeFindingSeverity::Critical,
            "missing_prompt",
            "Recipe requires at least one of prompt or instructions.",
        ));
    }

    let mut param_keys = BTreeSet::new();
    for param in &recipe.parameters {
        if param.key.trim().is_empty() {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "empty_parameter_key",
                "Recipe parameter keys cannot be empty.",
            ));
        } else if !param_keys.insert(param.key.clone()) {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "duplicate_parameter_key",
                format!("Duplicate recipe parameter key `{}`.", param.key),
            ));
        }
    }

    let references = template_references(recipe);
    for reference in references {
        if !param_keys.contains(&reference) {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "unknown_parameter_reference",
                format!("Template references unknown parameter `{reference}`."),
            ));
        }
    }

    for requirement in &recipe.required_mcp_servers {
        if requirement.server.trim().is_empty() {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "missing_mcp_server",
                "MCP requirements must name a server.",
            ));
        }
        if requirement
            .commands
            .iter()
            .any(|command| command.contains("&&") || command.contains(';'))
        {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "unsafe_mcp_command",
                "MCP command requirements must be simple command identifiers, not shell fragments.",
            ));
        }
    }

    if let Some(schema) = &recipe.response_schema {
        if !schema.is_object() {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "invalid_response_schema",
                "Recipe response_schema must be a JSON object.",
            ));
        }
    }

    validate_sub_recipe_graph(recipe, catalog, &mut findings);

    let verdict = if findings
        .iter()
        .any(|finding| finding.severity == RecipeFindingSeverity::Critical)
    {
        RecipeVerdict::Block
    } else if findings
        .iter()
        .any(|finding| finding.severity == RecipeFindingSeverity::Warning)
    {
        RecipeVerdict::Warn
    } else {
        RecipeVerdict::Pass
    };

    RecipeValidationRun {
        id: run_id.into(),
        recipe_id: recipe.id.clone(),
        verdict,
        findings,
        created_at: created_at.into(),
    }
}

fn validate_sub_recipe_graph(
    recipe: &Recipe,
    catalog: &BTreeMap<String, Recipe>,
    findings: &mut Vec<RecipeFinding>,
) {
    let recipe_id = stable_recipe_id(recipe);
    let mut refs = BTreeSet::new();
    for sub_recipe in &recipe.sub_recipes {
        let sub_id = sub_recipe.id.trim();
        if sub_id.is_empty() {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "empty_sub_recipe_id",
                "Sub-recipes must name a recipe id.",
            ));
            continue;
        }
        if !refs.insert(sub_id.to_string()) {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "duplicate_sub_recipe",
                format!("Duplicate sub-recipe reference `{sub_id}`."),
            ));
        }
        if sub_id == recipe_id {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "sub_recipe_cycle",
                "Recipe cannot include itself as a sub-recipe.",
            ));
        }
    }

    if catalog.is_empty() {
        return;
    }

    for sub_recipe in &recipe.sub_recipes {
        let sub_id = sub_recipe.id.trim();
        if sub_id.is_empty() || sub_id == recipe_id {
            continue;
        }
        let Some(target) = catalog.get(sub_id) else {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "unknown_sub_recipe",
                format!("Sub-recipe `{sub_id}` is not installed in the recipe catalog."),
            ));
            continue;
        };

        validate_sub_recipe_params(sub_recipe, target, findings);

        let mut path = vec![recipe_id.clone(), sub_id.to_string()];
        let mut visited = BTreeSet::new();
        if let Some(cycle) =
            find_sub_recipe_cycle(sub_id, &recipe_id, catalog, &mut path, &mut visited)
        {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "sub_recipe_cycle",
                format!("Sub-recipe graph cycles through `{}`.", cycle.join(" -> ")),
            ));
        }
    }
}

fn validate_sub_recipe_params(
    sub_recipe: &SubRecipeRef,
    target: &Recipe,
    findings: &mut Vec<RecipeFinding>,
) {
    let target_keys = target
        .parameters
        .iter()
        .map(|param| param.key.as_str())
        .collect::<BTreeSet<_>>();
    for key in sub_recipe.params.keys() {
        if !target_keys.contains(key.as_str()) {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "unknown_sub_recipe_parameter",
                format!(
                    "Sub-recipe `{}` passes unknown parameter `{}`.",
                    sub_recipe.id, key
                ),
            ));
        }
    }

    for param in &target.parameters {
        if param.requirement == RequirementLevel::Required
            && param.default.is_none()
            && param.value.is_none()
            && !sub_recipe.params.contains_key(&param.key)
        {
            findings.push(finding(
                findings.len(),
                RecipeFindingSeverity::Critical,
                "missing_sub_recipe_parameter",
                format!(
                    "Sub-recipe `{}` is missing required parameter `{}`.",
                    sub_recipe.id, param.key
                ),
            ));
        }
    }
}

fn find_sub_recipe_cycle(
    current: &str,
    root: &str,
    catalog: &BTreeMap<String, Recipe>,
    path: &mut Vec<String>,
    visited: &mut BTreeSet<String>,
) -> Option<Vec<String>> {
    if current == root {
        return Some(path.clone());
    }
    if !visited.insert(current.to_string()) {
        return None;
    }
    let recipe = catalog.get(current)?;
    for sub_recipe in &recipe.sub_recipes {
        let next = sub_recipe.id.trim();
        if next.is_empty() {
            continue;
        }
        path.push(next.to_string());
        if let Some(cycle) = find_sub_recipe_cycle(next, root, catalog, path, visited) {
            return Some(cycle);
        }
        path.pop();
    }
    None
}
fn template_references(recipe: &Recipe) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for template in recipe
        .instructions
        .iter()
        .chain(recipe.prompt.iter())
        .chain(recipe.activities.iter())
    {
        refs.extend(references_in_template(template));
    }
    refs
}

fn references_in_template(template: &str) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            break;
        };
        let key = after_start[..end].trim();
        if !key.is_empty() {
            refs.insert(key.to_string());
        }
        rest = &after_start[end + 2..];
    }
    refs
}

fn finding(
    index: usize,
    severity: RecipeFindingSeverity,
    kind: impl Into<String>,
    message: impl Into<String>,
) -> RecipeFinding {
    RecipeFinding {
        id: format!("RF{:03}", index + 1),
        severity,
        kind: kind.into(),
        message: message.into(),
    }
}
