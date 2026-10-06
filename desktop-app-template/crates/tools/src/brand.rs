//! Generate `src/brand.css` (Tailwind v4 `@theme` colour variables) from `assets/brand/brand.json`.
use crate::{read_json, Result};
use serde_json::json;
use std::path::Path;

fn valid_name(name: &str) -> bool {
    let mut parts = name.split('-');
    parts.next() == Some("brand") && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase()))
}

fn valid_hex(v: &str) -> bool {
    v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit())
}

pub fn sync(root: &Path) -> Result<()> {
    let brand = read_json(root, "assets/brand/brand.json")?;
    let colors = brand["colors"].as_object().ok_or("brand.json has no colors object")?;
    let mut css = String::from("/* Generated from assets/brand/brand.json; edit that file, then run brand-sync. */\n@theme {\n");
    for (name, value) in colors {
        let value = value.as_str().unwrap_or("");
        if !valid_name(name) || !valid_hex(value) {
            return Err(format!("Invalid brand colour entry: {name}"));
        }
        css.push_str(&format!("  --color-{name}: {value};\n"));
    }
    css.push_str("}\n");
    let target = root.join("src/brand.css");
    if std::fs::read_to_string(&target).ok().as_deref() != Some(css.as_str()) {
        std::fs::write(&target, css).map_err(|e| e.to_string())?;
    }
    println!("{}", json!({"event": "brand_theme_synced", "brand": brand["code"], "colors": colors.len()}));
    Ok(())
}
