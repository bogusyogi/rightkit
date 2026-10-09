//! Resolved lock graphs (Cargo.lock, npm package-lock v2/v3, pnpm v6/v9), without feature
//! or production-only pruning. Port of `ownership-lockfiles.mjs`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Map, Value};

use crate::committed::Bytes;
use crate::inventory::File;
use crate::json::{field, items, locale_compare, sha256_hex, str_of, truthy};
use crate::paths::{posix_basename, posix_dirname, posix_join, posix_normalize, posix_relative};
use crate::types::Cov;

pub const OWNERSHIP_LOCKFILES: [&str; 5] = [
    "Cargo.lock",
    "pnpm-lock.yaml",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
];
const SECTIONS: [&str; 3] = ["dependencies", "devDependencies", "optionalDependencies"];
const MAX_LOCK_BYTES: u64 = 64 * 1024 * 1024;

struct LockError {
    reason: &'static str,
    message: String,
}

fn fail<T>(message: &str) -> Result<T, LockError> {
    Err(LockError {
        reason: "lockfile-parse-failed",
        message: message.into(),
    })
}

fn unsupported<T>(message: impl Into<String>) -> Result<T, LockError> {
    Err(LockError {
        reason: "lockfile-unsupported-format",
        message: message.into(),
    })
}

type R<T> = Result<T, LockError>;

fn is_object(value: &Value) -> bool {
    value.is_object()
}

// ---- pnpm YAML subset -------------------------------------------------------------

fn split_flow(text: &str, delimiter: char) -> R<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut parts = Vec::new();
    let (mut quote, mut depth, mut start) = (None::<char>, 0i32, 0usize);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == '\\' && q == '"' {
                i += 2;
                continue;
            }
            if c == q {
                if q == '\'' && chars.get(i + 1) == Some(&'\'') {
                    i += 1;
                } else {
                    quote = None;
                }
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == '{' || c == '[' {
            depth += 1;
        } else if c == '}' || c == ']' {
            depth -= 1;
        } else if depth == 0
            && c == delimiter
            && (delimiter != ':' || chars.get(i + 1).is_none_or(|n| n.is_whitespace()))
        {
            parts.push(chars[start..i].iter().collect());
            start = i + 1;
            if delimiter == ':' {
                parts.push(chars[start..].iter().collect());
                return Ok(parts);
            }
        }
        if depth < 0 {
            return fail("unbalanced YAML flow value");
        }
        i += 1;
    }
    if quote.is_some() || depth != 0 {
        return fail("unterminated YAML value");
    }
    parts.push(chars[start..].iter().collect());
    Ok(parts)
}

fn scalar(raw: &str) -> R<Value> {
    let value = raw.trim();
    if value.starts_with(['&', '*', '!', '|', '>']) {
        return unsupported("YAML anchors, tags or block scalars");
    }
    if value.starts_with('"') {
        return serde_json::from_str::<Value>(value)
            .or_else(|_| fail("invalid YAML quoted scalar"));
    }
    if let Some(rest) = value.strip_prefix('\'') {
        if !value.ends_with('\'') || value.len() < 2 {
            return fail("invalid YAML quoted scalar");
        }
        return Ok(Value::String(rest[..rest.len() - 1].replace("''", "'")));
    }
    if value.starts_with('{') {
        if !value.ends_with('}') {
            return fail("invalid YAML flow map");
        }
        let inner = &value[1..value.len() - 1];
        let mut out = Map::new();
        if !inner.trim().is_empty() {
            for item in split_flow(inner, ',')? {
                let pair = split_flow(item.trim(), ':')?;
                if pair.len() != 2 {
                    return fail("invalid YAML flow entry");
                }
                let key = match scalar(&pair[0])? {
                    Value::String(key) => key,
                    other => other.to_string(),
                };
                if out.contains_key(&key) {
                    return fail("duplicate YAML key");
                }
                out.insert(key, scalar(&pair[1])?);
            }
        }
        return Ok(Value::Object(out));
    }
    if value.starts_with('[') {
        if !value.ends_with(']') {
            return fail("invalid YAML flow sequence");
        }
        let inner = &value[1..value.len() - 1];
        if inner.trim().is_empty() {
            return Ok(Value::Array(Vec::new()));
        }
        return split_flow(inner, ',')?
            .iter()
            .map(|item| scalar(item))
            .collect::<R<Vec<_>>>()
            .map(Value::Array);
    }
    if value.contains(['[', ']', '{', '}']) {
        return fail("invalid YAML scalar");
    }
    Ok(Value::String(strip_comment(value).to_string()))
}

/// `value.replace(/\s+#.*$/, "")`.
fn strip_comment(value: &str) -> &str {
    let bytes: Vec<(usize, char)> = value.char_indices().collect();
    for (index, (at, c)) in bytes.iter().enumerate() {
        if *c == '#' {
            // Walk back over whitespace; the match needs at least one.
            let mut start = index;
            while start > 0 && bytes[start - 1].1.is_whitespace() {
                start -= 1;
            }
            if start < index {
                return &value[..bytes[start].0.min(*at)];
            }
        }
    }
    value
}

struct Frame {
    indent: i64,
    map: Map<String, Value>,
    sequence: Option<Vec<Value>>,
    key: String,
}

impl Frame {
    fn finish(self) -> (String, Value) {
        let value = match self.sequence {
            Some(list) => Value::Array(list),
            None => Value::Object(self.map),
        };
        (self.key, value)
    }
}

fn parse_pnpm_yaml(text: &str) -> R<Value> {
    let mut stack = vec![Frame {
        indent: -1,
        map: Map::new(),
        sequence: None,
        key: String::new(),
    }];
    let pop = |stack: &mut Vec<Frame>| {
        let frame = stack.pop().expect("frame");
        let (key, value) = frame.finish();
        let parent = stack.last_mut().expect("parent frame");
        parent.map.insert(key, value);
    };
    for line in text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let leading_ws: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        if leading_ws.contains('\t') && leading_ws.trim_start_matches(' ').starts_with('\t') {
            return fail("YAML tab indentation");
        }
        let indent = line.chars().take_while(|c| *c == ' ').count() as i64;
        while stack.len() > 1 && indent <= stack.last().expect("frame").indent {
            pop(&mut stack);
        }
        let parent_indent = stack.last().expect("frame").indent;
        if (indent != parent_indent + 2 && parent_indent != -1)
            || (parent_indent == -1 && indent != 0)
        {
            return fail("invalid YAML indentation");
        }
        let content = &line[indent as usize..];
        if let Some(item) = content.strip_prefix("- ") {
            let parent = stack.last_mut().expect("frame");
            if parent.sequence.is_none() {
                if !parent.map.is_empty() {
                    return fail("mixed YAML collection");
                }
                parent.sequence = Some(Vec::new());
            }
            let value = scalar(item)?;
            parent.sequence.as_mut().expect("sequence").push(value);
            continue;
        }
        if stack.last().expect("frame").sequence.is_some() {
            return fail("mixed YAML collection");
        }
        let pair = split_flow(content, ':')?;
        if pair.len() != 2 {
            return fail("invalid YAML mapping");
        }
        let key = match scalar(&pair[0])? {
            Value::String(key) if !key.is_empty() => key,
            _ => return fail("invalid or duplicate YAML key"),
        };
        if stack.last().expect("frame").map.contains_key(&key) {
            return fail("invalid or duplicate YAML key");
        }
        let value = pair[1].trim();
        let parent = stack.last_mut().expect("frame");
        if value.is_empty() {
            parent.map.insert(key.clone(), Value::Null);
            stack.push(Frame {
                indent,
                map: Map::new(),
                sequence: None,
                key,
            });
        } else {
            let parsed = scalar(value)?;
            parent.map.insert(key, parsed);
        }
    }
    while stack.len() > 1 {
        pop(&mut stack);
    }
    Ok(Value::Object(stack.pop().expect("root").map))
}

// ---- graph ------------------------------------------------------------------------

struct Node {
    id: String,
    name: String,
    version: String,
    source: String,
    local: bool,
    edges: Vec<usize>,
    unresolved: Vec<String>,
}

#[derive(Default)]
struct Graph {
    nodes: Vec<Node>,
    index: HashMap<String, usize>,
    roots: Vec<usize>,
    diagnostics: Vec<(String, String)>,
}

impl Graph {
    fn add(
        &mut self,
        id: &str,
        name: Option<&str>,
        version: Option<&str>,
        source: String,
        local: bool,
    ) -> R<usize> {
        let (Some(name), Some(version)) = (
            name.filter(|n| !n.is_empty()),
            version.filter(|v| !v.is_empty()),
        ) else {
            return fail("package name/version missing");
        };
        let at = self.nodes.len();
        self.nodes.push(Node {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            source,
            local,
            edges: Vec::new(),
            unresolved: Vec::new(),
        });
        self.index.insert(id.into(), at);
        Ok(at)
    }

    fn get(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    fn connect(&mut self, from: usize, target: Option<usize>, reference: &str, optional: bool) {
        match target {
            None => {
                if optional {
                    return;
                }
                let message = format!("{}: {reference}", self.nodes[from].name);
                self.nodes[from].unresolved.push(message);
            }
            Some(target) => self.nodes[from].edges.push(target),
        }
    }
}

fn source_for(value: Option<&str>) -> String {
    match value {
        Some(v) if v.starts_with("git+") => v.to_string(),
        Some(v) if v.starts_with("git:") || is_https_git(v) => format!("git+{v}"),
        Some(v) if !v.is_empty() => v.to_string(),
        _ => "registry".into(),
    }
}

/// `^https?:.*\.git`.
fn is_https_git(value: &str) -> bool {
    let rest = value
        .strip_prefix("https:")
        .or_else(|| value.strip_prefix("http:"));
    rest.is_some_and(|r| r.contains(".git"))
}

fn parse_toml_value(text: &str) -> R<Value> {
    match text.parse::<toml::Table>() {
        Ok(table) => serde_json::to_value(table).or_else(|_| fail("invalid TOML")),
        Err(error) => fail(&format!("Invalid TOML document: {error}")),
    }
}

/// Cargo dependency reference: `^(\S+)(?: (\S+))?(?: \((.+)\))?$`.
fn parse_cargo_ref(reference: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    if reference.contains('\n') {
        return None;
    }
    let name_end = reference.find(' ').unwrap_or(reference.len());
    let name = &reference[..name_end];
    if name.is_empty() {
        return None;
    }
    let rest = &reference[name_end..];
    let source_of = |text: &'_ str| -> Option<usize> {
        // " (" ... ")" with a non-empty body, at the end of `text`.
        (text.starts_with(" (") && text.ends_with(')') && text.len() >= 5).then_some(text.len())
    };
    if let Some(after) = rest.strip_prefix(' ') {
        let run_end = after.find(' ').unwrap_or(after.len());
        let version = &after[..run_end];
        if !version.is_empty() {
            let tail = &after[run_end..];
            if tail.is_empty() {
                return Some((name, Some(version), None));
            }
            if source_of(tail).is_some() {
                return Some((name, Some(version), Some(&tail[2..tail.len() - 1])));
            }
        }
    }
    if rest.is_empty() {
        return Some((name, None, None));
    }
    source_of(rest).map(|_| (name, None, Some(&rest[2..rest.len() - 1])))
}

fn cargo_graph(text: &str, manifests: &[&File]) -> R<Graph> {
    let lock = parse_toml_value(text)?;
    let mut g = Graph::default();
    let packages = match field(&lock, "package").as_array() {
        Some(list) if matches!(field(&lock, "version").as_i64().unwrap_or(1), 1..=4) => {
            list.clone()
        }
        _ => return unsupported("Cargo lock version/package list"),
    };
    for (i, pkg) in packages.iter().enumerate() {
        let source = pkg.get("source").and_then(Value::as_str);
        g.add(
            &i.to_string(),
            pkg.get("name").and_then(Value::as_str),
            pkg.get("version").and_then(Value::as_str),
            source.map_or_else(
                || format!("path:{}", str_of(field(pkg, "name"))),
                String::from,
            ),
            source.is_none(),
        )?;
    }
    for (i, pkg) in packages.iter().enumerate() {
        let deps = field(pkg, "dependencies");
        if !deps.is_null() && !deps.is_array() {
            return fail("Cargo dependencies must be a list");
        }
        for reference in items(deps) {
            let Some(reference) = reference.as_str() else {
                return fail("invalid Cargo dependency reference");
            };
            let Some((name, version, source)) = parse_cargo_ref(reference) else {
                return fail("invalid Cargo dependency reference");
            };
            let candidates: Vec<usize> = g
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    n.name == name
                        && version.is_none_or(|v| n.version == v)
                        && source.is_none_or(|s| n.source == s)
                })
                .map(|(at, _)| at)
                .collect();
            let target = if candidates.len() == 1 {
                Some(candidates[0])
            } else {
                None
            };
            g.connect(i, target, reference, false);
        }
    }
    for manifest in manifests {
        let Some(bytes) = &manifest.bytes else {
            continue;
        };
        let parsed = parse_toml_value(&String::from_utf8_lossy(bytes))?;
        let Some(pkg) = parsed.get("package").filter(|p| truthy(p)) else {
            continue;
        };
        let name = str_of(field(pkg, "name"));
        let version = field(pkg, "version").as_str();
        let candidates: Vec<usize> = g
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.name == name && n.local && version.is_none_or(|v| n.version == v))
            .map(|(at, _)| at)
            .collect();
        if candidates.len() != 1 {
            g.diagnostics
                .push(("lockfile-root-unresolved".into(), manifest.rel.clone()));
            continue;
        }
        g.roots.push(candidates[0]);
    }
    Ok(g)
}

fn optional_peer(pkg: &Value, section: &str, name: &str) -> bool {
    section == "optionalDependencies"
        || (section == "peerDependencies"
            && field(field(field(pkg, "peerDependenciesMeta"), name), "optional").as_bool()
                == Some(true))
}

fn package_lock_graph(text: &str, manifests: &[&File], lock_dir: &str) -> R<Graph> {
    let lock: Value = serde_json::from_str(text).or_else(|e| fail(&e.to_string()))?;
    let mut g = Graph::default();
    let version = field(&lock, "lockfileVersion").as_i64();
    let Some(packages) = field(&lock, "packages")
        .as_object()
        .filter(|_| matches!(version, Some(2 | 3)))
    else {
        return unsupported("npm requires v2/v3 packages");
    };
    for (id, pkg) in packages {
        if !is_object(pkg) {
            return fail("invalid npm package");
        }
        if truthy(field(pkg, "link")) {
            continue;
        }
        let in_modules = id.contains("node_modules/");
        let name = field(pkg, "name").as_str().map(String::from).or_else(|| {
            Some(if in_modules {
                id.rsplit("node_modules/").next().unwrap_or("").to_string()
            } else if !id.is_empty() {
                id.clone()
            } else {
                field(&lock, "name")
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .unwrap_or("app")
                    .to_string()
            })
        });
        let version = field(pkg, "version")
            .as_str()
            .map(String::from)
            .or_else(|| (!in_modules).then(|| "0.0.0".to_string()));
        let resolved = field(pkg, "resolved").as_str();
        let local = !in_modules
            || resolved.is_some_and(|r| r.starts_with("file:") || r.starts_with("link:"));
        g.add(
            id,
            name.as_deref(),
            version.as_deref(),
            source_for(resolved),
            local,
        )?;
    }
    let resolve = |g: &Graph, from: &str, name: &str| -> Option<usize> {
        let mut dir = from.to_string();
        loop {
            let id = posix_join(&[&dir, "node_modules", name]);
            let linked = packages.get(&id);
            let target = match linked {
                Some(l) if truthy(field(l, "link")) => {
                    g.get(&posix_normalize(str_of(field(l, "resolved"))))
                }
                _ => g.get(&id),
            };
            if target.is_some() {
                return target;
            }
            if dir.is_empty() || dir == "." {
                return None;
            }
            dir = posix_dirname(&dir);
            if dir == "." {
                dir.clear();
            }
        }
    };
    let ids: Vec<(usize, String)> = g
        .nodes
        .iter()
        .enumerate()
        .map(|(at, n)| (at, n.id.clone()))
        .collect();
    for (at, id) in &ids {
        let pkg = &packages[id];
        let local = g.nodes[*at].local;
        let list: Vec<&str> = if local {
            SECTIONS
                .iter()
                .copied()
                .chain(["peerDependencies"])
                .collect()
        } else {
            vec!["dependencies", "optionalDependencies", "peerDependencies"]
        };
        for section in list {
            let value = field(pkg, section);
            if !value.is_null() && !is_object(value) {
                return fail("invalid npm dependencies");
            }
            if let Some(map) = value.as_object() {
                for name in map.keys() {
                    let target = resolve(&g, id, name);
                    g.connect(*at, target, name, optional_peer(pkg, section, name));
                }
            }
        }
    }
    for manifest in manifests {
        let Some(bytes) = &manifest.bytes else {
            continue;
        };
        let id = posix_relative(lock_dir, &posix_dirname(&manifest.rel));
        let Some(at) = g.get(&id) else {
            g.diagnostics
                .push(("lockfile-root-unresolved".into(), manifest.rel.clone()));
            continue;
        };
        let pkg: Value = serde_json::from_slice(bytes).or_else(|e| fail(&e.to_string()))?;
        if let Some(name) = field(&pkg, "name").as_str() {
            g.nodes[at].name = name.to_string();
        }
        for section in SECTIONS.iter().copied().chain(["peerDependencies"]) {
            if let Some(map) = field(&pkg, section).as_object() {
                for name in map.keys() {
                    let target = resolve(&g, &id, name);
                    g.connect(at, target, name, optional_peer(&pkg, section, name));
                }
            }
        }
        g.roots.push(at);
    }
    Ok(g)
}

fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "undefined".into(),
        Value::Object(_) => "[object Object]".into(),
        other => other.to_string(),
    }
}

/// `{name, version}` from a pnpm package key.
fn pnpm_identity(id: &str) -> R<(String, String)> {
    let bare = id.strip_prefix('/').unwrap_or(id);
    // ^(@[^/]+\/[^@]+|[^@]+)@(.+)$
    let search_from = usize::from(bare.starts_with('@'));
    let split = bare[search_from..].find('@').map(|p| p + search_from);
    let Some(at) = split.filter(|at| *at > 0 && at + 1 < bare.len()) else {
        return unsupported(format!("pnpm package key: {id}"));
    };
    let name = &bare[..at];
    if bare.starts_with('@') && !name[1..].contains('/') {
        return unsupported(format!("pnpm package key: {id}"));
    }
    let version = &bare[at + 1..];
    let end = version.find(['(', '_']).unwrap_or(version.len());
    Ok((name.to_string(), version[..end].to_string()))
}

fn pnpm_graph(text: &str, manifests: &[&File], lock_dir: &str) -> R<Graph> {
    let lock = parse_pnpm_yaml(text)?;
    let version = js_string(field(&lock, "lockfileVersion"));
    let version_ok = {
        let major = version.split('.').next().unwrap_or("");
        let minor_ok = version
            .split('.')
            .skip(1)
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
            && version.split('.').count() <= 2;
        matches!(major, "9" | "6") && minor_ok
    };
    let importers = field(&lock, "importers");
    let empty = Value::Object(Map::new());
    let packages_value = if field(&lock, "packages").is_null() {
        &empty
    } else {
        field(&lock, "packages")
    };
    if !version_ok || !is_object(importers) || !is_object(packages_value) {
        return unsupported("pnpm requires v6/v9 importers");
    }
    let v9 = version.starts_with('9');
    let snapshots_value = if v9 {
        if field(&lock, "snapshots").is_null() {
            &empty
        } else {
            field(&lock, "snapshots")
        }
    } else {
        packages_value
    };
    let Some(snapshots) = snapshots_value.as_object() else {
        return fail("invalid pnpm snapshots");
    };
    let packages = packages_value.as_object().expect("packages object");
    let mut g = Graph::default();
    for (id, pkg) in snapshots {
        if !is_object(pkg) {
            return fail("invalid pnpm snapshot");
        }
        let (name, identity_version) = pnpm_identity(id)?;
        let metadata = packages
            .get(id)
            .or_else(|| packages.get(id.split('(').next().unwrap_or(id)));
        let meta = metadata.unwrap_or(&empty);
        let resolution = field(meta, "resolution");
        let resolved = field(resolution, "tarball")
            .as_str()
            .map(String::from)
            .or_else(|| {
                field(resolution, "directory")
                    .as_str()
                    .map(|d| format!("path:{d}"))
            });
        let local = field(resolution, "type").as_str() == Some("directory")
            || identity_version.starts_with("file:");
        let version = field(meta, "version")
            .as_str()
            .map(String::from)
            .unwrap_or(identity_version);
        g.add(
            id,
            Some(&name),
            Some(&version),
            source_for(resolved.as_deref()),
            local,
        )?;
    }
    for (id, importer) in importers.as_object().expect("importers") {
        if !is_object(importer) {
            return fail("invalid pnpm importer");
        }
        g.add(
            &format!("importer:{id}"),
            Some(id),
            Some("0.0.0"),
            format!("path:{id}"),
            true,
        )?;
    }
    let resolve = |g: &Graph, node_id: &str, name: &str, raw: &Value| -> R<Option<usize>> {
        let reference = if is_object(raw) {
            field(raw, "version")
        } else {
            raw
        };
        let Some(reference) = reference.as_str() else {
            return fail("invalid pnpm dependency version");
        };
        if reference.starts_with("link:") || reference.starts_with("workspace:") {
            let Some(path) = reference.strip_prefix("link:") else {
                return Err(LockError {
                    reason: "lockfile-unresolved-dependency",
                    message: reference.into(),
                });
            };
            let importer = node_id.strip_prefix("importer:").unwrap_or(node_id);
            return Ok(g.get(&format!(
                "importer:{}",
                posix_normalize(&posix_join(&[importer, path]))
            )));
        }
        for candidate in [
            reference.to_string(),
            format!("{name}@{reference}"),
            format!("/{name}@{reference}"),
        ] {
            if let Some(found) = g.get(&candidate) {
                return Ok(Some(found));
            }
        }
        let normalized = reference.strip_prefix("npm:").unwrap_or(reference);
        Ok(g.get(normalized)
            .or_else(|| g.get(&format!("/{normalized}"))))
    };
    for at in 0..g.nodes.len() {
        let node_id = g.nodes[at].id.clone();
        let importer = node_id.starts_with("importer:");
        let pkg = if importer {
            field(importers, &node_id[9..]).clone()
        } else {
            snapshots[&node_id].clone()
        };
        let list: Vec<&str> = if importer {
            SECTIONS.to_vec()
        } else {
            vec!["dependencies", "optionalDependencies"]
        };
        for section in list {
            let value = field(&pkg, section);
            if !value.is_null() && !is_object(value) {
                return fail("invalid pnpm dependency map");
            }
            if let Some(map) = value.as_object() {
                for (name, raw) in map {
                    let target = resolve(&g, &node_id, name, raw)?;
                    g.connect(at, target, name, section == "optionalDependencies");
                }
            }
        }
    }
    for manifest in manifests {
        let Some(bytes) = &manifest.bytes else {
            continue;
        };
        let relative = posix_relative(lock_dir, &posix_dirname(&manifest.rel));
        let id = if relative.is_empty() {
            ".".to_string()
        } else {
            relative
        };
        let Some(at) = g.get(&format!("importer:{id}")) else {
            g.diagnostics
                .push(("lockfile-root-unresolved".into(), manifest.rel.clone()));
            continue;
        };
        let pkg: Value = serde_json::from_slice(bytes).or_else(|e| fail(&e.to_string()))?;
        g.nodes[at].name = field(&pkg, "name")
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| id.clone());
        let importer = field(importers, &id);
        for section in SECTIONS {
            if let Some(map) = field(&pkg, section).as_object() {
                for name in map.keys() {
                    if field(field(importer, section), name).is_null()
                        && !field(importer, section)
                            .as_object()
                            .is_some_and(|m| m.contains_key(name))
                    {
                        g.diagnostics.push((
                            "lockfile-root-unresolved".into(),
                            format!("{}: {section}.{name}", manifest.rel),
                        ));
                    }
                }
            }
        }
        g.roots.push(at);
    }
    Ok(g)
}

/// A transitive dependency found through a lock graph.
#[derive(Debug, Clone)]
pub struct Transitive {
    pub ecosystem: &'static str,
    pub name: String,
    pub resolved_version: String,
    pub source: String,
    pub file: String,
    pub dependency_path: Vec<String>,
}

impl Transitive {
    /// The lock file the path was found in (same as `file`).
    pub fn lockfile(&self) -> &str {
        &self.file
    }
}

pub struct ContentDigest {
    pub path: String,
    pub digest: String,
    pub provenance: Option<Value>,
}

pub struct LockContext<'a> {
    pub app_root: &'a Path,
    pub files: &'a [File],
    pub committed: bool,
}

/// Lock bytes stay bound to the scan inventory or committed snapshot; no installed graph
/// reads.
pub fn transitive_dependencies(
    ctx: &LockContext<'_>,
    manifests: &[&File],
    catalog_owners: &HashSet<String>,
    coverage: &mut Vec<Cov>,
    content_digests: &mut Vec<ContentDigest>,
    targets: &HashSet<String>,
) -> Vec<Transitive> {
    let mut result: Vec<Transitive> = Vec::new();
    let mut cache: HashMap<String, Option<Bytes>> = HashMap::new();
    let mut groups: Vec<(String, &'static str, Vec<&File>)> = Vec::new();
    let by_rel: HashMap<&str, &File> = ctx.files.iter().map(|f| (f.rel.as_str(), f)).collect();
    let mut read = |rel: &str,
                    coverage: &mut Vec<Cov>,
                    content_digests: &mut Vec<ContentDigest>|
     -> Result<Option<Bytes>, LockError> {
        if let Some(hit) = cache.get(rel) {
            return Ok(hit.clone());
        }
        let file = by_rel.get(rel);
        let mut bytes = file.and_then(|f| f.bytes.clone());
        let mut known = file.is_some();
        if file.is_none() && !ctx.committed {
            let absolute = crate::paths::resolve(&ctx.app_root.join(rel));
            if absolute.exists() {
                let meta = std::fs::symlink_metadata(&absolute).map_err(|_| LockError {
                    reason: "lockfile-unreadable",
                    message: rel.into(),
                })?;
                let root = crate::paths::real(ctx.app_root).ok();
                let real = crate::paths::real(&absolute).ok();
                let inside =
                    matches!((&root, &real), (Some(r), Some(p)) if p.starts_with(r) && p != r);
                if !meta.is_file()
                    || meta.file_type().is_symlink()
                    || !inside
                    || meta.len() > MAX_LOCK_BYTES
                {
                    return Err(LockError {
                        reason: "lockfile-unreadable",
                        message: rel.into(),
                    });
                }
                bytes = std::fs::read(&absolute).ok().map(std::sync::Arc::new);
                known = true;
                coverage.push(Cov::with_detail(
                    rel,
                    "lockfile-not-in-inventory",
                    "lock bytes are absent from Git-listed scan inputs",
                ));
            }
        }
        if known && bytes.is_none() {
            return Err(LockError {
                reason: "lockfile-unreadable",
                message: rel.into(),
            });
        }
        cache.insert(rel.to_string(), bytes.clone());
        if let Some(bytes) = &bytes {
            if !content_digests.iter().any(|item| item.path == rel) {
                content_digests.push(ContentDigest {
                    path: rel.into(),
                    digest: sha256_hex(bytes),
                    provenance: None,
                });
            }
        }
        Ok(bytes)
    };
    for manifest in manifests {
        let base = posix_basename(&manifest.rel);
        let ecosystem: &'static str = match base {
            "Cargo.toml" => "cargo",
            "package.json" => "npm",
            _ => continue,
        };
        if manifest.bytes.is_none() {
            continue;
        }
        let names: Vec<&str> = if ecosystem == "cargo" {
            vec!["Cargo.lock"]
        } else {
            OWNERSHIP_LOCKFILES[1..].to_vec()
        };
        let mut dir = posix_dirname(&manifest.rel);
        let mut found = false;
        let outcome: Result<(), LockError> = (|| loop {
            let mut locks = Vec::new();
            for name in &names {
                let rel = posix_join(&[&dir, name]);
                if read(&rel, coverage, content_digests)?.is_some() {
                    locks.push(rel);
                }
            }
            if !locks.is_empty() {
                for rel in locks {
                    match groups.iter_mut().find(|(r, _, _)| *r == rel) {
                        Some(group) => group.2.push(manifest),
                        None => groups.push((rel, ecosystem, vec![manifest])),
                    }
                }
                found = true;
                return Ok(());
            }
            if dir == "." {
                return Ok(());
            }
            dir = posix_dirname(&dir);
        })();
        match outcome {
            Err(error) => coverage.push(Cov::with_detail(
                manifest.rel.clone(),
                error.reason,
                error.message,
            )),
            Ok(()) if !found => coverage.push(Cov::with_detail(
                manifest.rel.clone(),
                "lockfile-missing",
                ecosystem,
            )),
            Ok(()) => {}
        }
    }
    for (rel, ecosystem, group) in &groups {
        let outcome: R<()> = (|| {
            let bytes = read(rel, coverage, content_digests)?.expect("lock bytes");
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let base = posix_basename(rel);
            let dir = posix_dirname(rel);
            if base == "yarn.lock" {
                return unsupported("yarn transitive graph");
            }
            let g = if *ecosystem == "cargo" {
                cargo_graph(&text, group)?
            } else if base == "pnpm-lock.yaml" {
                pnpm_graph(&text, group, &dir)?
            } else {
                package_lock_graph(&text, group, &dir)?
            };
            for (reason, detail) in &g.diagnostics {
                coverage.push(Cov::with_detail(
                    rel.clone(),
                    reason.clone(),
                    detail.clone(),
                ));
            }
            for node in &g.nodes {
                for detail in &node.unresolved {
                    coverage.push(Cov::with_detail(
                        rel.clone(),
                        "lockfile-unresolved-dependency",
                        detail.clone(),
                    ));
                }
            }
            let mut reverse: HashMap<usize, Vec<usize>> = HashMap::new();
            let mut viable: HashSet<usize> = HashSet::new();
            let mut ancestors: Vec<usize> = Vec::new();
            for (at, node) in g.nodes.iter().enumerate() {
                if !node.local && catalog_owners.contains(&node.name) {
                    continue;
                }
                if targets.contains(&format!("{ecosystem}:{}", node.name)) && viable.insert(at) {
                    ancestors.push(at);
                }
                for edge in &node.edges {
                    reverse.entry(*edge).or_default().push(at);
                }
            }
            let mut cursor = 0;
            while cursor < ancestors.len() {
                let current = ancestors[cursor];
                cursor += 1;
                for parent in reverse.get(&current).cloned().unwrap_or_default() {
                    if viable.insert(parent) {
                        ancestors.push(parent);
                    }
                }
            }
            let mut seen: HashSet<String> = HashSet::new();
            let mut steps = 0usize;
            let mut unique_roots: Vec<usize> = Vec::new();
            for root in &g.roots {
                if !unique_roots.contains(root) {
                    unique_roots.push(*root);
                }
            }
            let limit = || LockError {
                reason: "lockfile-traversal-limit",
                message: "more than 20000 dependency paths".into(),
            };
            for root in unique_roots {
                let mut queue: Vec<Vec<usize>> = vec![vec![root]];
                let mut at = 0;
                while at < queue.len() {
                    steps += 1;
                    if steps > 20000 {
                        return Err(limit());
                    }
                    let ids = queue[at].clone();
                    at += 1;
                    let node = &g.nodes[*ids.last().expect("path")];
                    if ids.len() > 1 && !node.local && catalog_owners.contains(&node.name) {
                        continue;
                    }
                    let dependency_path: Vec<String> =
                        ids.iter().map(|id| g.nodes[*id].name.clone()).collect();
                    if ids.len() > 2 && targets.contains(&format!("{ecosystem}:{}", node.name)) {
                        let key = serde_json::to_string(&(
                            &node.name,
                            &node.version,
                            &node.source,
                            &dependency_path,
                        ))
                        .unwrap_or_default();
                        if seen.insert(key) {
                            result.push(Transitive {
                                ecosystem,
                                name: node.name.clone(),
                                resolved_version: node.version.clone(),
                                source: node.source.clone(),
                                file: rel.clone(),
                                dependency_path,
                            });
                        }
                    }
                    for edge in &node.edges {
                        if viable.contains(edge) && !ids.contains(edge) {
                            if queue.len() >= 20000 {
                                return Err(limit());
                            }
                            let mut next = ids.clone();
                            next.push(*edge);
                            queue.push(next);
                        }
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            coverage.push(Cov::with_detail(rel.clone(), error.reason, error.message));
        }
    }
    result.sort_by(|a, b| {
        a.dependency_path
            .len()
            .cmp(&b.dependency_path.len())
            .then_with(|| {
                locale_compare(
                    &serde_json::to_string(&a.dependency_path).unwrap_or_default(),
                    &serde_json::to_string(&b.dependency_path).unwrap_or_default(),
                )
            })
    });
    result
}

/// TOML text as JSON, preserving document order.
pub fn parse_toml(text: &str) -> Result<Value, String> {
    parse_toml_value(text).map_err(|e| e.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_references_parse_like_the_regex() {
        assert_eq!(parse_cargo_ref("serde"), Some(("serde", None, None)));
        assert_eq!(
            parse_cargo_ref("serde 1.0.0"),
            Some(("serde", Some("1.0.0"), None))
        );
        assert_eq!(
            parse_cargo_ref("serde 1.0.0 (registry+https://x y)"),
            Some(("serde", Some("1.0.0"), Some("registry+https://x y")))
        );
        assert_eq!(
            parse_cargo_ref("serde (registry+x)"),
            Some(("serde", Some("(registry+x)"), None))
        );
    }

    #[test]
    fn pnpm_yaml_subset_parses_maps_and_flow_values() {
        let text = "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      a:\n        version: 1.0.0\npackages:\n  a@1.0.0:\n    resolution: {integrity: sha512-x}\n";
        let value = parse_pnpm_yaml(text).ok().expect("parse");
        assert_eq!(value["lockfileVersion"], "9.0");
        assert_eq!(
            value["importers"]["."]["dependencies"]["a"]["version"],
            "1.0.0"
        );
        assert_eq!(
            value["packages"]["a@1.0.0"]["resolution"]["integrity"],
            "sha512-x"
        );
    }
}
