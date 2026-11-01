fn openapi_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if let Some(param) = segment.strip_prefix(':') {
                format!("{{{param}}}")
            } else {
                segment.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn path_parameters(path: &str) -> Vec<Value> {
    path.split('/')
        .filter_map(|segment| {
            segment
                .strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
        })
        .map(|name| {
            json!({
                "name": name,
                "in": "path",
                "required": true,
                "schema": {"type": "string"}
            })
        })
        .collect()
}

fn operation_id(method: &str, path: &str) -> String {
    let suffix = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            segment
                .trim_matches(|ch| ch == '{' || ch == '}')
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .collect::<String>()
        })
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if suffix.is_empty() {
        method.to_string()
    } else {
        format!("{method}_{suffix}")
    }
}
