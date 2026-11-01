use crate::checksum::sha256_hex;
use crate::types::{Recipe, RenderedRecipe, RequirementLevel};
use std::collections::BTreeMap;

pub fn render_recipe(
    recipe: &Recipe,
    params: &BTreeMap<String, String>,
) -> Result<RenderedRecipe, RecipeRenderError> {
    let values = parameter_values(recipe, params)?;
    let mut sections = Vec::new();
    if let Some(instructions) = recipe.instructions.as_deref() {
        if !instructions.trim().is_empty() {
            sections.push(render_template(instructions, &values)?);
        }
    }
    if let Some(prompt) = recipe.prompt.as_deref() {
        if !prompt.trim().is_empty() {
            sections.push(render_template(prompt, &values)?);
        }
    }
    for activity in &recipe.activities {
        sections.push(render_template(activity, &values)?);
    }
    let rendered_prompt = sections.join("\n\n");
    Ok(RenderedRecipe {
        recipe_id: recipe.id.clone(),
        title: recipe.title.clone(),
        rendered_prompt_hash: sha256_hex(rendered_prompt.as_bytes()),
        rendered_prompt,
        params: values,
        settings: recipe.settings.clone(),
        required_toolsets: recipe.required_toolsets.clone(),
        required_plugins: recipe.required_plugins.clone(),
        skills: recipe.skills.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRenderError {
    pub message: String,
}

impl std::fmt::Display for RecipeRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecipeRenderError {}
fn parameter_values(
    recipe: &Recipe,
    params: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, RecipeRenderError> {
    let mut values = BTreeMap::new();
    for param in &recipe.parameters {
        let value = params
            .get(&param.key)
            .cloned()
            .or_else(|| param.value.clone())
            .or_else(|| param.default.clone());
        match (value, &param.requirement) {
            (Some(value), _) => {
                values.insert(param.key.clone(), value);
            }
            (None, RequirementLevel::Required) => {
                return Err(RecipeRenderError {
                    message: format!("missing required recipe parameter `{}`", param.key),
                });
            }
            (None, RequirementLevel::Optional) => {
                values.insert(param.key.clone(), String::new());
            }
        }
    }
    Ok(values)
}

fn render_template(
    template: &str,
    values: &BTreeMap<String, String>,
) -> Result<String, RecipeRenderError> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            return Err(RecipeRenderError {
                message: "unterminated recipe template parameter".to_string(),
            });
        };
        let key = after_start[..end].trim();
        let Some(value) = values.get(key) else {
            return Err(RecipeRenderError {
                message: format!("unknown recipe parameter `{key}`"),
            });
        };
        out.push_str(value);
        rest = &after_start[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}
