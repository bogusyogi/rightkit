use crate::types::{RecipeDeeplink, RecipeDeeplinkError};
use std::collections::BTreeMap;

/// URL scheme of recipe deeplinks unless the host sets its own.
pub const DEFAULT_DEEPLINK_SCHEME: &str = "rightkit-agent";

static DEEPLINK_SCHEME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the deeplink scheme (the part before `://`, for example `myapp`) that
/// [`build_recipe_deeplink`] writes and [`parse_recipe_deeplink`] requires. Call once at start-up;
/// returns `false` when one was already set.
pub fn set_deeplink_scheme(scheme: &str) -> bool {
    DEEPLINK_SCHEME.set(scheme.to_string()).is_ok()
}

/// The deeplink scheme in force.
pub fn deeplink_scheme() -> &'static str {
    DEEPLINK_SCHEME
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_DEEPLINK_SCHEME)
}

pub fn build_recipe_deeplink(
    recipe_id: &str,
    action: &str,
    params: &BTreeMap<String, String>,
) -> Result<RecipeDeeplink, RecipeDeeplinkError> {
    let recipe_id = recipe_id.trim();
    if recipe_id.is_empty() {
        return Err(deeplink_error("recipe id is required"));
    }
    let action = normalize_deeplink_action(action)?;
    let params_json = serde_json::to_string(params)
        .map_err(|e| deeplink_error(format!("failed to serialize params: {e}")))?;
    let url = format!(
        "{}://recipes/{}?action={}&params={}",
        deeplink_scheme(),
        percent_encode(recipe_id),
        percent_encode(&action),
        percent_encode(&params_json)
    );
    Ok(RecipeDeeplink {
        recipe_id: recipe_id.to_string(),
        action,
        params: params.clone(),
        url,
    })
}

pub fn parse_recipe_deeplink(url: &str) -> Result<RecipeDeeplink, RecipeDeeplinkError> {
    let trimmed = url.trim();
    let prefix = format!("{}://recipes/", deeplink_scheme());
    let Some(rest) = trimmed.strip_prefix(prefix.as_str()) else {
        return Err(deeplink_error(format!(
            "recipe deeplink must start with {prefix}"
        )));
    };
    let (recipe_part, query_part) = rest.split_once('?').unwrap_or((rest, ""));
    let recipe_id = percent_decode(recipe_part)?;
    if recipe_id.trim().is_empty() {
        return Err(deeplink_error("recipe id is required"));
    }

    let mut action = "run".to_string();
    let mut params = BTreeMap::new();
    for pair in query_part.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(raw_key)?;
        let value = percent_decode(raw_value)?;
        match key.as_str() {
            "action" => action = normalize_deeplink_action(&value)?,
            "params" => {
                params = serde_json::from_str(&value)
                    .map_err(|e| deeplink_error(format!("invalid params JSON: {e}")))?;
            }
            _ => {}
        }
    }

    Ok(RecipeDeeplink {
        url: build_recipe_deeplink(&recipe_id, &action, &params)?.url,
        recipe_id,
        action,
        params,
    })
}

fn normalize_deeplink_action(action: &str) -> Result<String, RecipeDeeplinkError> {
    let action = action.trim();
    if action.is_empty() || action.eq_ignore_ascii_case("run") {
        Ok("run".to_string())
    } else if action.eq_ignore_ascii_case("render") {
        Ok("render".to_string())
    } else if action.eq_ignore_ascii_case("schedule") {
        Ok("schedule".to_string())
    } else {
        Err(deeplink_error(format!(
            "unsupported recipe deeplink action `{action}`"
        )))
    }
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(value: &str) -> Result<String, RecipeDeeplinkError> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(deeplink_error("invalid percent escape"));
            }
            let hi =
                hex_value(bytes[i + 1]).ok_or_else(|| deeplink_error("invalid percent escape"))?;
            let lo =
                hex_value(bytes[i + 2]).ok_or_else(|| deeplink_error("invalid percent escape"))?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|e| deeplink_error(format!("invalid UTF-8 in deeplink: {e}")))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn deeplink_error(message: impl Into<String>) -> RecipeDeeplinkError {
    RecipeDeeplinkError {
        message: message.into(),
    }
}
