use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    pub id: Option<String>,
    pub version: String,
    pub title: String,
    pub description: String,
    pub instructions: Option<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub activities: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<RecipeParameter>,
    #[serde(default)]
    pub required_toolsets: Vec<String>,
    #[serde(default)]
    pub required_mcp_servers: Vec<RecipeMcpRequirement>,
    #[serde(default)]
    pub required_plugins: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub settings: RecipeSettings,
    #[serde(default)]
    pub response_schema: Option<Value>,
    #[serde(default)]
    pub retry: Option<RetryPolicy>,
    #[serde(default)]
    pub sub_recipes: Vec<SubRecipeRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecipeParameter {
    pub key: String,
    pub input_type: RecipeInputType,
    pub requirement: RequirementLevel,
    pub description: String,
    pub default: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RecipeInputType {
    String,
    Number,
    Boolean,
    Path,
    Secret,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RequirementLevel {
    Required,
    Optional,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecipeSettings {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub workspace: Option<String>,
    pub permission_mode: Option<String>,
    pub toolset: Option<String>,
    pub execution_backend: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecipeMcpRequirement {
    pub server: String,
    #[serde(default)]
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubRecipeRef {
    pub id: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledRecipe {
    pub id: String,
    pub source: RecipeSource,
    pub source_uri: Option<String>,
    pub title: String,
    pub version: String,
    pub checksum: String,
    pub trusted: bool,
    pub recipe: Recipe,
    pub installed_at: String,
    pub latest_validation: Option<RecipeValidationRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RecipeSource {
    Local,
    Inline,
    Github,
    BuiltIn,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RecipeVerdict {
    Pass,
    Warn,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecipeValidationRun {
    pub id: String,
    pub recipe_id: Option<String>,
    pub verdict: RecipeVerdict,
    #[serde(default)]
    pub findings: Vec<RecipeFinding>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecipeFinding {
    pub id: String,
    pub severity: RecipeFindingSeverity,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum RecipeFindingSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenderedRecipe {
    pub recipe_id: Option<String>,
    pub title: String,
    pub rendered_prompt: String,
    pub rendered_prompt_hash: String,
    pub params: BTreeMap<String, String>,
    pub settings: RecipeSettings,
    #[serde(default)]
    pub required_toolsets: Vec<String>,
    #[serde(default)]
    pub required_plugins: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecipeRun {
    pub id: String,
    pub recipe_id: String,
    pub session_id: String,
    pub status: RecipeRunStatus,
    pub params: BTreeMap<String, String>,
    pub rendered_prompt_hash: String,
    pub result_artifact_id: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RecipeRunStatus {
    Submitted,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecipeDeeplink {
    pub recipe_id: String,
    pub action: String,
    pub params: BTreeMap<String, String>,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeDeeplinkError {
    pub message: String,
}

impl std::fmt::Display for RecipeDeeplinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecipeDeeplinkError {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecipeStoreSnapshot {
    #[serde(default)]
    pub recipes: Vec<InstalledRecipe>,
    #[serde(default)]
    pub runs: BTreeMap<String, Vec<RecipeRun>>,
}

#[derive(Debug, Clone, Default)]
pub struct RecipeStore {
    recipes: BTreeMap<String, InstalledRecipe>,
    runs: BTreeMap<String, Vec<RecipeRun>>,
}

impl RecipeStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_snapshot(snapshot: RecipeStoreSnapshot) -> Self {
        let recipes = snapshot
            .recipes
            .into_iter()
            .map(|recipe| (recipe.id.clone(), recipe))
            .collect();
        Self {
            recipes,
            runs: snapshot.runs,
        }
    }

    pub fn snapshot(&self) -> RecipeStoreSnapshot {
        RecipeStoreSnapshot {
            recipes: self.list(),
            runs: self.runs.clone(),
        }
    }

    pub fn install(&mut self, recipe: InstalledRecipe) {
        self.recipes.insert(recipe.id.clone(), recipe);
    }

    pub fn list(&self) -> Vec<InstalledRecipe> {
        self.recipes.values().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<InstalledRecipe> {
        self.recipes.get(id).cloned()
    }

    pub fn add_run(&mut self, run: RecipeRun) {
        self.runs
            .entry(run.recipe_id.clone())
            .or_default()
            .push(run);
    }

    pub fn runs(&self, recipe_id: &str) -> Vec<RecipeRun> {
        self.runs.get(recipe_id).cloned().unwrap_or_default()
    }
}
