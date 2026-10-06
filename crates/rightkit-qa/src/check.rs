//! Assertions over a step's JSON result: a small path syntax (`a.b[0].c`,
//! `list[*].name`) plus value predicates, and `${...}` interpolation of saved
//! results into later steps.
use crate::util::{err, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Resolve a path. `[*]` fans out into an array of the remaining path's values.
pub fn select(root: &Value, path: &str) -> Option<Value> {
    let mut cur = vec![root.clone()];
    let mut fanned = false;
    for seg in split_path(path) {
        let mut next = vec![];
        for v in cur {
            match seg.as_str() {
                "[*]" => {
                    fanned = true;
                    if let Some(a) = v.as_array() {
                        next.extend(a.iter().cloned());
                    }
                }
                s if s.starts_with('[') => {
                    let i: usize = s[1..s.len() - 1].parse().ok()?;
                    next.push(v.get(i)?.clone());
                }
                key => {
                    if let Some(x) = v.get(key) {
                        next.push(x.clone());
                    } else if fanned {
                        continue;
                    } else {
                        return None;
                    }
                }
            }
        }
        cur = next;
    }
    Some(if fanned { Value::Array(cur) } else { cur.into_iter().next()? })
}

fn split_path(path: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            '[' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut idx = String::from("[");
                for d in chars.by_ref() {
                    idx.push(d);
                    if d == ']' {
                        break;
                    }
                }
                out.push(idx);
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One assertion. `path` empty/absent targets the whole result.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Expect {
    pub path: Option<String>,
    pub eq: Option<Value>,
    pub ne: Option<Value>,
    pub gte: Option<f64>,
    pub lte: Option<f64>,
    /// String contains substring, or array contains element.
    pub contains: Option<Value>,
    pub exists: Option<bool>,
    pub len_gte: Option<usize>,
    pub len_eq: Option<usize>,
    pub not_empty: Option<bool>,
}

fn to_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn declared_beyond_exists(e: &Expect) -> bool {
    e.eq.is_some() || e.ne.is_some() || e.gte.is_some() || e.lte.is_some() || e.contains.is_some() || e.len_gte.is_some() || e.len_eq.is_some() || e.not_empty.is_some()
}

pub fn check(result: &Value, e: &Expect) -> Result<()> {
    let label = e.path.clone().unwrap_or_else(|| "<result>".into());
    let got = match e.path.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => select(result, p),
        None => Some(result.clone()),
    };
    let declared = e.eq.is_some()
        || e.ne.is_some()
        || e.gte.is_some()
        || e.lte.is_some()
        || e.contains.is_some()
        || e.exists.is_some()
        || e.len_gte.is_some()
        || e.len_eq.is_some()
        || e.not_empty.is_some();
    if !declared {
        return err(format!("{label}: expectation declares no constraint (a bare path asserts nothing)"));
    }
    if let Some(want) = e.exists {
        if got.is_some() != want {
            return err(format!("{label}: exists={} expected {want}", got.is_some()));
        }
        if !want {
            // Absence was asserted; no other constraint can apply to a missing value.
            return if declared_beyond_exists(e) { err(format!("{label}: exists=false cannot be combined with value constraints")) } else { Ok(()) };
        }
    }
    let Some(got) = got else { return err(format!("{label}: path not found in {}", crate::util::tail(&result.to_string(), 400))) };
    if let Some(v) = &e.eq {
        if &got != v && !(got.is_number() && v.is_number() && got.as_f64() == v.as_f64()) {
            return err(format!("{label}: expected {v}, got {got}"));
        }
    }
    if let Some(v) = &e.ne {
        if &got == v {
            return err(format!("{label}: expected not {v}"));
        }
    }
    if let Some(min) = e.gte {
        match to_f64(&got) {
            Some(n) if n >= min => {}
            _ => return err(format!("{label}: expected >= {min}, got {got}")),
        }
    }
    if let Some(max) = e.lte {
        match to_f64(&got) {
            Some(n) if n <= max => {}
            _ => return err(format!("{label}: expected <= {max}, got {got}")),
        }
    }
    if let Some(needle) = &e.contains {
        let ok = match (&got, needle) {
            (Value::String(s), Value::String(n)) => s.contains(n.as_str()),
            (Value::Array(a), n) => a.contains(n),
            _ => false,
        };
        if !ok {
            return err(format!("{label}: expected to contain {needle}, got {}", crate::util::tail(&got.to_string(), 400)));
        }
    }
    let len = match &got {
        Value::Array(a) => Some(a.len()),
        Value::String(s) => Some(s.len()),
        Value::Object(o) => Some(o.len()),
        _ => None,
    };
    if let Some(n) = e.len_gte {
        if len.map(|l| l >= n) != Some(true) {
            return err(format!("{label}: expected length >= {n}, got {len:?}"));
        }
    }
    if let Some(n) = e.len_eq {
        if len != Some(n) {
            return err(format!("{label}: expected length {n}, got {len:?}"));
        }
    }
    if e.not_empty == Some(false) && !(len == Some(0) || got.is_null()) {
        return err(format!("{label}: expected empty"));
    }
    if e.not_empty == Some(true) && (len == Some(0) || got.is_null()) {
        return err(format!("{label}: expected non-empty"));
    }
    Ok(())
}

pub fn check_all(result: &Value, expects: &[Expect]) -> Result<()> {
    for e in expects {
        check(result, e)?;
    }
    Ok(())
}

/// Replace `${name.path}` references with saved values. A string that is exactly
/// one placeholder keeps the saved value's JSON type.
pub fn interpolate(v: &Value, vars: &BTreeMap<String, Value>) -> Result<Value> {
    Ok(match v {
        Value::String(s) => interpolate_str(s, vars)?,
        Value::Array(a) => Value::Array(a.iter().map(|x| interpolate(x, vars)).collect::<Result<_>>()?),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| Ok((k.clone(), interpolate(x, vars)?))).collect::<Result<_>>()?),
        other => other.clone(),
    })
}

fn lookup(name: &str, vars: &BTreeMap<String, Value>) -> Result<Value> {
    let (head, rest) = name.split_once('.').map(|(h, r)| (h, Some(r))).unwrap_or((name, None));
    let base = vars.get(head).ok_or_else(|| crate::util::Error(format!("unknown variable ${{{name}}}")))?;
    match rest {
        None => Ok(base.clone()),
        Some(p) => select(base, p).ok_or_else(|| crate::util::Error(format!("variable path not found: ${{{name}}}"))),
    }
}

fn interpolate_str(s: &str, vars: &BTreeMap<String, Value>) -> Result<Value> {
    if let Some(inner) = s.strip_prefix("${").and_then(|r| r.strip_suffix('}')) {
        if !inner.contains("${") {
            return lookup(inner, vars);
        }
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else { return err(format!("unterminated ${{ in {s}")) };
        match lookup(&after[..end], vars)? {
            Value::String(t) => out.push_str(&t),
            other => out.push_str(&other.to_string()),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(path: &str) -> Expect {
        Expect { path: Some(path.into()), ..Default::default() }
    }

    #[test]
    fn selects_nested_indexed_and_fanned_paths() {
        let v = json!({"json": {"contract_version": 3, "capabilities": [{"name": "generate"}, {"name": "job.wait"}]}});
        assert_eq!(select(&v, "json.contract_version"), Some(json!(3)));
        assert_eq!(select(&v, "json.capabilities[1].name"), Some(json!("job.wait")));
        assert_eq!(select(&v, "json.capabilities[*].name"), Some(json!(["generate", "job.wait"])));
        assert_eq!(select(&v, "json.nope"), None);
    }

    #[test]
    fn predicates() {
        let v = json!({"n": 3, "s": "hello world", "a": ["x", "y"], "e": []});
        assert!(check(&v, &Expect { gte: Some(2.0), ..ex("n") }).is_ok());
        assert!(check(&v, &Expect { gte: Some(4.0), ..ex("n") }).is_err());
        assert!(check(&v, &Expect { contains: Some(json!("world")), ..ex("s") }).is_ok());
        assert!(check(&v, &Expect { contains: Some(json!("y")), ..ex("a") }).is_ok());
        assert!(check(&v, &Expect { eq: Some(json!(3.0)), ..ex("n") }).is_ok());
        assert!(check(&v, &Expect { exists: Some(false), ..ex("zzz") }).is_ok());
        assert!(check(&v, &Expect { not_empty: Some(true), ..ex("e") }).is_err());
        assert!(check(&v, &Expect { len_gte: Some(2), ..ex("a") }).is_ok());
    }

    #[test]
    fn every_declared_constraint_is_evaluated() {
        let v = json!({"s": "actual", "n": 3});
        // exists=true must not short-circuit the value constraint.
        assert!(check(&v, &Expect { exists: Some(true), eq: Some(json!("expected")), ..ex("s") }).is_err());
        assert!(check(&v, &Expect { exists: Some(true), eq: Some(json!("actual")), ..ex("s") }).is_ok());
        assert!(check(&v, &Expect { exists: Some(true), gte: Some(5.0), ..ex("n") }).is_err());
        assert!(check(&v, &Expect { exists: Some(false), eq: Some(json!("x")), ..ex("zzz") }).is_err());
        assert!(check(&v, &Expect { ..ex("s") }).is_err(), "a bare path asserts nothing and must not pass");
        assert!(check(&v, &Expect { not_empty: Some(false), ..ex("s") }).is_err());
    }

    #[test]
    fn interpolation_keeps_types_for_whole_placeholders() {
        let vars = BTreeMap::from([("job".to_string(), json!({"json": {"id": "j1", "n": 4}}))]);
        assert_eq!(interpolate(&json!("${job.json.n}"), &vars).unwrap(), json!(4));
        assert_eq!(interpolate(&json!("id=${job.json.id}!"), &vars).unwrap(), json!("id=j1!"));
        assert!(interpolate(&json!("${missing}"), &vars).is_err());
        assert_eq!(interpolate(&json!({"a": ["${job.json.id}"]}), &vars).unwrap(), json!({"a": ["j1"]}));
    }
}
