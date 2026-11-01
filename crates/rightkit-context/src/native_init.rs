//! Explicit native `membrane init` enrollment.
//!
//! Operator-owned registry mutation: nothing here starts, stops or installs the
//! resident.

use serde_json::{json, Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct InitOptions {
    pub root: PathBuf,
    pub repository_id: String,
    pub scope_id: String,
    pub rebind_from: Option<PathBuf>,
    pub virtual_id: Option<String>,
    pub tenant_id: Option<String>,
    pub parents: Vec<String>,
    pub native: Option<String>,
    pub config: Option<PathBuf>,
    pub dry_run: bool,
}

pub fn init(options: &InitOptions) -> Result<Value, String> {
    let path = rightkit_context_runtime::authorization::default_registry_path()
        .ok_or_else(|| "registry path is unresolvable".to_string())?;
    if options.rebind_from.is_some() {
        rebind_at(options, &path)
    } else {
        init_at(options, &path, None)
    }
}

fn registry_key(path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut value = absolute.to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/") {
        value = rest.to_owned();
    }
    while value.len() > 1 && value.ends_with('/') {
        value.pop();
    }
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn rebind_at(options: &InitOptions, path: &Path) -> Result<Value, String> {
    let old_requested = options
        .rebind_from
        .as_ref()
        .ok_or("rebind source is required")?;
    let new_root = fs::canonicalize(&options.root).map_err(|e| {
        format!(
            "canonicalize repository root {}: {e}",
            options.root.display()
        )
    })?;
    if !new_root.is_dir() {
        return Err("new repository root must be a directory".into());
    }
    let old_key = registry_key(old_requested);
    let old_canonical_key = fs::canonicalize(old_requested)
        .ok()
        .map(|path| registry_key(&path));
    let new_key = registry_key(&new_root);
    if old_key == new_key {
        return Err("rebind source and target must differ".into());
    }
    let _lock = (!options.dry_run)
        .then(|| registry_lock(path))
        .transpose()?;
    let mut registry = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| format!("registry is malformed: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("registry is missing".into())
        }
        Err(e) => return Err(format!("read registry {}: {e}", path.display())),
    };
    normalize_registry(&mut registry)?;
    let bindings = registry
        .get_mut("bindings")
        .and_then(Value::as_object_mut)
        .ok_or("registry bindings must be an object")?;
    let matches = bindings
        .keys()
        .filter(|candidate| {
            registry_key(Path::new(candidate)) == old_key
                || fs::canonicalize(candidate)
                    .ok()
                    .is_some_and(|resolved| Some(registry_key(&resolved)) == old_canonical_key)
        })
        .cloned()
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "rebind source must match exactly one registry binding (found {})",
            matches.len()
        ));
    }
    if bindings.keys().any(|candidate| {
        registry_key(Path::new(candidate)) == new_key
            || fs::canonicalize(candidate)
                .ok()
                .is_some_and(|resolved| registry_key(&resolved) == new_key)
    }) {
        return Err("rebind target already has a registry binding".into());
    }
    let source_key = matches.into_iter().next().unwrap();
    let mut binding = bindings
        .remove(&source_key)
        .ok_or("rebind source disappeared")?;
    if binding.get("repository_id").and_then(Value::as_str) != Some(options.repository_id.as_str())
        || binding.get("scope_id").and_then(Value::as_str) != Some(options.scope_id.as_str())
    {
        return Err("rebind identity arguments do not match source binding".into());
    }
    if let Some(descriptor) = binding
        .get_mut("scope_descriptor")
        .and_then(Value::as_object_mut)
    {
        let descriptor_path = descriptor
            .get("path")
            .and_then(Value::as_str)
            .map(Path::new);
        let descriptor_matches_old = descriptor_path.is_some_and(|path| {
            registry_key(path) == old_key
                || fs::canonicalize(path)
                    .ok()
                    .is_some_and(|resolved| Some(registry_key(&resolved)) == old_canonical_key)
        });
        if descriptor.get("kind").and_then(Value::as_str) == Some("filesystem")
            && descriptor_matches_old
        {
            descriptor.insert(
                "path".into(),
                Value::String(new_root.to_string_lossy().into_owned()),
            );
        }
    }
    bindings.insert(new_root.to_string_lossy().into_owned(), binding);
    let receipt = json!({"action":"rebind","source_root":source_key,"root":new_root,"registry":path,"dry_run":options.dry_run});
    if !options.dry_run {
        write_atomic(path, &registry)?;
    }
    Ok(receipt)
}

fn init_at(
    options: &InitOptions,
    path: &Path,
    injected_binding: Option<Value>,
) -> Result<Value, String> {
    let root = fs::canonicalize(&options.root).map_err(|e| {
        format!(
            "canonicalize repository root {}: {e}",
            options.root.display()
        )
    })?;
    if !root.is_dir() {
        return Err("project root must be a directory".into());
    }
    if options.repository_id.trim().is_empty() || options.scope_id.trim().is_empty() {
        return Err("repository & scope are required".into());
    }
    let descriptor = if let Some(id) = &options.virtual_id {
        if id.trim().is_empty() {
            return Err("--virtual-id is invalid".into());
        }
        let tenant = options
            .tenant_id
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| "--tenant-id is required with --virtual-id".to_string())?;
        json!({"kind":"virtual","id":id,"tenant_id":tenant,"parents":options.parents,"inherit_global":false})
    } else {
        if options.tenant_id.is_some() || !options.parents.is_empty() {
            return Err("--tenant-id/--parent require --virtual-id".into());
        }
        json!({"kind":"filesystem","path":options.scope_id})
    };
    validate_descriptor(&descriptor)?;
    let provider = {
        let mut value = Map::from_iter([(String::from("transport"), json!("loopback"))]);
        if let Some(path) = &options.config {
            value.insert("config".into(), json!({"path":path}));
        }
        if let Some(name) = &options.native {
            value.insert("native".into(), json!({"name":name}));
        }
        if !options.dry_run {
            let binding = match injected_binding.clone() {
                Some(binding) => binding,
                None => rightkit_context_runtime::service::installed_binding_projection()?,
            };
            value.insert("installation_binding".into(), binding);
        }
        Value::Object(value)
    };
    let receipt_installation_binding = provider.get("installation_binding").cloned();
    let _lock = (!options.dry_run)
        .then(|| registry_lock(path))
        .transpose()?;
    let mut registry = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|e| format!("registry is malformed: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            json!({"schema_version":2,"bindings":{}})
        }
        Err(e) => return Err(format!("read registry {}: {e}", path.display())),
    };
    normalize_registry(&mut registry)?;
    let bindings = registry
        .get_mut("bindings")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "registry bindings must be an object".to_string())?;
    let key = root.to_string_lossy().into_owned();
    #[cfg(windows)]
    let key = if let Some(unc) = key.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        key.strip_prefix(r"\\?\").unwrap_or(&key).to_string()
    };
    // JS realpath & Rust canonicalize spell Windows device paths differently.
    // Resolve existing keys before replacing so enrollment cannot fork authority
    // or lose token metadata solely because of spelling/case.
    let prior_keys = bindings
        .keys()
        .filter(|candidate| {
            *candidate == &key || fs::canonicalize(candidate).ok().as_ref() == Some(&root)
        })
        .cloned()
        .collect::<Vec<_>>();
    if prior_keys.len() > 1 {
        return Err("registry contains duplicate canonical repository roots".into());
    }
    let prior = prior_keys
        .first()
        .and_then(|prior_key| bindings.remove(prior_key));
    let prior_object = prior.as_ref().and_then(Value::as_object);
    let mut object = Map::new();
    object.insert("repository_id".into(), json!(options.repository_id));
    object.insert("scope_id".into(), json!(options.scope_id));
    object.insert("scope_descriptor".into(), descriptor);
    object.insert("provider_config".into(), provider);
    // Public Pull remains read-only, while public Push writes exact agent-authored
    // memory. Normal enrollment must therefore admit the narrow write operation.
    object.insert("grant_policy".into(), json!({"level":"write-proposed"}));
    for field in ["token_grant", "token_audit"] {
        if let Some(value) = prior_object.and_then(|prior| prior.get(field)) {
            object.insert(field.into(), value.clone());
        }
    }
    bindings.insert(key.clone(), Value::Object(object));
    let installation = json!({"precedence":"native_then_config","selected":if options.native.is_some(){"native"}else if options.config.is_some(){"config"}else{"loopback"},"native":options.native.as_ref().map(|name| json!({"name":name})),"config":options.config.as_ref().map(|path| json!({"path":path}))});
    let mut receipt = json!({"action":"enroll","root":key,"repository_id":options.repository_id,"scope_id":options.scope_id,"registry":path,"installation":installation,"dry_run":options.dry_run});
    if let Some(binding) = receipt_installation_binding {
        receipt["installation_binding"] = binding.clone();
    }
    if !options.dry_run {
        write_atomic(path, &registry)?;
    }
    Ok(receipt)
}

fn validate_descriptor(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("scope_descriptor must be an object")?;
    match object.get("kind").and_then(Value::as_str) {
        Some("filesystem")
            if object.get("path").and_then(Value::as_str).is_some()
                && object.keys().all(|k| k == "kind" || k == "path") =>
        {
            Ok(())
        }
        Some("virtual") => {
            if object
                .get("id")
                .and_then(Value::as_str)
                .is_none_or(|v| v.trim().is_empty())
                || object
                    .get("tenant_id")
                    .and_then(Value::as_str)
                    .is_none_or(|v| v.trim().is_empty())
            {
                return Err("virtual scope_descriptor identity is invalid".into());
            }
            let parents = object
                .get("parents")
                .and_then(Value::as_array)
                .ok_or("virtual scope_descriptor parents are invalid")?;
            if parents.len() > 7 {
                return Err("virtual scope_descriptor parents are invalid".into());
            }
            let mut seen = std::collections::HashSet::new();
            if parents.iter().any(|v| {
                v.as_str().is_none_or(|p| {
                    p.trim().is_empty() || p.starts_with("virtual:") || !seen.insert(p)
                })
            }) {
                return Err("virtual scope_descriptor parents are invalid".into());
            }
            if object.get("inherit_global") != Some(&Value::Bool(false))
                || object.keys().any(|k| {
                    !["kind", "id", "tenant_id", "parents", "inherit_global"].contains(&k.as_str())
                })
            {
                return Err("virtual scope_descriptor global inheritance is invalid".into());
            }
            Ok(())
        }
        _ => Err("scope_descriptor kind is invalid".into()),
    }
}

fn normalize_registry(registry: &mut Value) -> Result<(), String> {
    let version = registry
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or("schema_version must be 1 or 2")?;
    if version != 1 && version != 2 {
        return Err("schema_version must be 1 or 2".into());
    }
    if version == 1 {
        registry["schema_version"] = json!(2);
        if let Some(bindings) = registry.get_mut("bindings").and_then(Value::as_object_mut) {
            for (root, binding) in bindings.iter_mut() {
                if !binding.is_object() {
                    return Err(format!("binding {root} is malformed"));
                }
                if binding.get("scope_descriptor").is_none() {
                    let scope = binding
                        .get("scope_id")
                        .and_then(Value::as_str)
                        .unwrap_or(root);
                    binding["scope_descriptor"] = json!({"kind":"filesystem","path":scope});
                }
            }
        }
    }
    if !registry.get("bindings").is_some_and(Value::is_object) {
        return Err("registry bindings must be an object".into());
    }
    for (root, binding) in registry["bindings"].as_object().unwrap() {
        let object = binding
            .as_object()
            .ok_or_else(|| format!("binding {root} is malformed"))?;
        if object
            .get("repository_id")
            .and_then(Value::as_str)
            .is_none_or(|v| v.trim().is_empty())
            || object
                .get("scope_id")
                .and_then(Value::as_str)
                .is_none_or(|v| v.trim().is_empty())
        {
            return Err(format!("binding {root} identity is invalid"));
        }
        if let Some(descriptor) = object.get("scope_descriptor") {
            validate_descriptor(descriptor)?;
        }
    }
    Ok(())
}

fn write_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().ok_or("registry has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("create registry directory: {e}"))?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize registry: {e}"))?;
    rightkit_fs::atomic_write(path, &bytes).map_err(|e| format!("replace registry: {e}"))
}

/// Serializes registry read-modify-write across processes. The OS lock is released on exit or
/// crash, so a killed writer cannot leave the registry busy.
fn registry_lock(registry: &Path) -> Result<rightkit_fs::lock::FileLock, String> {
    let path = registry.with_extension("json.lock");
    fs::create_dir_all(registry.parent().ok_or("registry has no parent")?)
        .map_err(|e| format!("create registry directory: {e}"))?;
    rightkit_fs::lock::FileLock::acquire(&path, &rightkit_fs::lock::LockOptions::default())
        .map_err(|e| format!("registry is busy: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn schema_one_normalizes_without_dropping_token_audit() {
        let mut v = json!({"schema_version":1,"bindings":{"C:\\repo":{"repository_id":"r","scope_id":"s","token_grant":{"generation":2},"token_audit":[{"reason":"rotation"}]}}});
        normalize_registry(&mut v).unwrap();
        assert_eq!(v["schema_version"], 2);
        let key = "C:\\repo";
        assert_eq!(v["bindings"][key]["token_grant"]["generation"], 2);
        assert!(v["bindings"][key]["scope_descriptor"].is_object());
    }

    #[test]
    fn malformed_schema_one_binding_returns_error() {
        let mut registry = json!({"schema_version":1,"bindings":{"C:\\repo":42}});
        assert!(normalize_registry(&mut registry)
            .unwrap_err()
            .contains("malformed"));
    }

    #[test]
    fn enrollment_preserves_tokens_on_second_update_and_dry_run_writes_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let registry = temp.path().join("project-registry.json");
        let options = |repository_id: &str, dry_run| InitOptions {
            root: root.clone(),
            repository_id: repository_id.into(),
            scope_id: "scope".into(),
            rebind_from: None,
            virtual_id: None,
            tenant_id: None,
            parents: Vec::new(),
            native: Some("membrane".into()),
            config: None,
            dry_run,
        };
        let fixture_binding = Some(
            json!({"workspaceRoot":"fixture","db":"fixture.db","installationId":"i","serviceInstanceId":"s","stableCurrent":"fixture/current"}),
        );
        assert_eq!(
            init_at(&options("one", false), &registry, fixture_binding.clone()).unwrap()
                ["repository_id"],
            "one"
        );
        let mut stored: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        let bindings = stored["bindings"].as_object_mut().unwrap();
        let old_key = bindings.keys().next().unwrap().clone();
        let binding = bindings.remove(&old_key).unwrap();
        bindings.insert(
            fs::canonicalize(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            binding,
        );
        stored["bindings"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .next()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("token_grant".into(), json!({"generation":3}));
        fs::write(&registry, serde_json::to_vec(&stored).unwrap()).unwrap();
        init_at(&options("two", false), &registry, fixture_binding.clone()).unwrap();
        let before = fs::read(&registry).unwrap();
        init_at(&options("three", true), &registry, fixture_binding).unwrap();
        assert_eq!(fs::read(&registry).unwrap(), before);
        let final_value: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(final_value["bindings"].as_object().unwrap().len(), 1);
        let binding = final_value["bindings"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(binding["repository_id"], "two");
        assert_eq!(binding["grant_policy"]["level"], "write-proposed");
        assert_eq!(binding["token_grant"]["generation"], 3);
    }

    #[test]
    fn rebind_moves_missing_source_atomically_preserving_binding_and_dry_run() {
        let temp = tempfile::tempdir().unwrap();
        let new_root = temp.path().join("old-root");
        fs::create_dir(&new_root).unwrap();
        let old_root = temp.path().join("removed-membrane");
        let registry = temp.path().join("project-registry.json");
        let old_key = old_root.to_string_lossy().into_owned();
        let original = json!({"schema_version":2,"bindings":{old_key.clone():{
            "repository_id":"old-repository","scope_id":"old-scope",
            "scope_descriptor":{"kind":"filesystem","path":old_key},
            "provider_config":{"transport":"loopback"},"grant_policy":{"level":"write-proposed"},
            "token_grant":{"generation":7},"token_audit":[{"reason":"rotation"}]
        }}});
        fs::write(&registry, serde_json::to_vec(&original).unwrap()).unwrap();
        let options = InitOptions {
            root: new_root.clone(),
            repository_id: "old-repository".into(),
            scope_id: "old-scope".into(),
            rebind_from: Some(old_root),
            virtual_id: None,
            tenant_id: None,
            parents: Vec::new(),
            native: None,
            config: None,
            dry_run: true,
        };
        let before = fs::read(&registry).unwrap();
        assert_eq!(rebind_at(&options, &registry).unwrap()["action"], "rebind");
        assert_eq!(fs::read(&registry).unwrap(), before);
        let mut committed = options;
        committed.dry_run = false;
        rebind_at(&committed, &registry).unwrap();
        let moved: Value = serde_json::from_slice(&fs::read(&registry).unwrap()).unwrap();
        assert!(moved["bindings"].get(&old_key).is_none());
        let binding = moved["bindings"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(binding["repository_id"], "old-repository");
        assert_eq!(binding["scope_id"], "old-scope");
        assert_eq!(binding["token_grant"]["generation"], 7);
        assert_eq!(
            binding["scope_descriptor"]["path"],
            new_root.canonicalize().unwrap().to_string_lossy().as_ref()
        );
    }

    #[test]
    fn rebind_refuses_existing_canonical_target() {
        let temp = tempfile::tempdir().unwrap();
        let old_root = temp.path().join("removed");
        let new_root = temp.path().join("new");
        fs::create_dir(&new_root).unwrap();
        let registry = temp.path().join("project-registry.json");
        let old_key = old_root.to_string_lossy().into_owned();
        let target_key = new_root
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        fs::write(&registry, serde_json::to_vec(&json!({"schema_version":2,"bindings":{old_key.clone():{"repository_id":"a","scope_id":"a"},target_key:{"repository_id":"b","scope_id":"b"}}})).unwrap()).unwrap();
        let options = InitOptions {
            root: new_root,
            repository_id: "a".into(),
            scope_id: "a".into(),
            rebind_from: Some(old_root),
            virtual_id: None,
            tenant_id: None,
            parents: Vec::new(),
            native: None,
            config: None,
            dry_run: false,
        };
        assert!(rebind_at(&options, &registry)
            .unwrap_err()
            .contains("target already"));
    }

    #[test]
    fn rebind_rejects_busy_registry_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let old_root = temp.path().join("removed");
        let new_root = temp.path().join("new");
        fs::create_dir(&new_root).unwrap();
        let registry = temp.path().join("project-registry.json");
        let old_key = old_root.to_string_lossy().into_owned();
        fs::write(&registry, serde_json::to_vec(&json!({"schema_version":2,"bindings":{old_key:{"repository_id":"r","scope_id":"s"}}})).unwrap()).unwrap();
        let before = fs::read(&registry).unwrap();
        // Another writer holds the registry's OS lock (rightkit-fs; a stale lock file alone is not busy).
        let _held = registry_lock(&registry).unwrap();
        let options = InitOptions {
            root: new_root,
            repository_id: "r".into(),
            scope_id: "s".into(),
            rebind_from: Some(old_root),
            virtual_id: None,
            tenant_id: None,
            parents: Vec::new(),
            native: None,
            config: None,
            dry_run: false,
        };
        assert!(rebind_at(&options, &registry)
            .unwrap_err()
            .contains("registry is busy"));
        assert_eq!(fs::read(&registry).unwrap(), before);
    }
}
