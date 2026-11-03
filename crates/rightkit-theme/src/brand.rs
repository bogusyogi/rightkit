//! Brand file parsing and resolution. Port of `packages/theme/src/brand.ts`: the same `brand.json`
//! gives the same per-mode values here as in the TypeScript generator.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::color::{ensure_contrast, is_hex, luminance, mix, parse_hex, readable_on, to_hex, Rgb};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Light,
    Dark,
}

impl Mode {
    pub const ALL: [Mode; 2] = [Mode::Light, Mode::Dark];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
    }
}

/// `brand.json`. Superset of GenRight's approved file; see `@rightkit/theme`'s `BrandFile`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrandFile {
    pub name: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    pub colors: BTreeMap<String, String>,
    #[serde(default)]
    pub dark: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub tokens: Option<BrandTokens>,
    #[serde(default)]
    pub wordmark: Option<String>,
    #[serde(default)]
    pub wordmark_font: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BrandTokens {
    #[serde(default)]
    pub light: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub dark: Option<BTreeMap<String, String>>,
}

impl BrandFile {
    fn token_overrides(&self, mode: Mode) -> Option<&BTreeMap<String, String>> {
        let tokens = self.tokens.as_ref()?;
        match mode {
            Mode::Light => tokens.light.as_ref(),
            Mode::Dark => tokens.dark.as_ref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeError(pub String);

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ThemeError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ThemeError> {
    Err(ThemeError(msg.into()))
}

/// The flat suite colours `@rightkit/react-shell/shell.css` defines, per mode (`SUITE_DEFAULTS`).
pub fn suite_defaults(mode: Mode) -> BTreeMap<&'static str, &'static str> {
    let rows: &[(&str, &str)] = match mode {
        Mode::Light => &[
            ("chrome", "#EEE9DF"),
            ("plane", "#FFFCF5"),
            ("elevated", "#FFFFFF"),
            ("panel", "#FFFFFF"),
            ("sunk", "#F6F1E8"),
            ("ink", "#231F1A"),
            ("ink-2", "#6B655C"),
            ("ink-3", "#8A8378"),
            ("ok", "#1E7F3C"),
            ("warn", "#B25000"),
            ("bad", "#C4001A"),
            ("stage", "#0B0C0D"),
            ("success", "#1E7F3C"),
            ("success-soft", "rgba(30,127,60,.14)"),
            ("success-border", "rgba(30,127,60,.3)"),
            ("warning", "#B25000"),
            ("warning-bg", "rgba(178,80,0,.14)"),
            ("star", "#B8860B"),
        ],
        Mode::Dark => &[
            ("chrome", "#0F0F10"),
            ("plane", "#17171A"),
            ("elevated", "#1F1F23"),
            ("panel", "#1F1F23"),
            ("sunk", "#1C1C1F"),
            ("ink", "#F2F0EC"),
            ("ink-2", "#969390"),
            ("ink-3", "#6E6B68"),
            ("ok", "#4CD07D"),
            ("warn", "#FFB340"),
            ("bad", "#FF6961"),
            ("stage", "#0B0C0D"),
            ("success", "#16A766"),
            ("success-soft", "rgba(47,125,79,.12)"),
            ("success-border", "rgba(47,125,79,.3)"),
            ("warning", "#FFAD47"),
            ("warning-bg", "rgba(255,173,71,.16)"),
            ("star", "#E0B740"),
        ],
    };
    rows.iter().copied().collect()
}

/// The suite colour names a brand file may override (`OVERRIDABLE_TOKENS`).
pub const OVERRIDABLE_TOKENS: &[&str] = &[
    "chrome",
    "plane",
    "elevated",
    "panel",
    "sunk",
    "ink",
    "ink-2",
    "ink-3",
    "ok",
    "warn",
    "bad",
    "ok-dot",
    "warn-dot",
    "line",
    "line-strong",
    "hairline",
    "accent-ink",
    "stage",
    "success",
    "success-soft",
    "success-border",
    "warning",
    "warning-bg",
    "star",
];

/// `brand(-[a-z]+)*`
fn is_brand_key(key: &str) -> bool {
    let mut parts = key.split('-');
    parts.next() == Some("brand")
        && parts.all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase()))
}

fn is_rgb_function(value: &str) -> bool {
    let v = value.trim().to_ascii_lowercase();
    (v.starts_with("rgb(") || v.starts_with("rgba("))
        && v.ends_with(')')
        && v.matches(')').count() == 1
}

/// Parse and validate untrusted JSON text into a [`BrandFile`] (same rules as `parseBrand`).
pub fn parse_brand(json: &str) -> Result<BrandFile, ThemeError> {
    let raw: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| ThemeError(format!("brand file is not JSON: {e}")))?;
    let Some(object) = raw.as_object() else {
        return err("brand file must be a JSON object");
    };
    match object.get("name").and_then(|v| v.as_str()) {
        Some(name) if !name.trim().is_empty() => {}
        _ => return err("name: required string"),
    }
    let Some(colors) = object.get("colors").and_then(|v| v.as_object()) else {
        return err("colors: required object");
    };
    for (k, v) in colors {
        if !is_brand_key(k) {
            return err(format!("colors.{k}: names must match brand(-word)*"));
        }
        if !v.as_str().is_some_and(is_hex) {
            return err(format!("colors.{k}: expected #RRGGBB, got {v}"));
        }
    }
    if !colors.contains_key("brand") {
        return err("colors.brand: required");
    }
    if let Some(dark) = object.get("dark") {
        let Some(dark) = dark.as_object() else {
            return err("dark: expected object");
        };
        for (k, v) in dark {
            if !is_brand_key(k) || !v.as_str().is_some_and(is_hex) {
                return err(format!("dark.{k}: expected a brand-* name with #RRGGBB"));
            }
        }
    }
    if let Some(tokens) = object.get("tokens") {
        let Some(tokens) = tokens.as_object() else {
            return err("tokens: expected object");
        };
        for (mode, map) in tokens {
            if mode != "light" && mode != "dark" {
                return err(format!("tokens.{mode}: only light and dark are allowed"));
            }
            let Some(map) = map.as_object() else {
                return err(format!("tokens.{mode}: expected object"));
            };
            for (k, v) in map {
                if !OVERRIDABLE_TOKENS.contains(&k.as_str()) {
                    return err(format!("tokens.{mode}.{k}: unknown name"));
                }
                if !v.as_str().is_some_and(|s| is_hex(s) || is_rgb_function(s)) {
                    return err(format!("tokens.{mode}.{k}: expected a hex or rgb() colour"));
                }
            }
        }
    }
    if object.get("wordmark").is_some_and(|v| !v.is_string()) {
        return err("wordmark: expected a string");
    }
    if let Some(font) = object.get("wordmarkFont") {
        match font.as_str() {
            Some(s) if !s.contains([';', '{', '}']) => {}
            _ => return err("wordmarkFont: expected a plain font-family string"),
        }
    }
    serde_json::from_value(raw).map_err(|e| ThemeError(format!("brand file: {e}")))
}

/// Per-mode values of a resolved brand.
#[derive(Debug, Clone)]
pub struct ResolvedBrand {
    /// Flat suite colours that differ from the defaults, per mode.
    pub overrides: BTreeMap<Mode, BTreeMap<String, String>>,
    /// Defaults merged with overrides: every suite colour per mode.
    pub effective: BTreeMap<Mode, BTreeMap<String, String>>,
    /// Brand palette per mode.
    pub palette: BTreeMap<Mode, BTreeMap<String, String>>,
}

fn hex(value: &str) -> Result<Rgb, ThemeError> {
    parse_hex(value).ok_or_else(|| ThemeError(format!("invalid hex colour: {value}")))
}

/// The text surface (plane or elevated) on which accent text has the least contrast in `mode`.
fn worst_surface(brand: &BrandFile, mode: Mode) -> Rgb {
    let defaults = suite_defaults(mode);
    let overrides = brand.token_overrides(mode);
    let pick = |name: &str| -> Rgb {
        let fallback = parse_hex(defaults[name]).expect("suite defaults are hex");
        overrides
            .and_then(|m| m.get(name))
            .and_then(|v| parse_hex(v))
            .unwrap_or(fallback)
    };
    let (plane, elevated) = (pick("plane"), pick("elevated"));
    // dark: the lighter surface; light: the darker one; the first wins a tie.
    let take_second = match mode {
        Mode::Dark => luminance(elevated) > luminance(plane),
        Mode::Light => luminance(elevated) < luminance(plane),
    };
    if take_second {
        elevated
    } else {
        plane
    }
}

/// Resolve a brand file into per-mode values. Light accent = `brand-action` else `brand`. Dark accent =
/// `dark["brand-action"]`, else `dark.brand`, else the brand colour lifted to 4.5:1 on the dark plane (a
/// brand that already passes is kept). `accent-ink` is the better of white and near-black on the accent.
pub fn resolve_brand(brand: &BrandFile) -> Result<ResolvedBrand, ThemeError> {
    let c = &brand.colors;
    let brand_color = c
        .get("brand")
        .ok_or_else(|| ThemeError("colors.brand: required".into()))?;
    let light_accent = to_hex(hex(c.get("brand-action").unwrap_or(brand_color))?);
    let light_text = match c.get("brand-ink") {
        Some(v) => to_hex(hex(v)?),
        None => to_hex(ensure_contrast(
            hex(&light_accent)?,
            worst_surface(brand, Mode::Light),
            4.5,
        )),
    };
    let dark_plane = worst_surface(brand, Mode::Dark);
    let dark = brand.dark.as_ref();
    let dark_get = |key: &str| dark.and_then(|d| d.get(key));
    let dark_brand = match dark_get("brand") {
        Some(v) => v.clone(),
        None => to_hex(ensure_contrast(hex(brand_color)?, dark_plane, 4.5)),
    };
    let dark_accent = dark_get("brand-action")
        .cloned()
        .unwrap_or_else(|| dark_brand.clone());
    let dark_text = match dark_get("brand-ink") {
        Some(v) => v.clone(),
        None => to_hex(ensure_contrast(hex(&dark_accent)?, dark_plane, 4.5)),
    };

    let mut light_palette = c.clone();
    light_palette.insert("brand".into(), to_hex(hex(brand_color)?));
    light_palette.insert("brand-action".into(), light_accent);
    light_palette.insert("brand-ink".into(), light_text);

    let mut dark_palette = c.clone();
    if let Some(d) = dark {
        for (k, v) in d {
            dark_palette.insert(k.clone(), v.clone());
        }
    }
    let dark_accent_rgb = hex(&dark_accent)?;
    dark_palette.insert("brand".into(), to_hex(hex(&dark_brand)?));
    dark_palette.insert("brand-action".into(), to_hex(dark_accent_rgb));
    dark_palette.insert("brand-ink".into(), to_hex(hex(&dark_text)?));
    dark_palette.insert(
        "brand-soft".into(),
        dark_get("brand-soft")
            .cloned()
            .unwrap_or_else(|| to_hex(mix(dark_plane, dark_accent_rgb, 0.18))),
    );
    dark_palette.insert(
        "brand-border".into(),
        dark_get("brand-border")
            .cloned()
            .unwrap_or_else(|| to_hex(mix(dark_plane, dark_accent_rgb, 0.4))),
    );
    // The dark palette never inherits the light-only hover colour by accident.
    if dark_get("brand-action-hover").is_none() {
        dark_palette.remove("brand-action-hover");
    }

    let palette = BTreeMap::from([(Mode::Light, light_palette), (Mode::Dark, dark_palette)]);
    let mut overrides = BTreeMap::new();
    let mut effective = BTreeMap::new();
    for mode in Mode::ALL {
        let action = hex(&palette[&mode]["brand-action"])?;
        let mut over = BTreeMap::from([("accent-ink".to_owned(), to_hex(readable_on(action)))]);
        if let Some(given) = brand.token_overrides(mode) {
            for (k, v) in given {
                over.insert(k.clone(), v.clone());
            }
        }
        let mut eff: BTreeMap<String, String> = suite_defaults(mode)
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        for (k, v) in &over {
            eff.insert(k.clone(), v.clone());
        }
        overrides.insert(mode, over);
        effective.insert(mode, eff);
    }
    Ok(ResolvedBrand {
        overrides,
        effective,
        palette,
    })
}
