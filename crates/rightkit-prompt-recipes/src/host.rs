//! Host options: the few places where a host's shipped behaviour differs from the crate's
//! defaults, installed once per process with [`set_options`] (first call wins), before the first
//! composition. Every field defaults to the crate's own behaviour, so a host that never calls
//! [`set_options`] is unchanged. The crate carries no product names: the host passes its own.
//!
//! | field | default | what it changes |
//! | --- | --- | --- |
//! | `app_name` | none (the messages say "This app" / "the app") | the app's name in two user-visible character messages |
//! | `training_approval_prefix` | `rightkit-train-v1` | the first segment of the text hashed into a character-training plan id, so the approval basis (and every token minted over it) is the host's |
//! | `content_root_env` | `RIGHTKIT_CONTENT_ROOT` | the environment variable the video templates read their content root from |
//! | `preflight_source` | `video_preflight` | the `source` value of the video preflight meta (stored in the draft and bound by its approval) |
//! | `video_brand_order` | the order of [`crate::brands::set_brands`] | the order the video brand error lists the brand codes in (the set accepted is still the brands') |
//! | `omit_node_depends_on` | `false` | leave `depends_on` out of each node status the recipe catalogue reports |
//! | `overlay` | none | host JSON merged over the embedded recipe and video libraries |
//!
//! # Overlay
//!
//! `overlay` is a JSON object keyed by library, `recipes/<library>` or `video/<library>` (the same
//! names [`crate::brands`] uses), each value a patch merged over that library's embedded JSON:
//!
//! - objects merge by key, recursively: a key in the patch replaces or extends the same key of the
//!   library; keys the patch does not name are kept, in their place;
//! - arrays and scalars replace the library's value whole;
//! - `null` deletes the key (a `null` inside a new object is dropped);
//! - a patch for a library the crate does not have is refused.
//!
//! The merged library is validated before anything is installed: it must stay a JSON object, its
//! `items` (or each category's `items`) must stay objects of objects, and the built-in video shot
//! contract templates must still pass [`crate::video_templates::validate_shot_contract`]. A
//! refused overlay leaves nothing installed. Per-brand defaults still come from
//! [`crate::brands::set_brands`] (`_brand_defaults`), which overrides a `_brand_defaults` key an
//! overlay might carry. Keys an overlay adds list after the embedded ones.
//!
//! The overlay applies when a library is first loaded, so [`set_options`] refuses an overlay once
//! any library has loaded ([`HostOptionsError::LibrariesInUse`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use serde_json::{Map, Value};

/// The default first segment of the character-training approval basis.
pub const DEFAULT_TRAINING_APPROVAL_PREFIX: &str = "rightkit-train-v1";
/// The default environment variable of the video templates' content root.
pub const DEFAULT_CONTENT_ROOT_ENV: &str = "RIGHTKIT_CONTENT_ROOT";
/// The default `source` of the video preflight meta.
pub const DEFAULT_PREFLIGHT_SOURCE: &str = "video_preflight";

/// What a host changes; see the [module docs](self). `HostOptions::default()` is today's behaviour.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HostOptions {
    /// The app's display name, e.g. `Acme`: the character messages then read "Acme does not
    /// upload LoRA weights" and "Only the Acme app can approve it".
    pub app_name: Option<String>,
    /// First segment of the text hashed into a training plan id (default
    /// [`DEFAULT_TRAINING_APPROVAL_PREFIX`]).
    pub training_approval_prefix: Option<String>,
    /// Environment variable read for the video content root (default
    /// [`DEFAULT_CONTENT_ROOT_ENV`]).
    pub content_root_env: Option<String>,
    /// `source` of the video preflight meta (default [`DEFAULT_PREFLIGHT_SOURCE`]).
    pub preflight_source: Option<String>,
    /// The order the video preflight's "brand must be one of: ..." message lists the host's brand
    /// codes in (default: the order they were given to [`crate::brands::set_brands`]). Codes it
    /// does not name follow in their own order; names that are not brand codes are ignored.
    pub video_brand_order: Option<Vec<String>>,
    /// Leave `depends_on` out of each node status of the recipe catalogue (default: reported).
    pub omit_node_depends_on: bool,
    /// Library patches, `{"recipes/<library>" | "video/<library>": <patch>}`.
    pub overlay: Option<Value>,
}

/// Why [`set_options`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostOptionsError {
    /// Options are already installed (first call wins).
    AlreadySet,
    /// An overlay was given after a library had loaded; install the options first.
    LibrariesInUse,
    /// The overlay is malformed, names an unknown library or fails validation.
    InvalidOverlay(String),
}

impl std::fmt::Display for HostOptionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadySet => f.write_str("host options are already set"),
            Self::LibrariesInUse => {
                f.write_str("a library is already loaded; set the overlay before first use")
            }
            Self::InvalidOverlay(m) => write!(f, "invalid overlay: {m}"),
        }
    }
}

impl std::error::Error for HostOptionsError {}

struct Installed {
    options: HostOptions,
    merged: HashMap<String, Value>,
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();
static LIBRARIES_LOADED: AtomicBool = AtomicBool::new(false);

/// Install the host's options once. A refused call installs nothing, so the host may correct the
/// options and call again; once a call succeeds, later calls get [`HostOptionsError::AlreadySet`].
pub fn set_options(options: HostOptions) -> Result<(), HostOptionsError> {
    if INSTALLED.get().is_some() {
        return Err(HostOptionsError::AlreadySet);
    }
    let mut merged = HashMap::new();
    if let Some(overlay) = options.overlay.as_ref().filter(|o| !is_empty_object(o)) {
        if LIBRARIES_LOADED.load(Ordering::SeqCst) {
            return Err(HostOptionsError::LibrariesInUse);
        }
        merged = merge_overlay(overlay).map_err(HostOptionsError::InvalidOverlay)?;
    }
    INSTALLED
        .set(Installed { options, merged })
        .map_err(|_| HostOptionsError::AlreadySet)
}

fn is_empty_object(v: &Value) -> bool {
    v.as_object().is_some_and(Map::is_empty)
}

fn options() -> Option<&'static HostOptions> {
    INSTALLED.get().map(|i| &i.options)
}

fn text(v: Option<&'static String>, default: &'static str) -> &'static str {
    v.map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(default)
}

/// The subject of "does not upload LoRA weights": the app's name, else "This app".
#[cfg_attr(not(feature = "commands"), allow(dead_code))]
pub(crate) fn app_subject() -> String {
    match options()
        .and_then(|o| o.app_name.as_deref())
        .filter(|n| !n.trim().is_empty())
    {
        Some(name) => name.to_string(),
        None => "This app".to_string(),
    }
}

/// "the app", or "the <name> app" when the host named it.
#[cfg_attr(not(feature = "commands"), allow(dead_code))]
pub(crate) fn the_app() -> String {
    match options()
        .and_then(|o| o.app_name.as_deref())
        .filter(|n| !n.trim().is_empty())
    {
        Some(name) => format!("the {name} app"),
        None => "the app".to_string(),
    }
}

/// The training approval basis' first segment.
#[cfg_attr(not(feature = "commands"), allow(dead_code))]
pub(crate) fn training_approval_prefix() -> &'static str {
    text(
        options().and_then(|o| o.training_approval_prefix.as_ref()),
        DEFAULT_TRAINING_APPROVAL_PREFIX,
    )
}

/// The environment variable of the video content root.
#[cfg_attr(not(feature = "commands"), allow(dead_code))]
pub(crate) fn content_root_env() -> &'static str {
    text(
        options().and_then(|o| o.content_root_env.as_ref()),
        DEFAULT_CONTENT_ROOT_ENV,
    )
}

/// The `source` of the video preflight meta.
#[cfg_attr(not(feature = "commands"), allow(dead_code))]
pub(crate) fn preflight_source() -> &'static str {
    text(
        options().and_then(|o| o.preflight_source.as_ref()),
        DEFAULT_PREFLIGHT_SOURCE,
    )
}

/// The host's brand codes in the order the video brand message lists them.
#[cfg_attr(not(feature = "commands"), allow(dead_code))]
pub(crate) fn video_brand_codes() -> Vec<String> {
    let codes = crate::brands::codes();
    let Some(order) = options().and_then(|o| o.video_brand_order.as_ref()) else {
        return codes.to_vec();
    };
    let mut out: Vec<String> = order
        .iter()
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| codes.contains(c))
        .collect();
    out.dedup();
    for c in codes {
        if !out.contains(c) {
            out.push(c.clone());
        }
    }
    out
}

/// Whether the recipe catalogue reports `depends_on` on each node status.
#[cfg_attr(not(feature = "commands"), allow(dead_code))]
pub(crate) fn report_node_depends_on() -> bool {
    !options().is_some_and(|o| o.omit_node_depends_on)
}

/// The library `key` (`recipes/<name>` or `video/<name>`) as a loader should hold it: the
/// overlaid JSON when the host gave a patch for it, else `parse` of the embedded `src`. Marks
/// libraries as in use.
pub(crate) fn load_library(key: &str, parse: impl FnOnce() -> Value) -> Value {
    LIBRARIES_LOADED.store(true, Ordering::SeqCst);
    match INSTALLED.get().and_then(|i| i.merged.get(key)) {
        Some(v) => v.clone(),
        None => parse(),
    }
}

fn embedded(key: &str) -> Option<Value> {
    let (area, name) = key.split_once('/')?;
    let src = match area {
        "recipes" => crate::recipes::embedded_source(name),
        "video" => crate::video_templates::embedded_source(name),
        _ => None,
    }?;
    serde_json::from_str(src).ok()
}

fn merge_overlay(overlay: &Value) -> Result<HashMap<String, Value>, String> {
    let patches = overlay
        .as_object()
        .ok_or("the overlay must be a JSON object keyed by library")?;
    let mut out = HashMap::new();
    for (key, patch) in patches {
        let mut lib = embedded(key).ok_or_else(|| {
            format!("{key:?} is not a library (use recipes/<library> or video/<library>)")
        })?;
        if !patch.is_object() {
            return Err(format!("{key}: the patch must be a JSON object"));
        }
        merge(&mut lib, patch);
        validate(key, &lib).map_err(|m| format!("{key}: {m}"))?;
        out.insert(key.clone(), lib);
    }
    Ok(out)
}

/// `patch` over `base`: objects by key, everything else replaced whole, `null` deletes.
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    b.shift_remove(k);
                } else if let Some(existing) = b.get_mut(k) {
                    merge(existing, v);
                } else {
                    b.insert(k.clone(), without_nulls(v));
                }
            }
        }
        (b, p) => *b = without_nulls(p),
    }
}

fn without_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(_, x)| !x.is_null())
                .map(|(k, x)| (k.clone(), without_nulls(x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn items_of(v: &Value, what: &str) -> Result<(), String> {
    let items = v
        .as_object()
        .ok_or_else(|| format!("{what} must be an object of items"))?;
    match items.iter().find(|(_, i)| !i.is_object()) {
        Some((k, _)) => Err(format!("{what}.{k} must be an object")),
        None => Ok(()),
    }
}

fn validate(key: &str, lib: &Value) -> Result<(), String> {
    let root = lib.as_object().ok_or("the library must stay an object")?;
    if let Some(items) = root.get("items") {
        items_of(items, "items")?;
    }
    if let Some(cats) = root.get("categories") {
        let cats = cats.as_object().ok_or("categories must stay an object")?;
        for (ck, cat) in cats {
            let cat = cat
                .as_object()
                .ok_or_else(|| format!("categories.{ck} must be an object"))?;
            if let Some(items) = cat.get("items") {
                items_of(items, &format!("categories.{ck}.items"))?;
            }
        }
    }
    if key == "video/shot_contract_templates" {
        crate::video_templates::validate_builtin_templates(lib)?;
    }
    Ok(())
}
