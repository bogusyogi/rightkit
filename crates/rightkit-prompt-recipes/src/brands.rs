//! The host's brands: which brand codes a request may carry and what each brand picks by default
//! from each library. The crate ships no brands; a host installs its own once at startup with
//! [`set_brands`], before the first composition.
//!
//! Defaults are keyed by library and then by brand code, with the same entry shape the libraries
//! use (`{"item_key": "..."}` for a recipe library, a library-specific spec for a video one):
//!
//! ```json
//! {
//!   "recipes/aesthetics": {"ACME": {"item_key": "premium_product"}},
//!   "recipes/models": {"ACME": {"item_key": "flux-2-pro"}},
//!   "video/camera_moves": {"ACME": {"camera_key": "slow_push_in"}}
//! }
//! ```
//!
//! Recipe library names: `aesthetics`, `composition`, `lens`, `lighting`, `depth`, `color`,
//! `skin`, `references`, `editing`, `models`. Video library names are the file stems under
//! `data/video_templates/` (`camera_moves`, `ad_patterns`, `aesthetics`, ...).
//!
//! Without brands, a request names its model and items explicitly; the brand code `NA` (no brand)
//! is always accepted and has no defaults.

use std::sync::OnceLock;

use serde_json::Value;

/// The brand code that means "no brand"; always accepted, never has defaults.
pub const NO_BRAND: &str = "NA";

/// A host's brand set.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Brands {
    /// Upper-case brand codes a request may carry (besides [`NO_BRAND`]).
    pub codes: Vec<String>,
    /// `{"recipes/<library>" | "video/<library>": {"<CODE>": <default entry>}}`.
    pub defaults: Value,
}

static BRANDS: OnceLock<Brands> = OnceLock::new();

/// Install the host's brands once. Returns the rejected value when brands are already set. Call it
/// before the first composition: each library takes its brand defaults when it is first loaded.
pub fn set_brands(brands: Brands) -> Result<(), Box<Brands>> {
    let brands = Brands {
        codes: brands
            .codes
            .iter()
            .map(|c| c.trim().to_ascii_uppercase())
            .filter(|c| !c.is_empty() && c != NO_BRAND)
            .collect(),
        defaults: brands.defaults,
    };
    BRANDS.set(brands).map_err(Box::new)
}

/// The brands in force (empty until [`set_brands`]).
pub fn brands() -> &'static Brands {
    static NONE: OnceLock<Brands> = OnceLock::new();
    BRANDS
        .get()
        .unwrap_or_else(|| NONE.get_or_init(Brands::default))
}

/// The host's brand codes, without [`NO_BRAND`].
pub fn codes() -> &'static [String] {
    &brands().codes
}

/// True for a host brand code or [`NO_BRAND`] (case-insensitive).
pub fn is_known(code: &str) -> bool {
    let code = code.trim().to_ascii_uppercase();
    code == NO_BRAND || codes().contains(&code)
}

/// The per-brand defaults of one library (`recipes/<name>` or `video/<name>`), if the host gave any.
pub fn defaults_for(library: &str) -> Option<&'static Value> {
    brands().defaults.get(library).filter(|v| v.is_object())
}

/// `json` with the host's defaults for `library` under `_brand_defaults`, the key the composers read.
pub(crate) fn with_defaults(library: &str, mut json: Value) -> Value {
    if let (Some(defaults), Some(map)) = (defaults_for(library), json.as_object_mut()) {
        map.insert("_brand_defaults".to_string(), defaults.clone());
    }
    json
}
