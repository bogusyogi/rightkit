//! Structured-config checks run on a file an edit just changed: JSON (with the comment- and
//! trailing-comma-tolerant form for the files that allow it), TOML and YAML. The edit tool's own
//! result carries the parse error, and the verify fan-out reads the same parser, so both agree on
//! what a healthy config file is.

use std::path::Path;

fn is_json_ext(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "json")
}

fn is_toml_ext(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "toml")
}

fn is_yaml_ext(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "yaml" || e == "yml")
}

/// `tsconfig*.json`, `jsconfig*.json`, `.eslintrc*.json` and anything under `.vscode/` are JSON with comments
/// and trailing commas; their tools accept that, so a strict parse would call a healthy file broken.
pub(crate) fn is_jsonc_path(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with("tsconfig")
        || name.starts_with("jsconfig")
        || name.starts_with(".eslintrc")
        || path.components().any(|part| part.as_os_str() == ".vscode")
}

/// `text` with comments and trailing commas replaced by spaces, so line and column numbers stay true.
fn blank_jsonc_extras(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let (mut i, mut in_string) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
        } else if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            let end = (i + 2..chars.len().saturating_sub(1))
                .find(|&at| chars[at] == '*' && chars[at + 1] == '/')
                .map_or(chars.len(), |at| at + 2);
            for &skipped in &chars[i..end] {
                out.push(if skipped == '\n' { '\n' } else { ' ' });
            }
            i = end;
        } else {
            out.push(c);
            i += 1;
        }
    }
    // A comma whose next real character closes the object or array is a trailing comma.
    let mut in_string = false;
    let mut at = 0;
    while at < out.len() {
        match out[at] {
            '"' => in_string = !in_string,
            '\\' if in_string => at += 1,
            ',' if !in_string => {
                let next = out[at + 1..].iter().find(|ch| !ch.is_whitespace());
                if matches!(next, Some('}') | Some(']')) {
                    out[at] = ' ';
                }
            }
            _ => {}
        }
        at += 1;
    }
    out.into_iter().collect()
}

/// Strict JSON parse of `text`, or a comment- and trailing-comma-tolerant one for the files that allow them.
pub(crate) fn parse_json_text(
    path: &Path,
    text: &str,
) -> Result<serde_json::Value, serde_json::Error> {
    if is_jsonc_path(path) {
        serde_json::from_str(&blank_jsonc_extras(text))
    } else {
        serde_json::from_str(text)
    }
}

/// The parse error (with its line) of a just-edited `.json`, `.toml`, `.yaml` or `.yml` file that no longer parses,
/// for the edit tool's own result. `None` for any other file, a file that parses, or one that is gone.
pub(crate) fn config_parse_error(workspace_root: &Path, path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(workspace_root.join(path)).ok()?;
    config_text_error(path, &text)
}

/// [`config_parse_error`] for text already in hand.
fn config_text_error(path: &Path, text: &str) -> Option<String> {
    let error = if is_json_ext(path.to_str().unwrap_or("")) {
        parse_json_text(path, text).err()?.to_string()
    } else if is_toml_ext(path.to_str().unwrap_or("")) {
        toml::from_str::<toml::Value>(text).err()?.to_string()
    } else if is_yaml_ext(path.to_str().unwrap_or("")) {
        // Every document of a multi-document file must parse.
        serde_yaml::Deserializer::from_str(text)
            .find_map(|document| {
                <serde_yaml::Value as serde::Deserialize>::deserialize(document).err()
            })?
            .to_string()
    } else {
        return None;
    };
    Some(error)
}

/// True when `previous` (the file before the edit) already failed to parse with the same error, ignoring
/// line and column numbers: that fault is not the edit's.
pub(crate) fn config_error_predates_edit(path: &Path, previous: &str, current_error: &str) -> bool {
    let key = |text: &str| {
        text.chars()
            .filter(|c| !c.is_ascii_digit())
            .collect::<String>()
    };
    config_text_error(path, previous).is_some_and(|before| key(&before) == key(current_error))
}
