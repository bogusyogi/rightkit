//! studio.brands: the seven brand codes (DD, RH, TS, HR, DV, KDP, NA) and their metadata,
//! ported from the Media Console baseline (a7271cf `src/lib/types/gallery.ts` `Brand`,
//! `compat-lib/media-probe.mjs` `inferProject`). Data lives in `data/brands/brands.json`;
//! Hosts that stamp a brand code on indexed artifacts embed the same file.
//!
//! Commands:
//! - `studio.brands.list`: `{brands:[{code,name,descriptor,name_verified,segments,...}], default_code}`.
//! - `studio.brands.get`: `{code}` -> one brand; `unknown_brand` for any other code.
//! - `studio.brands.infer`: `{path}` -> `{code, brand}` from the file's folder location.

use serde_json::{json, Value};

use rightkit_media_gen::{Context, GenError, GenResult};

pub const COMMANDS: &[&str] = &[
    "studio.brands.list",
    "studio.brands.get",
    "studio.brands.infer",
];

const BRANDS_JSON: &str = include_str!("../data/brands/brands.json");

fn catalog() -> &'static Value {
    static CATALOG: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| serde_json::from_str(BRANDS_JSON).expect("brands.json"))
}

/// Brand code for a file location: the first catalogue brand whose consecutive path-segment
/// sequence (or segment prefix) matches; the catalogue's `default_code` when none does.
pub fn infer_brand_code(path: &str) -> String {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    let segs: Vec<&str> = lower.split('/').filter(|s| !s.is_empty()).collect();
    let catalog = catalog();
    let default = catalog["default_code"].as_str().unwrap_or("NA");
    for brand in catalog["brands"].as_array().into_iter().flatten() {
        let code = brand["code"].as_str().unwrap_or_default();
        let by_sequence = brand["segments"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|seq| {
                let seq: Vec<&str> = seq
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                !seq.is_empty() && segs.windows(seq.len()).any(|w| w == seq.as_slice())
            });
        let by_prefix = brand["segment_prefixes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|p| segs.iter().any(|s| s.starts_with(p)));
        if by_sequence || by_prefix {
            return code.to_string();
        }
    }
    default.to_string()
}

fn find(code: &str) -> Option<Value> {
    let wanted = code.trim().to_ascii_uppercase();
    catalog()["brands"]
        .as_array()?
        .iter()
        .find(|b| b["code"] == wanted.as_str())
        .cloned()
}

pub fn handle(_ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.brands.list" => Ok(json!({
            "brands": catalog()["brands"],
            "default_code": catalog()["default_code"],
        })),
        "studio.brands.get" => {
            let code = input.get("code").and_then(Value::as_str).unwrap_or("");
            find(code).ok_or_else(|| {
                GenError::new("unknown_brand", format!("unknown brand code: {code:?}"))
            })
        }
        "studio.brands.infer" => {
            let path = input
                .get("path")
                .and_then(Value::as_str)
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| GenError::new("invalid_input", "path is required"))?;
            let code = infer_brand_code(path);
            Ok(json!({"code": code, "brand": find(&code)}))
        }
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}
