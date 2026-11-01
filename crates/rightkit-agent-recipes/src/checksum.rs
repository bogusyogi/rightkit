use crate::types::Recipe;

pub fn recipe_checksum(recipe: &Recipe) -> String {
    let bytes = serde_json::to_vec(recipe).unwrap_or_default();
    sha256_hex(&bytes)
}

pub fn stable_recipe_id(recipe: &Recipe) -> String {
    recipe
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("recipe-{}", &recipe_checksum(recipe)[..12]))
}
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}
