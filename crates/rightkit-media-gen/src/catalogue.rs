//! Catalogue of declared models: provider models described by data instead of
//! Rust tables. The catalogue is the union of an embedded default file and an
//! optional user file, loaded once per process; the fal and WaveSpeed adapters
//! validate requests against an entry, build the provider body from its
//! `inputs`, and price it from its `pricing`. See `docs/CATALOGUE.md`.

use crate::providers::{Durations, GenRequest, Kind, ModelInfo, ProviderError, ReferenceInput};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use time::{Date, Month, OffsetDateTime};

const EMBEDDED: &str = include_str!("../data/catalogue/models.json");

pub const SCHEMA_VERSION: u32 = 1;
/// A price older than this many days is unqualified for routing.
pub const STALE_AFTER_DAYS: i64 = 60;
/// Providers whose adapters accept declared models in this stage.
pub const PROVIDERS: [&str; 2] = ["fal", "wavespeed"];
/// Largest `max_bytes` a `json` input may declare.
pub const MAX_JSON_BYTES: usize = 65536;
/// Tolerance when rounding billed seconds up to a multiple, so `0.6 / 0.2`
/// computed as `2.9999999999999996` or `3.0000000000000004` bills 3 steps, not 4.
const ROUND_EPSILON: f64 = 1e-9;
/// Step a derived megapixel price is rounded up to when the entry gives none.
pub const DEFAULT_ROUND_UP_MP: f64 = 0.01;
/// Characters per billed unit of a `per_character` price.
pub const CHARS_PER_UNIT: f64 = 1000.0;
/// Characters assumed for the advertised `est_usd` of a `per_character` price when the entry gives none.
const DEFAULT_EST_CHARS: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    String,
    Integer,
    Number,
    Boolean,
    Url,
    UrlList,
    /// A duration the provider takes as a string (`"5"`, `"10"`, `"auto"`).
    EnumSeconds,
    /// An opaque object or array passed through unchanged.
    Json,
}

impl InputType {
    fn name(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Url => "url",
            Self::UrlList => "url_list",
            Self::EnumSeconds => "enum_seconds",
            Self::Json => "json",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Start,
    End,
    Reference,
    Video,
    Audio,
    Mask,
}

impl Role {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
            Self::Reference => "reference",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Mask => "mask",
        }
    }
}

/// One request input: where it comes from in the request and where it goes in
/// the provider body.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// Key in the request `params`, or the literal `prompt` (the request prompt).
    pub param: String,
    /// Key sent to the provider. Required when the input is sent; ignored when
    /// `send` is false.
    #[serde(default)]
    pub json_key: String,
    #[serde(rename = "type")]
    pub ty: InputType,
    #[serde(default)]
    pub role: Option<Role>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default, rename = "enum")]
    pub enum_values: Option<Vec<Value>>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub max_items: Option<usize>,
    /// Alternative `params` names for the same input.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// False: the input is validated and may be read by pricing but is NOT
    /// sent to the provider.
    #[serde(default = "yes")]
    pub send: bool,
    /// `enum_seconds` only: billed length of an enum value that is not a number.
    #[serde(default)]
    pub seconds_for: BTreeMap<String, f64>,
    /// `json` only: largest serialized size in bytes (1 to [`MAX_JSON_BYTES`]).
    #[serde(default)]
    pub max_bytes: Option<usize>,
    /// `string` only: longest accepted value, counted in Unicode scalar values
    /// (`str::chars().count()`), checked before submit.
    #[serde(default)]
    pub max_len: Option<usize>,
    /// Sent inside the JSON object of this name (`body[nest][json_key]`) instead of at the
    /// top level of the body, for providers that take `{"image_size": {"width": .., "height": ..}}`.
    #[serde(default)]
    pub nest: Option<String>,
}

/// A `'static` copy of a param name, one per distinct name for the life of the process.
fn intern(s: &str) -> &'static str {
    static NAMES: std::sync::Mutex<BTreeSet<&'static str>> = std::sync::Mutex::new(BTreeSet::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(known) = names.get(s) {
        return known;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    names.insert(leaked);
    leaked
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceBy {
    pub param: String,
    pub values: BTreeMap<String, f64>,
}

/// The condition of an [`Adjust`]: exactly one of `equals` and `present`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    pub param: String,
    #[serde(default)]
    pub equals: Option<Value>,
    #[serde(default)]
    pub present: Option<bool>,
}

/// One conditional change to a computed amount: exactly one of `multiply`
/// and `add_usd`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adjust {
    pub when: When,
    #[serde(default)]
    pub multiply: Option<f64>,
    #[serde(default)]
    pub add_usd: Option<f64>,
}

/// Output-size source of a derived `per_megapixel` price. Either way the size is read from inputs
/// the provider receives, so the price always follows what is sent.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case", deny_unknown_fields)]
pub enum SizeSource {
    /// A sent string input with an `enum`; `sizes` maps every enum value to `[width, height]` pixels.
    Enum {
        param: String,
        sizes: BTreeMap<String, [u32; 2]>,
    },
    /// Two sent integer inputs holding pixels, each with a declared `min` (at least 1) and `max`.
    WidthHeight {
        width_param: String,
        height_param: String,
    },
}

/// Whether the provider states how it rounds billed megapixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rounding {
    /// The provider's page states the rounding rule.
    Stated,
    /// The page does not; the entry's `round_up_to_mp` is an assumption to check against a live charge.
    Assumed,
}

impl Rounding {
    fn name(self) -> &'static str {
        match self {
            Self::Stated => "stated",
            Self::Assumed => "assumed",
        }
    }
}

/// Where a reserved cap on billable seconds comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapBasis {
    /// A limit the provider documents (the entry's text or the provider's page). A measured source
    /// longer than a stated cap is refused before submit, because the provider would refuse it.
    Stated,
    /// A conservative figure the entry sets because the provider's maximum is unknown to the repo.
    /// A measured source is priced at its real length, however long; only an unmeasured one reserves the cap.
    Assumed,
}

impl CapBasis {
    fn name(self) -> &'static str {
        match self {
            Self::Stated => "stated",
            Self::Assumed => "assumed",
        }
    }
}

/// A `per_second` or `per_minute` price whose billed seconds the ENGINE derives from the source
/// media the entry sends (`source_param`, a required sent `url` input with role video or audio),
/// never from a number the caller states. A source the engine has measured (a probe of its bytes)
/// bills its real length, rounded UP to `round_up_to_s` (default 1 s). An https URL the engine
/// cannot measure before submit reserves `reserve_cap_seconds`, and a request for an entry with no
/// cap is refused. The entry's `seconds_param` input is only a hint: it is never priced, and when a
/// measurement exists a hint more than [`HINT_TOLERANCE_S`] off it is refused.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedSeconds {
    pub source_param: String,
    #[serde(default)]
    pub reserve_cap_seconds: Option<f64>,
    /// Required with `reserve_cap_seconds`, so a reader of `media modalities` always sees where the cap comes from.
    #[serde(default)]
    pub reserve_cap_basis: Option<CapBasis>,
}

/// How far a caller's `seconds_param` hint may differ from the engine's measurement of the source.
pub const HINT_TOLERANCE_S: f64 = 1.0;

/// Megapixels the provider bills per input image, added for every URL in a sent `url_list` input.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputImages {
    pub param: String,
    pub megapixels_each: f64,
}

/// A `per_megapixel` price whose megapixels the engine derives from sent inputs instead of
/// a number the caller states: output megapixels from `size`, rounded UP to `round_up_to_mp`
/// (default [`DEFAULT_ROUND_UP_MP`]), plus `input_images`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DerivedMegapixels {
    pub size: SizeSource,
    #[serde(default)]
    pub round_up_to_mp: Option<f64>,
    /// Required, so a reader of `media modalities` always sees whether the rounding is known.
    pub rounding: Rounding,
    #[serde(default)]
    pub input_images: Option<InputImages>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pricing {
    Flat {
        usd: f64,
        #[serde(default)]
        adjust: Vec<Adjust>,
    },
    PerSecond {
        usd: f64,
        seconds_param: String,
        #[serde(default)]
        by: Option<PriceBy>,
        #[serde(default)]
        min_billed_s: Option<f64>,
        #[serde(default)]
        max_billed_s: Option<f64>,
        #[serde(default)]
        round_up_to_s: Option<f64>,
        #[serde(default)]
        derived: Option<DerivedSeconds>,
        #[serde(default)]
        adjust: Vec<Adjust>,
    },
    PerMinute {
        usd: f64,
        seconds_param: String,
        #[serde(default)]
        by: Option<PriceBy>,
        #[serde(default)]
        min_billed_s: Option<f64>,
        #[serde(default)]
        max_billed_s: Option<f64>,
        #[serde(default)]
        round_up_to_s: Option<f64>,
        #[serde(default)]
        derived: Option<DerivedSeconds>,
        #[serde(default)]
        adjust: Vec<Adjust>,
    },
    PerItem {
        usd: f64,
        count_param: String,
        #[serde(default)]
        adjust: Vec<Adjust>,
    },
    /// `usd` per 1000 characters of a sent string input (`text_param`, possibly `prompt`),
    /// counted in Unicode scalar values. The count is read from the value that is sent, never stated.
    PerCharacter {
        usd: f64,
        text_param: String,
        /// Characters assumed for the advertised `est_usd` only (default 1000, at most the input's `max_len`).
        #[serde(default)]
        est_chars: Option<u32>,
        #[serde(default)]
        adjust: Vec<Adjust>,
    },
    /// Flat form: `usd` with `megapixels_param` or a `megapixels` constant.
    /// Tiered form: `first_usd` and `additional_usd` with `megapixels_param`.
    /// Derived form: either rate form with `derived` in place of `megapixels_param`.
    PerMegapixel {
        #[serde(default)]
        usd: Option<f64>,
        #[serde(default)]
        first_usd: Option<f64>,
        #[serde(default)]
        additional_usd: Option<f64>,
        #[serde(default)]
        megapixels_param: Option<String>,
        #[serde(default)]
        megapixels: Option<f64>,
        #[serde(default)]
        derived: Option<DerivedMegapixels>,
        #[serde(default)]
        adjust: Vec<Adjust>,
    },
}

/// Borrowed view of the fields `per_second` and `per_minute` share.
struct Timed<'a> {
    usd: f64,
    seconds_param: &'a str,
    by: Option<&'a PriceBy>,
    min_billed_s: Option<f64>,
    max_billed_s: Option<f64>,
    round_up_to_s: Option<f64>,
    derived: Option<&'a DerivedSeconds>,
    per_minute: bool,
}

impl Pricing {
    fn name(&self) -> &'static str {
        match self {
            Self::Flat { .. } => "flat",
            Self::PerSecond { .. } => "per_second",
            Self::PerMinute { .. } => "per_minute",
            Self::PerItem { .. } => "per_item",
            Self::PerCharacter { .. } => "per_character",
            Self::PerMegapixel { .. } => "per_megapixel",
        }
    }

    fn timed(&self) -> Option<Timed<'_>> {
        match self {
            Self::PerSecond {
                usd,
                seconds_param,
                by,
                min_billed_s,
                max_billed_s,
                round_up_to_s,
                derived,
                ..
            } => Some(Timed {
                usd: *usd,
                seconds_param: seconds_param.as_str(),
                by: by.as_ref(),
                min_billed_s: *min_billed_s,
                max_billed_s: *max_billed_s,
                round_up_to_s: *round_up_to_s,
                derived: derived.as_ref(),
                per_minute: false,
            }),
            Self::PerMinute {
                usd,
                seconds_param,
                by,
                min_billed_s,
                max_billed_s,
                round_up_to_s,
                derived,
                ..
            } => Some(Timed {
                usd: *usd,
                seconds_param: seconds_param.as_str(),
                by: by.as_ref(),
                min_billed_s: *min_billed_s,
                max_billed_s: *max_billed_s,
                round_up_to_s: *round_up_to_s,
                derived: derived.as_ref(),
                per_minute: true,
            }),
            _ => None,
        }
    }

    fn adjust(&self) -> &[Adjust] {
        match self {
            Self::Flat { adjust, .. }
            | Self::PerSecond { adjust, .. }
            | Self::PerMinute { adjust, .. }
            | Self::PerItem { adjust, .. }
            | Self::PerCharacter { adjust, .. }
            | Self::PerMegapixel { adjust, .. } => adjust,
        }
    }
}

/// Output durations advertised for the model (same fields `ModelInfo` exposes).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurationSpec {
    #[serde(default)]
    pub durations_s: Option<Vec<u32>>,
    #[serde(default)]
    pub min_duration_s: Option<u32>,
    #[serde(default)]
    pub max_duration_s: Option<u32>,
    #[serde(default)]
    pub default_duration_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub provider: String,
    pub model: String,
    pub kind: Kind,
    pub label: String,
    /// Path after the provider base: the fal queue endpoint, or the WaveSpeed
    /// slug after `/api/v3/`.
    pub endpoint: String,
    pub inputs: Vec<Input>,
    #[serde(default)]
    pub constants: Map<String, Value>,
    pub pricing: Pricing,
    /// ISO date the price was read from `price_source`.
    pub priced_at: String,
    pub price_source: String,
    #[serde(default)]
    pub durations: Option<DurationSpec>,
    /// `false`: the price and schema were read from the provider's pages and no live charge has
    /// confirmed them. Absent on entries that predate the field. Reported by `media modalities`.
    #[serde(default)]
    pub reconciled: Option<bool>,
    /// Body keys the provider's schema documents beyond `inputs`, which a request may supply in
    /// `params.provider_params` (camera presets: Kling `camera_control`, Seedance `camera_fixed`).
    /// Any other key in `provider_params` is dropped with a warning. Empty: the model documents none.
    #[serde(default)]
    pub provider_params: Vec<String>,
}

/// A request checked against an entry: the provider body and the value of every
/// input after defaults, keyed by the input's `param`.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub body: Value,
    pub values: BTreeMap<String, Value>,
}

pub fn today() -> Date {
    OffsetDateTime::now_utc().date()
}

fn parse_date(s: &str) -> Option<Date> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    if !b
        .iter()
        .enumerate()
        .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return None;
    }
    let (y, m, d) = (
        s[0..4].parse().ok()?,
        s[5..7].parse::<u8>().ok()?,
        s[8..10].parse().ok()?,
    );
    Date::from_calendar_date(y, Month::try_from(m).ok()?, d).ok()
}

/// A url-type input value: an https URL or a local file path (uploaded to the user's own fal storage per run).
fn is_url_or_local(s: &str) -> bool {
    is_https(s)
        || is_dev_storage_url(s)
        || (crate::fal_storage::local_path(s).is_some() && !s.contains(' ') && s.trim() == s)
}

/// A URL under the development-only fal REST base override (the e2e mock's upload target). Uploaded
/// files are rewritten to such URLs before submit validates them again; in a release build the
/// override is ignored, so this never matches there.
fn is_dev_storage_url(s: &str) -> bool {
    if !crate::providers::PROVIDER_BASE_OVERRIDES || s.chars().any(char::is_whitespace) {
        return false;
    }
    let base = crate::fal_storage::rest_base();
    base != "https://rest.alpha.fal.ai" && s.starts_with(&format!("{base}/"))
}

fn is_https(s: &str) -> bool {
    s.len() > "https://".len() && s.starts_with("https://") && !s.chars().any(char::is_whitespace)
}

/// Numbers compare by value (`5` equals `5.0`); everything else structurally.
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// An amount or length that pricing may use: finite and at least 0.
fn money(field: &str, v: f64) -> Result<(), String> {
    if v.is_finite() && v >= 0.0 {
        Ok(())
    } else {
        Err(format!(
            "field `{field}`: must be a finite amount of at least 0"
        ))
    }
}

/// What a pricing field reads from an input.
#[derive(Clone, Copy)]
enum Use {
    Count,
    Seconds,
    Megapixels,
    Selector,
}

fn valid_endpoint(e: &str) -> bool {
    !e.is_empty()
        && !e.starts_with('/')
        && !e.contains("..")
        && e.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'/' | b'-'))
        && e.split('/').all(|seg| !seg.is_empty() && seg != ".")
}

/// Load-time rules of an `enum_seconds` input: a non-empty enum of unique
/// strings, each billed as a finite number of seconds of at least 0, either
/// itself (numeric strings) or through `seconds_for` (any other string).
fn validate_enum_seconds(i: &Input, at: &dyn Fn(&str) -> String) -> Result<(), String> {
    let Some(e) = &i.enum_values else {
        return Err(format!(
            "{}: an enum_seconds input needs an enum of strings",
            at("enum")
        ));
    };
    if e.is_empty() {
        return Err(format!("{}: must not be empty", at("enum")));
    }
    let mut seen = BTreeSet::new();
    for v in e {
        let Some(s) = v.as_str() else {
            return Err(format!("{}: {v} must be a string", at("enum")));
        };
        if !seen.insert(s) {
            return Err(format!("{}: {s:?} is listed twice", at("enum")));
        }
        let billed = i
            .seconds_for
            .get(s)
            .copied()
            .or_else(|| s.parse::<f64>().ok());
        if !billed.is_some_and(|b| b.is_finite() && b >= 0.0) {
            return Err(format!(
                "{}: {s:?} is not a number of seconds; give it a billed length in `seconds_for`",
                at("enum")
            ));
        }
    }
    for (k, secs) in &i.seconds_for {
        if !seen.contains(k.as_str()) {
            return Err(format!("{}: {k:?} is not in the enum", at("seconds_for")));
        }
        if k.parse::<f64>().is_ok() {
            return Err(format!(
                "{}: {k:?} is numeric and is billed as itself",
                at("seconds_for")
            ));
        }
        if !(secs.is_finite() && *secs >= 0.0) {
            return Err(format!(
                "{}: {k:?} must be a finite number of seconds of at least 0",
                at("seconds_for")
            ));
        }
    }
    Ok(())
}

impl Input {
    /// The enum string whose numeric value equals `n` (`enum_seconds` inputs).
    fn enum_seconds_match(&self, n: f64) -> Option<&str> {
        self.enum_values
            .iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|s| s.parse::<f64>().is_ok_and(|x| x == n))
    }

    /// An `enum_seconds` value: a listed string, or a number equal to a numeric one.
    fn check_enum_seconds(&self, v: &Value) -> Result<(), String> {
        let listed = match v {
            Value::String(s) => self
                .enum_values
                .iter()
                .flatten()
                .any(|x| x.as_str() == Some(s.as_str())),
            Value::Number(n) => n
                .as_f64()
                .is_some_and(|n| self.enum_seconds_match(n).is_some()),
            _ => return Err("must be a string or a number of seconds".into()),
        };
        if listed {
            Ok(())
        } else {
            let list: Vec<String> = self
                .enum_values
                .iter()
                .flatten()
                .map(Value::to_string)
                .collect();
            Err(format!(
                "must be one of [{}] (as a string or the equal number), got {v}",
                list.join(", ")
            ))
        }
    }

    /// Checks one value, then returns the form that is sent and priced: an
    /// `enum_seconds` number becomes its enum string; everything else is unchanged.
    fn resolve(&self, v: &Value) -> Result<Value, String> {
        self.check(v)?;
        if let (InputType::EnumSeconds, Value::Number(n)) = (self.ty, v) {
            if let Some(s) = n.as_f64().and_then(|n| self.enum_seconds_match(n)) {
                return Ok(json!(s));
            }
        }
        Ok(v.clone())
    }

    /// The number pricing reads from a value: the value itself, or for an
    /// `enum_seconds` string its billed seconds. None when not a finite number.
    fn amount(&self, v: &Value) -> Option<f64> {
        let n = match (self.ty, v) {
            (InputType::EnumSeconds, Value::String(s)) => self
                .seconds_for
                .get(s.as_str())
                .copied()
                .or_else(|| s.parse::<f64>().ok()),
            _ => v.as_f64(),
        };
        n.filter(|x| x.is_finite())
    }

    /// A stand-in for a required input that has no default, used only for the
    /// advertised `est_usd`: the first enum value, else the `min`; a required
    /// url counts as supplied.
    fn floor_value(&self) -> Option<Value> {
        if let Some(first) = self.enum_values.iter().flatten().next() {
            return Some(first.clone());
        }
        match self.ty {
            InputType::Integer => self.min.map(|m| json!(m.ceil() as i64)),
            InputType::Number => self.min.map(|m| json!(m)),
            // A required reference counts as supplied.
            InputType::Url => Some(json!("https://example.com/")),
            InputType::UrlList => Some(json!(["https://example.com/"])),
            _ => None,
        }
    }

    /// Checks one value against this input's type, enum and range.
    fn check(&self, v: &Value) -> Result<(), String> {
        match self.ty {
            InputType::EnumSeconds => return self.check_enum_seconds(v),
            InputType::Json => {
                if !(v.is_object() || v.is_array()) {
                    return Err("must be a JSON object or array".into());
                }
                let max = self.max_bytes.unwrap_or(0);
                let len = serde_json::to_string(v).map_or(usize::MAX, |s| s.len());
                if len > max {
                    return Err(format!("is {len} bytes serialized; the limit is {max}"));
                }
            }
            InputType::String => {
                let Some(text) = v.as_str() else {
                    return Err("must be a string".into());
                };
                if let Some(max) = self.max_len {
                    let len = text.chars().count();
                    if len > max {
                        return Err(format!("is {len} characters; the limit is {max} (counted in Unicode scalar values)"));
                    }
                }
            }
            InputType::Integer => {
                if v.as_i64().is_none() && v.as_u64().is_none() {
                    return Err("must be an integer".into());
                }
            }
            InputType::Number => {
                if !v.is_number() {
                    return Err("must be a number".into());
                }
            }
            InputType::Boolean => {
                if !v.is_boolean() {
                    return Err("must be a boolean".into());
                }
            }
            InputType::Url => {
                if !v.as_str().is_some_and(is_url_or_local) {
                    return Err("must be an https URL or a local file path".into());
                }
            }
            InputType::UrlList => {
                let a = v
                    .as_array()
                    .ok_or_else(|| "must be an array of https URLs".to_string())?;
                if a.is_empty() {
                    return Err("must hold at least one URL".into());
                }
                if let Some(max) = self.max_items.filter(|m| a.len() > *m) {
                    return Err(format!("holds {} items; the limit is {max}", a.len()));
                }
                if let Some(n) = a
                    .iter()
                    .position(|u| !u.as_str().is_some_and(is_url_or_local))
                {
                    return Err(format!(
                        "item {n} must be an https URL or a local file path"
                    ));
                }
            }
        }
        if let Some(e) = &self.enum_values {
            if !e.iter().any(|x| same(x, v)) {
                let list: Vec<String> = e.iter().map(Value::to_string).collect();
                return Err(format!("must be one of [{}], got {v}", list.join(", ")));
            }
        }
        if let Some(n) = v.as_f64() {
            if self.min.is_some_and(|m| n < m) {
                return Err(format!(
                    "must be at least {}, got {n}",
                    self.min.unwrap_or_default()
                ));
            }
            if self.max.is_some_and(|m| n > m) {
                return Err(format!(
                    "must be at most {}, got {n}",
                    self.max.unwrap_or_default()
                ));
            }
        }
        Ok(())
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.param.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

impl Entry {
    /// Params of `url` and `url_list` inputs: the only places a local file path may be supplied.
    pub fn url_params(&self) -> Vec<String> {
        self.inputs
            .iter()
            .filter(|i| matches!(i.ty, InputType::Url | InputType::UrlList))
            .map(|i| i.param.clone())
            .collect()
    }

    fn input(&self, param: &str) -> Option<&Input> {
        self.inputs.iter().find(|i| i.param == param)
    }

    /// Load-time validation. The message names the offending field.
    pub fn validate(&self) -> Result<(), String> {
        if !PROVIDERS.contains(&self.provider.as_str()) {
            return Err(format!(
                "field `provider`: {:?} is not supported (use {})",
                self.provider,
                PROVIDERS.join(" or ")
            ));
        }
        if self.model.trim().is_empty() {
            return Err("field `model`: must not be empty".into());
        }
        if self.label.trim().is_empty() {
            return Err("field `label`: must not be empty".into());
        }
        if !valid_endpoint(&self.endpoint) {
            return Err(format!(
                "field `endpoint`: {:?} must be a relative path of [A-Za-z0-9._/-] with no `..`, scheme or leading slash",
                self.endpoint
            ));
        }
        let mut names = BTreeSet::new();
        let mut keys: BTreeSet<String> = BTreeSet::new();
        for (n, i) in self.inputs.iter().enumerate() {
            let at = |f: &str| format!("field `inputs[{n}].{f}`");
            if i.param.is_empty() {
                return Err(format!("{}: must not be empty", at("param")));
            }
            if i.send && i.json_key.is_empty() {
                return Err(format!("{}: must not be empty", at("json_key")));
            }
            for name in i.names() {
                if name.is_empty()
                    || (name == "prompt" && i.param != "prompt")
                    || !names.insert(name)
                {
                    return Err(format!(
                        "{}: {name:?} is empty, reserved or already used by another input",
                        at("param")
                    ));
                }
            }
            if i.send {
                // A nested input is keyed by its full path, so two may share a `json_key` under different parents.
                let path = match &i.nest {
                    Some(n) => format!("{n}\u{1f}{}", i.json_key),
                    None => i.json_key.clone(),
                };
                if !keys.insert(path)
                    || (i.nest.is_none() && self.constants.contains_key(&i.json_key))
                {
                    return Err(format!(
                        "{}: {:?} is sent more than once",
                        at("json_key"),
                        i.json_key
                    ));
                }
            }
            if let Some(n) = &i.nest {
                if n.is_empty() || !i.send {
                    return Err(format!(
                        "{}: a nest needs a non-empty name and a sent input",
                        at("nest")
                    ));
                }
            }
            if let Some(m) = i.max_len {
                if i.ty != InputType::String {
                    return Err(format!(
                        "{}: only string inputs take max_len",
                        at("max_len")
                    ));
                }
                if m == 0 {
                    return Err(format!("{}: must be at least 1", at("max_len")));
                }
            }
            if i.param == "prompt" && i.ty != InputType::String {
                return Err(format!("{}: the prompt input must be a string", at("type")));
            }
            if i.param == "prompt" && !i.send {
                return Err(format!("{}: the prompt input must be sent", at("send")));
            }
            match (i.ty, i.max_bytes) {
                (InputType::Json, Some(b)) if (1..=MAX_JSON_BYTES).contains(&b) => {}
                (InputType::Json, _) => {
                    return Err(format!(
                        "{}: a json input needs max_bytes from 1 to {MAX_JSON_BYTES}",
                        at("max_bytes")
                    ));
                }
                (_, Some(_)) => {
                    return Err(format!(
                        "{}: only json inputs take max_bytes",
                        at("max_bytes")
                    ))
                }
                (_, None) => {}
            }
            if i.ty != InputType::EnumSeconds && !i.seconds_for.is_empty() {
                return Err(format!(
                    "{}: only enum_seconds inputs take seconds_for",
                    at("seconds_for")
                ));
            }
            if i.role.is_some() && !matches!(i.ty, InputType::Url | InputType::UrlList) {
                return Err(format!("{}: a role needs type url or url_list", at("role")));
            }
            if i.max_items.is_some() && i.ty != InputType::UrlList {
                return Err(format!(
                    "{}: only url_list inputs take max_items",
                    at("max_items")
                ));
            }
            if (i.min.is_some() || i.max.is_some())
                && !matches!(i.ty, InputType::Integer | InputType::Number)
            {
                return Err(format!(
                    "{}: min and max need type integer or number",
                    at("min")
                ));
            }
            if let (Some(lo), Some(hi)) = (i.min, i.max) {
                if lo > hi {
                    return Err(format!("{}: min is above max", at("min")));
                }
            }
            if i.ty == InputType::EnumSeconds {
                validate_enum_seconds(i, &at)?;
            } else if let Some(e) = &i.enum_values {
                if e.is_empty() {
                    return Err(format!("{}: must not be empty", at("enum")));
                }
                if matches!(i.ty, InputType::Url | InputType::UrlList | InputType::Json) {
                    return Err(format!("{}: url and json inputs take no enum", at("enum")));
                }
                for v in e {
                    let probe = Input {
                        enum_values: None,
                        min: None,
                        max: None,
                        ..i.clone()
                    };
                    probe.check(v).map_err(|m| format!("{}: {m}", at("enum")))?;
                }
            }
            if let Some(d) = i.default.as_ref().filter(|d| !d.is_null()) {
                i.check(d).map_err(|m| format!("{}: {m}", at("default")))?;
            }
        }
        for k in self.constants.keys() {
            if k.is_empty() {
                return Err("field `constants`: keys must not be empty".into());
            }
        }
        // A nest name is a key of the body, so no top-level input or constant may use it.
        for (n, i) in self.inputs.iter().enumerate() {
            if let Some(nest) = &i.nest {
                let top = self
                    .inputs
                    .iter()
                    .any(|o| o.send && o.nest.is_none() && o.json_key == *nest);
                if top || self.constants.contains_key(nest) {
                    return Err(format!(
                        "field `inputs[{n}].nest`: {nest:?} is also a top-level key of the body"
                    ));
                }
            }
        }
        self.validate_pricing()?;
        let read = self.pricing_reads();
        for (n, i) in self.inputs.iter().enumerate() {
            if !i.send && !read.contains(i.param.as_str()) {
                return Err(format!(
                    "field `inputs[{n}].send`: an input that is not sent must be read by pricing"
                ));
            }
        }
        if parse_date(&self.priced_at).is_none() {
            return Err(format!(
                "field `priced_at`: {:?} is not an ISO date (YYYY-MM-DD)",
                self.priced_at
            ));
        }
        if !is_https(&self.price_source) {
            return Err(format!(
                "field `price_source`: {:?} must be an https URL",
                self.price_source
            ));
        }
        if let Some(d) = &self.durations {
            if d.durations_s.as_ref().is_some_and(|s| s.is_empty()) {
                return Err("field `durations.durations_s`: must not be empty".into());
            }
            if d.durations_s.is_some() && (d.min_duration_s.is_some() || d.max_duration_s.is_some())
            {
                return Err("field `durations`: use durations_s or min/max, not both".into());
            }
            if let (Some(lo), Some(hi)) = (d.min_duration_s, d.max_duration_s) {
                if lo > hi {
                    return Err("field `durations`: min_duration_s is above max_duration_s".into());
                }
            }
        }
        Ok(())
    }

    /// A parameter that pricing reads must be a declared input of the right
    /// type that is either defaulted or required, so a priced request always
    /// carries a value.
    fn pricing_param(&self, field: &str, param: &str, usage: Use) -> Result<&Input, String> {
        let i = self
            .input(param)
            .filter(|_| param != "prompt")
            .ok_or_else(|| format!("field `{field}`: {param:?} is not a declared input"))?;
        match usage {
            Use::Count | Use::Megapixels
                if !matches!(i.ty, InputType::Integer | InputType::Number) =>
            {
                return Err(format!(
                    "field `{field}`: input {param:?} must be an integer or number"
                ));
            }
            Use::Seconds
                if !matches!(
                    i.ty,
                    InputType::Integer | InputType::Number | InputType::EnumSeconds
                ) =>
            {
                return Err(format!(
                    "field `{field}`: input {param:?} must be an integer, number or enum_seconds"
                ));
            }
            Use::Selector if i.ty != InputType::String => {
                return Err(format!(
                    "field `{field}`: the input {param:?} must be a string"
                ));
            }
            _ => {}
        }
        if !i.required && i.default.as_ref().is_none_or(Value::is_null) {
            return Err(format!("field `{field}`: pricing depends on {param:?}, which has no default and is not required"));
        }
        Ok(i)
    }

    /// Every input name pricing reads, for the rule that an unsent input must be read.
    fn pricing_reads(&self) -> BTreeSet<&str> {
        let mut read = BTreeSet::new();
        if let Some(t) = self.pricing.timed() {
            read.insert(t.seconds_param);
            if let Some(d) = t.derived {
                read.insert(d.source_param.as_str());
            }
            if let Some(by) = t.by {
                read.insert(by.param.as_str());
            }
        }
        match &self.pricing {
            Pricing::PerItem { count_param, .. } => {
                read.insert(count_param.as_str());
            }
            Pricing::PerCharacter { text_param, .. } => {
                read.insert(text_param.as_str());
            }
            Pricing::PerMegapixel {
                megapixels_param,
                derived,
                ..
            } => {
                if let Some(p) = megapixels_param {
                    read.insert(p.as_str());
                }
                if let Some(d) = derived {
                    match &d.size {
                        SizeSource::Enum { param, .. } => {
                            read.insert(param.as_str());
                        }
                        SizeSource::WidthHeight {
                            width_param,
                            height_param,
                        } => {
                            read.insert(width_param.as_str());
                            read.insert(height_param.as_str());
                        }
                    }
                    if let Some(img) = &d.input_images {
                        read.insert(img.param.as_str());
                    }
                }
            }
            _ => {}
        }
        for a in self.pricing.adjust() {
            read.insert(a.when.param.as_str());
        }
        read
    }

    fn validate_pricing(&self) -> Result<(), String> {
        match &self.pricing {
            Pricing::Flat { usd, .. } => money("pricing.usd", *usd)?,
            Pricing::PerItem {
                usd, count_param, ..
            } => {
                money("pricing.usd", *usd)?;
                self.pricing_param("pricing.count_param", count_param, Use::Count)?;
            }
            Pricing::PerCharacter { .. } => self.validate_per_character()?,
            Pricing::PerMegapixel { .. } => self.validate_megapixel()?,
            Pricing::PerSecond { .. } | Pricing::PerMinute { .. } => self.validate_timed()?,
        }
        self.validate_adjust()
    }

    /// `per_second` and `per_minute`: rate, seconds input, billing bounds, `by`.
    fn validate_timed(&self) -> Result<(), String> {
        let Some(t) = self.pricing.timed() else {
            return Ok(());
        };
        money("pricing.usd", t.usd)?;
        if let Some(d) = t.derived {
            self.validate_derived_seconds(d, t.seconds_param)?;
        } else {
            let secs =
                self.pricing_param("pricing.seconds_param", t.seconds_param, Use::Seconds)?;
            if !secs.send && (!secs.required || secs.default.as_ref().is_some_and(|d| !d.is_null()))
            {
                return Err(format!(
                    "field `pricing.seconds_param`: input {:?} is not sent, so the caller states its length: mark it required and give it no default",
                    t.seconds_param
                ));
            }
        }
        for (f, v) in [
            ("min_billed_s", t.min_billed_s),
            ("max_billed_s", t.max_billed_s),
            ("round_up_to_s", t.round_up_to_s),
        ] {
            if let Some(v) = v {
                money(&format!("pricing.{f}"), v)?;
            }
        }
        if t.round_up_to_s.is_some_and(|s| s <= 0.0) {
            return Err("field `pricing.round_up_to_s`: must be above 0".into());
        }
        if let (Some(lo), Some(hi)) = (t.min_billed_s, t.max_billed_s) {
            if lo > hi {
                return Err("field `pricing.min_billed_s`: is above max_billed_s".into());
            }
        }
        let Some(by) = t.by else { return Ok(()) };
        let i = self.pricing_param("pricing.by.param", &by.param, Use::Selector)?;
        if by.values.is_empty() {
            return Err("field `pricing.by.values`: must not be empty".into());
        }
        for (k, v) in &by.values {
            money(&format!("pricing.by.values.{k}"), *v)?;
        }
        match i.default.as_ref().filter(|d| !d.is_null()) {
            Some(d) => {
                let d = d.as_str().unwrap_or_default();
                if !by.values.contains_key(d) {
                    return Err(format!(
                        "field `pricing.by.values`: no price for the default {d:?} of {:?}",
                        by.param
                    ));
                }
            }
            // Required with no default: the request must name a value, and every
            // value the enum allows must have a price.
            None if i.enum_values.is_none() => {
                return Err(format!(
                    "field `pricing.by.param`: {:?} is required with no default, so it needs an enum and a price for every value",
                    by.param
                ));
            }
            None => {}
        }
        for v in i.enum_values.iter().flatten() {
            if !v.as_str().is_some_and(|s| by.values.contains_key(s)) {
                return Err(format!(
                    "field `pricing.by.values`: no price for allowed value {v} of {:?}",
                    by.param
                ));
            }
        }
        Ok(())
    }

    /// The `derived` part of a `per_second` or `per_minute` price: the hint input, the sent source
    /// media, and the reserved cap with its basis.
    fn validate_derived_seconds(&self, d: &DerivedSeconds, hint_param: &str) -> Result<(), String> {
        let f = "pricing.seconds_param";
        let hint = self
            .input(hint_param)
            .filter(|_| hint_param != "prompt")
            .ok_or_else(|| format!("field `{f}`: {hint_param:?} is not a declared input"))?;
        if !matches!(hint.ty, InputType::Integer | InputType::Number) {
            return Err(format!(
                "field `{f}`: input {hint_param:?} must be an integer or number"
            ));
        }
        if hint.send || hint.required || hint.default.as_ref().is_some_and(|v| !v.is_null()) {
            return Err(format!(
                "field `{f}`: with `pricing.derived` the input {hint_param:?} is only a hint: it must be unsent, optional and have no default, because the billed seconds come from the source media"
            ));
        }
        let f = "pricing.derived.source_param";
        let src = self.derivation_input(f, &d.source_param)?;
        if src.ty != InputType::Url || !matches!(src.role, Some(Role::Video | Role::Audio)) {
            return Err(format!(
                "field `{f}`: input {:?} must be a url input with role video or audio",
                d.source_param
            ));
        }
        if !src.required {
            return Err(format!(
                "field `{f}`: input {:?} must be required, so the billed media is always sent",
                d.source_param
            ));
        }
        match (d.reserve_cap_seconds, d.reserve_cap_basis) {
            (None, None) => {}
            (Some(c), Some(_)) if c.is_finite() && c > 0.0 => {}
            (Some(_), Some(_)) => return Err("field `pricing.derived.reserve_cap_seconds`: must be a finite number of seconds above 0".into()),
            (Some(_), None) => return Err("field `pricing.derived.reserve_cap_basis`: a cap needs its basis, `stated` or `assumed`".into()),
            (None, Some(_)) => return Err("field `pricing.derived.reserve_cap_basis`: a basis without reserve_cap_seconds".into()),
        }
        Ok(())
    }

    /// `per_megapixel`: flat (`usd`) or tiered (`first_usd` + `additional_usd`),
    /// with the megapixels from a numeric input or a constant.
    fn validate_megapixel(&self) -> Result<(), String> {
        let Pricing::PerMegapixel {
            usd,
            first_usd,
            additional_usd,
            megapixels_param,
            megapixels,
            derived,
            ..
        } = &self.pricing
        else {
            return Ok(());
        };
        if let Some(d) = derived {
            if megapixels_param.is_some() || megapixels.is_some() {
                return Err("field `pricing.derived`: give derived, megapixels_param or megapixels, one source only".into());
            }
            match (usd, first_usd, additional_usd) {
                (Some(u), None, None) => money("pricing.usd", *u)?,
                (None, Some(f), Some(a)) => {
                    money("pricing.first_usd", *f)?;
                    money("pricing.additional_usd", *a)?;
                }
                _ => return Err("field `pricing`: per_megapixel takes either `usd` or both `first_usd` and `additional_usd`".into()),
            }
            return self.validate_derived(d);
        }
        match (usd, first_usd, additional_usd) {
            (Some(u), None, None) => {
                money("pricing.usd", *u)?;
                match (megapixels_param, megapixels) {
                    (Some(p), None) => {
                        self.pricing_param("pricing.megapixels_param", p, Use::Megapixels)?;
                    }
                    (None, Some(m)) => money("pricing.megapixels", *m)?,
                    (None, None) => {
                        return Err("field `pricing.megapixels_param`: per_megapixel needs megapixels_param or a megapixels constant".into());
                    }
                    (Some(_), Some(_)) => {
                        return Err("field `pricing.megapixels`: give megapixels_param or megapixels, not both".into());
                    }
                }
            }
            (None, Some(f), Some(a)) => {
                money("pricing.first_usd", *f)?;
                money("pricing.additional_usd", *a)?;
                match (megapixels_param, megapixels) {
                    (Some(p), None) => {
                        self.pricing_param("pricing.megapixels_param", p, Use::Megapixels)?;
                    }
                    // A constant is the maximum the route can bill when no size is sent: reserved, never stated by the caller.
                    (None, Some(m)) => money("pricing.megapixels", *m)?,
                    (None, None) => {
                        return Err("field `pricing.megapixels_param`: a tiered per_megapixel price needs megapixels_param or a megapixels constant".into());
                    }
                    (Some(_), Some(_)) => {
                        return Err("field `pricing.megapixels`: give megapixels_param or megapixels, not both".into());
                    }
                }
            }
            _ => {
                return Err("field `pricing`: per_megapixel takes either `usd` or both `first_usd` and `additional_usd`".into());
            }
        }
        Ok(())
    }

    /// An input that pricing derives a basis from: declared, not `prompt`, and SENT. A price basis
    /// read from an unsent input would be a number the caller states, which the engine never trusts.
    fn derivation_input(&self, field: &str, param: &str) -> Result<&Input, String> {
        let i = self
            .input(param)
            .filter(|_| param != "prompt")
            .ok_or_else(|| format!("field `{field}`: {param:?} is not a declared input"))?;
        if !i.send {
            return Err(format!("field `{field}`: input {param:?} is not sent, so the caller would state the price basis; derive it from a sent input"));
        }
        if !i.required && i.default.as_ref().is_none_or(Value::is_null) {
            return Err(format!("field `{field}`: pricing depends on {param:?}, which has no default and is not required"));
        }
        Ok(i)
    }

    /// `per_character`: a rate and a sent string input.
    fn validate_per_character(&self) -> Result<(), String> {
        let Pricing::PerCharacter {
            usd,
            text_param,
            est_chars,
            ..
        } = &self.pricing
        else {
            return Ok(());
        };
        money("pricing.usd", *usd)?;
        let i = self.input(text_param).ok_or_else(|| {
            format!("field `pricing.text_param`: {text_param:?} is not a declared input")
        })?;
        if i.ty != InputType::String {
            return Err(format!(
                "field `pricing.text_param`: input {text_param:?} must be a string"
            ));
        }
        if !i.send {
            return Err(format!("field `pricing.text_param`: input {text_param:?} is not sent, so the caller would state its length; price the text that is sent"));
        }
        if text_param != "prompt" && !i.required && i.default.as_ref().is_none_or(Value::is_null) {
            return Err(format!("field `pricing.text_param`: pricing depends on {text_param:?}, which has no default and is not required"));
        }
        if let Some(n) = est_chars {
            if *n == 0 || i.max_len.is_some_and(|m| *n as usize > m) {
                return Err(
                    "field `pricing.est_chars`: must be from 1 to the input's max_len".into(),
                );
            }
        }
        Ok(())
    }

    /// The `derived` part of a `per_megapixel` price.
    fn validate_derived(&self, d: &DerivedMegapixels) -> Result<(), String> {
        let step = d.round_up_to_mp.unwrap_or(DEFAULT_ROUND_UP_MP);
        if !(step.is_finite() && step > 0.0) {
            return Err(
                "field `pricing.derived.round_up_to_mp`: must be a finite number above 0".into(),
            );
        }
        match &d.size {
            SizeSource::Enum { param, sizes } => {
                let f = "pricing.derived.size.param";
                let i = self.derivation_input(f, param)?;
                let (InputType::String, Some(allowed)) = (i.ty, &i.enum_values) else {
                    return Err(format!(
                        "field `{f}`: input {param:?} must be a string with an enum"
                    ));
                };
                if sizes.is_empty() {
                    return Err("field `pricing.derived.size.sizes`: must not be empty".into());
                }
                for (k, [w, h]) in sizes {
                    if *w == 0 || *h == 0 {
                        return Err(format!("field `pricing.derived.size.sizes.{k}`: width and height must be at least 1"));
                    }
                }
                for v in allowed {
                    if !v.as_str().is_some_and(|s| sizes.contains_key(s)) {
                        return Err(format!(
                            "field `pricing.derived.size.sizes`: no pixel size for allowed value {v} of {param:?}; leave a value without a published size out of the enum"
                        ));
                    }
                }
                for k in sizes.keys() {
                    if !allowed.iter().any(|v| v.as_str() == Some(k.as_str())) {
                        return Err(format!("field `pricing.derived.size.sizes`: {k:?} is not in the enum of {param:?}"));
                    }
                }
            }
            SizeSource::WidthHeight {
                width_param,
                height_param,
            } => {
                if width_param == height_param {
                    return Err(
                        "field `pricing.derived.size`: width_param and height_param must differ"
                            .into(),
                    );
                }
                for (f, p) in [
                    ("pricing.derived.size.width_param", width_param),
                    ("pricing.derived.size.height_param", height_param),
                ] {
                    let i = self.derivation_input(f, p)?;
                    if i.ty != InputType::Integer {
                        return Err(format!("field `{f}`: input {p:?} must be an integer"));
                    }
                    if !i.min.is_some_and(|m| m >= 1.0) || i.max.is_none() {
                        return Err(format!("field `{f}`: input {p:?} needs a min of at least 1 and a max, so the size is bounded"));
                    }
                }
            }
        }
        if let Some(img) = &d.input_images {
            let f = "pricing.derived.input_images.param";
            let i = self
                .input(&img.param)
                .filter(|_| img.param != "prompt")
                .ok_or_else(|| format!("field `{f}`: {:?} is not a declared input", img.param))?;
            if i.ty != InputType::UrlList || !i.send {
                return Err(format!(
                    "field `{f}`: input {:?} must be a sent url_list",
                    img.param
                ));
            }
            money(
                "pricing.derived.input_images.megapixels_each",
                img.megapixels_each,
            )?;
        }
        Ok(())
    }

    /// The `adjust` list: one change per item, on a declared input whose value
    /// is always known (or, for `present`, whose absence means false).
    fn validate_adjust(&self) -> Result<(), String> {
        for (n, a) in self.pricing.adjust().iter().enumerate() {
            let at = |f: &str| format!("field `pricing.adjust[{n}].{f}`");
            match (a.multiply, a.add_usd) {
                (Some(m), None) => {
                    if !(m.is_finite() && m >= 0.0) {
                        return Err(format!(
                            "{}: must be a finite number of at least 0",
                            at("multiply")
                        ));
                    }
                }
                (None, Some(x)) => {
                    if !(x.is_finite() && x >= 0.0) {
                        return Err(format!(
                            "{}: must be a finite amount of at least 0",
                            at("add_usd")
                        ));
                    }
                }
                _ => {
                    return Err(format!(
                        "{}: give exactly one of multiply and add_usd",
                        at("multiply")
                    ))
                }
            }
            let param = a.when.param.as_str();
            let i = self
                .input(param)
                .filter(|_| param != "prompt")
                .ok_or_else(|| {
                    format!("{}: {param:?} is not a declared input", at("when.param"))
                })?;
            match (&a.when.equals, a.when.present) {
                (Some(eq), None) => {
                    let fits = match i.ty {
                        InputType::Boolean => eq.is_boolean(),
                        InputType::String => eq.is_string(),
                        InputType::Integer | InputType::Number => eq.is_number(),
                        InputType::EnumSeconds => eq.as_str().is_some_and(|s| {
                            i.enum_values
                                .iter()
                                .flatten()
                                .any(|x| x.as_str() == Some(s))
                        }),
                        InputType::Url | InputType::UrlList | InputType::Json => {
                            return Err(format!("{}: a {} input cannot be tested with equals (use present for url inputs)", at("when.equals"), i.ty.name()));
                        }
                    };
                    if !fits {
                        return Err(format!(
                            "{}: {eq} does not fit the {} input {param:?}",
                            at("when.equals"),
                            i.ty.name()
                        ));
                    }
                    if i.ty != InputType::EnumSeconds {
                        if let Some(e) = &i.enum_values {
                            if !e.iter().any(|x| same(x, eq)) {
                                return Err(format!(
                                    "{}: {eq} is not one of the values {param:?} allows",
                                    at("when.equals")
                                ));
                            }
                        }
                    }
                    // The value is always known: given, defaulted, or required.
                    if !i.required && i.default.as_ref().is_none_or(Value::is_null) {
                        return Err(format!("{}: pricing depends on {param:?}, which has no default and is not required", at("when.param")));
                    }
                }
                (None, Some(_)) => {
                    if !matches!(i.ty, InputType::Url | InputType::UrlList | InputType::Json) {
                        return Err(format!(
                            "{}: present needs a url or url_list input (or a json input)",
                            at("when.present")
                        ));
                    }
                }
                _ => {
                    return Err(format!(
                        "{}: give exactly one of equals and present",
                        at("when")
                    ))
                }
            }
        }
        Ok(())
    }

    /// True when `priced_at` is more than [`STALE_AFTER_DAYS`] before `today`.
    pub fn is_stale_on(&self, today: Date) -> bool {
        parse_date(&self.priced_at)
            .is_none_or(|d| i64::from(today.to_julian_day() - d.to_julian_day()) > STALE_AFTER_DAYS)
    }

    /// Checks a request against the entry and builds the provider body.
    pub fn prepare(&self, req: &GenRequest) -> Result<Prepared, ProviderError> {
        self.prepare_on(req, today())
    }

    pub fn prepare_on(&self, req: &GenRequest, today: Date) -> Result<Prepared, ProviderError> {
        let bad = |m: String| {
            ProviderError::new("invalid_request", format!("{}: {m}", self.model), false)
        };
        if self.is_stale_on(today) {
            return Err(ProviderError::new(
                "price_stale",
                format!(
                    "{}/{}: price set on {} is more than {STALE_AFTER_DAYS} days old (today {today}); refresh the entry before routing to it",
                    self.provider, self.model, self.priced_at
                ),
                false,
            ));
        }
        if req.kind != self.kind {
            return Err(bad("model does not match request kind".into()));
        }
        let none = Map::new();
        let params = match &req.params {
            Value::Object(m) => m,
            Value::Null => &none,
            _ => return Err(bad("params must be an object".into())),
        };
        let declared: BTreeSet<&str> = self
            .inputs
            .iter()
            .filter(|i| i.param != "prompt")
            .flat_map(Input::names)
            .collect();
        if let Some(k) = params
            .keys()
            .find(|k| k.as_str() != "provider_params" && !declared.contains(k.as_str()))
        {
            let list: Vec<&str> = declared.iter().copied().collect();
            return Err(bad(format!(
                "params.{k} is not an input of this model (declared: {})",
                list.join(", ")
            )));
        }
        let mut body = self.constants.clone();
        let mut values = BTreeMap::new();
        for i in &self.inputs {
            let given: Vec<&str> = i
                .names()
                .filter(|n| params.get(*n).is_some_and(|v| !v.is_null()))
                .collect();
            if given.len() > 1 {
                return Err(bad(format!(
                    "params.{} and params.{} are the same input; pass one",
                    given[0], given[1]
                )));
            }
            let value = if i.param == "prompt" {
                Some(json!(req.prompt))
            } else {
                given
                    .first()
                    .and_then(|n| params.get(*n))
                    .cloned()
                    .or_else(|| i.default.clone().filter(|d| !d.is_null()))
            };
            let Some(value) = value else {
                if i.required {
                    return Err(bad(format!("params.{} is required", i.param)));
                }
                continue;
            };
            let value = i
                .resolve(&value)
                .map_err(|m| bad(format!("params.{} {m}", i.param)))?;
            if i.send {
                match &i.nest {
                    None => {
                        body.insert(i.json_key.clone(), value.clone());
                    }
                    Some(n) => {
                        if let Value::Object(inner) = body
                            .entry(n.clone())
                            .or_insert_with(|| Value::Object(Map::new()))
                        {
                            inner.insert(i.json_key.clone(), value.clone());
                        }
                    }
                }
            }
            values.insert(i.param.clone(), value);
        }
        match params.get("provider_params") {
            None | Some(Value::Null) => {}
            Some(Value::Object(extra)) => {
                for (k, v) in extra {
                    if self.provider_params.iter().any(|a| a == k) && !body.contains_key(k) {
                        body.insert(k.clone(), v.clone());
                    } else {
                        eprintln!("rightkit-media-gen: {}/{}: provider_params.{k} is not a documented body key of this model; dropped", self.provider, self.model);
                    }
                }
            }
            Some(_) => return Err(bad("params.provider_params must be an object".into())),
        }
        Ok(Prepared {
            body: Value::Object(body),
            values,
        })
    }

    fn bad(&self, m: String) -> ProviderError {
        ProviderError::new("invalid_request", format!("{}: {m}", self.model), false)
    }

    /// Whether an `adjust` condition holds for these (post-default) values.
    fn applies(&self, w: &When, values: &BTreeMap<String, Value>) -> Result<bool, ProviderError> {
        let v = values.get(&w.param);
        if let Some(want) = w.present {
            let has = v.is_some_and(|v| match v {
                Value::Null => false,
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
                _ => true,
            });
            return Ok(has == want);
        }
        let Some(eq) = &w.equals else {
            return Err(self.bad(format!(
                "pricing condition on params.{} has neither equals nor present",
                w.param
            )));
        };
        let v = v.ok_or_else(|| {
            self.bad(format!("params.{} is needed to price the request", w.param))
        })?;
        let v = match self.input(&w.param) {
            Some(i) => i.resolve(v).unwrap_or_else(|_| v.clone()),
            None => v.clone(),
        };
        Ok(same(&v, eq))
    }

    /// Price of a prepared request, in USD, with no measurement of the source media (see
    /// [`Entry::price_with`]): an entry with a derived length reserves its cap.
    pub fn price(&self, values: &BTreeMap<String, Value>) -> Result<f64, ProviderError> {
        self.price_with(values, None)
    }

    /// Price of a prepared request, in USD. Always finite and at least 0, or an
    /// `invalid_request` error: a value pricing needs that is missing, negative
    /// or not finite is refused, never guessed.
    ///
    /// `probed_secs` is the length the ENGINE measured from the source media it will send (the
    /// bytes of the entry's `derived.source_param`), or None when it cannot measure it (an https URL).
    /// It matters only to an entry with a `derived` length; every other entry ignores it.
    pub fn price_with(
        &self,
        values: &BTreeMap<String, Value>,
        probed_secs: Option<f64>,
    ) -> Result<f64, ProviderError> {
        let num = |p: &str| -> Result<f64, ProviderError> {
            let v = values
                .get(p)
                .ok_or_else(|| self.bad(format!("params.{p} is needed to price the request")))?;
            let i = self
                .input(p)
                .ok_or_else(|| self.bad(format!("params.{p} is not an input of this model")))?;
            i.amount(v).filter(|x| *x >= 0.0).ok_or_else(|| {
                self.bad(format!(
                    "params.{p} must be a finite number of at least 0 to price the request"
                ))
            })
        };
        let mut amount = match &self.pricing {
            Pricing::Flat { usd, .. } => *usd,
            Pricing::PerItem {
                usd, count_param, ..
            } => usd * num(count_param)?,
            Pricing::PerCharacter {
                usd, text_param, ..
            } => {
                let text = values
                    .get(text_param)
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        self.bad(format!(
                            "params.{text_param} is needed to price the request"
                        ))
                    })?;
                // Unicode scalar values, exactly as `max_len` counts them; whole characters, no minimum.
                let chars = text.chars().count();
                if chars == 0 {
                    return Err(self.bad(format!(
                        "params.{text_param} must not be empty to price the request"
                    )));
                }
                usd * chars as f64 / CHARS_PER_UNIT
            }
            Pricing::PerMegapixel {
                usd,
                first_usd,
                additional_usd,
                megapixels_param,
                megapixels,
                derived,
                ..
            } => {
                let mp = match (derived, megapixels_param, megapixels) {
                    (Some(d), _, _) => self.derived_megapixels(d, values)?,
                    (None, Some(p), _) => num(p)?,
                    (None, None, Some(m)) => *m,
                    (None, None, None) => {
                        return Err(self.bad("per_megapixel pricing has no megapixels".into()))
                    }
                };
                match (usd, first_usd, additional_usd) {
                    (Some(u), _, _) => u * mp,
                    // The first megapixel (or any part of it) at `first_usd`, each megapixel beyond at `additional_usd`.
                    (None, Some(f), Some(a)) => f + a * (mp - 1.0).max(0.0),
                    _ => return Err(self.bad("per_megapixel pricing has no rate".into())),
                }
            }
            Pricing::PerSecond { .. } | Pricing::PerMinute { .. } => {
                let t = self
                    .pricing
                    .timed()
                    .ok_or_else(|| self.bad("pricing is not time-based".into()))?;
                let rate = match t.by {
                    None => t.usd,
                    Some(by) => {
                        let v = values
                            .get(&by.param)
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        *by.values.get(v).ok_or_else(|| {
                            self.bad(format!("no price for params.{} = {v:?}", by.param))
                        })?
                    }
                };
                let mut secs = match t.derived {
                    Some(d) => self.derived_seconds(
                        d,
                        t.seconds_param,
                        t.round_up_to_s,
                        values,
                        probed_secs,
                    )?,
                    None => num(t.seconds_param)?,
                };
                if let Some(step) = t.round_up_to_s.filter(|s| *s > 0.0) {
                    secs = ((secs / step) - ROUND_EPSILON).ceil().max(0.0) * step;
                }
                if let Some(lo) = t.min_billed_s {
                    secs = secs.max(lo);
                }
                if let Some(hi) = t.max_billed_s {
                    secs = secs.min(hi);
                }
                rate * secs / if t.per_minute { 60.0 } else { 1.0 }
            }
        };
        for a in self.pricing.adjust() {
            if self.applies(&a.when, values)? {
                if let Some(m) = a.multiply {
                    amount *= m;
                }
                if let Some(x) = a.add_usd {
                    amount += x;
                }
            }
        }
        if !(amount.is_finite() && amount >= 0.0) {
            return Err(self.bad("the computed price is not a finite amount of at least 0".into()));
        }
        Ok(if amount == 0.0 { 0.0 } else { amount })
    }

    /// Billed seconds of a `derived` timed price, before the entry's min/max bounds. With a
    /// measurement of the source media: its real length rounded UP to `round_up_to_s` (default 1 s).
    /// Without one: the entry's `reserve_cap_seconds`, or a refusal when it declares none. The
    /// caller's hint is never priced; it can only be refused when it disagrees with a measurement.
    fn derived_seconds(
        &self,
        d: &DerivedSeconds,
        hint_param: &str,
        round_up_to_s: Option<f64>,
        values: &BTreeMap<String, Value>,
        probed_secs: Option<f64>,
    ) -> Result<f64, ProviderError> {
        let src = &d.source_param;
        let Some(p) = probed_secs else {
            return d.reserve_cap_seconds.ok_or_else(|| {
                self.bad(format!(
                    "params.{src} is a remote URL the engine cannot measure before submit, and this model declares no maximum billable length, so the request cannot be priced; use a source the engine can measure"
                ))
            });
        };
        if !(p.is_finite() && p > 0.0) {
            return Err(self.bad(format!(
                "the measured length of params.{src} is not a positive number of seconds"
            )));
        }
        if let Some(h) = values
            .get(hint_param)
            .and_then(Value::as_f64)
            .filter(|h| h.is_finite())
        {
            if (h - p).abs() > HINT_TOLERANCE_S {
                return Err(self.bad(format!(
                    "params.{hint_param} states {h} s but the engine measured {p:.3} s in params.{src}; omit it or state the real length"
                )));
            }
        }
        if let (Some(cap), Some(CapBasis::Stated)) = (d.reserve_cap_seconds, d.reserve_cap_basis) {
            if p > cap + ROUND_EPSILON {
                return Err(self.bad(format!(
                    "params.{src} is {p:.3} s long; the provider accepts at most {cap} s"
                )));
            }
        }
        let step = round_up_to_s.filter(|s| *s > 0.0).unwrap_or(1.0);
        Ok(((p / step) - ROUND_EPSILON).ceil().max(0.0) * step)
    }

    /// Megapixels a derived `per_megapixel` price bills: the output size read from the SENT size
    /// inputs, rounded up to the entry's step, plus the declared megapixels of each input image.
    fn derived_megapixels(
        &self,
        d: &DerivedMegapixels,
        values: &BTreeMap<String, Value>,
    ) -> Result<f64, ProviderError> {
        let dim = |p: &str| -> Result<f64, ProviderError> {
            values
                .get(p)
                .and_then(Value::as_f64)
                .filter(|x| x.is_finite() && *x >= 1.0)
                .ok_or_else(|| {
                    self.bad(format!(
                        "params.{p} must be a number of pixels of at least 1 to price the request"
                    ))
                })
        };
        let (w, h) = match &d.size {
            SizeSource::Enum { param, sizes } => {
                let v = values.get(param).and_then(Value::as_str).ok_or_else(|| {
                    self.bad(format!("params.{param} is needed to price the request"))
                })?;
                let [w, h] = *sizes
                    .get(v)
                    .ok_or_else(|| self.bad(format!("no pixel size for params.{param} = {v:?}")))?;
                (f64::from(w), f64::from(h))
            }
            SizeSource::WidthHeight {
                width_param,
                height_param,
            } => (dim(width_param)?, dim(height_param)?),
        };
        let step = d.round_up_to_mp.unwrap_or(DEFAULT_ROUND_UP_MP);
        let exact = w * h / 1_000_000.0;
        let mut mp = ((exact / step) - ROUND_EPSILON).ceil().max(0.0) * step;
        if let Some(img) = &d.input_images {
            let n = values
                .get(&img.param)
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            mp += n as f64 * img.megapixels_each;
        }
        Ok(mp)
    }

    /// Price of one request made of defaults (advertised as `est_usd`). A
    /// required input with no default is taken at its first `enum` value, else
    /// its `min`; if none exists the estimate is 0.0 (unknown).
    pub fn default_estimate(&self) -> f64 {
        let mut values = BTreeMap::new();
        for i in &self.inputs {
            let v = i.default.clone().filter(|d| !d.is_null()).or_else(|| {
                if i.required {
                    i.floor_value()
                } else {
                    None
                }
            });
            if let Some(v) = v.and_then(|v| i.resolve(&v).ok()) {
                values.insert(i.param.clone(), v);
            }
        }
        if let Pricing::PerCharacter {
            text_param,
            est_chars,
            ..
        } = &self.pricing
        {
            let cap = self
                .input(text_param)
                .and_then(|i| i.max_len)
                .unwrap_or(usize::MAX);
            let n = est_chars.map_or(DEFAULT_EST_CHARS, |n| n as usize).min(cap);
            values.insert(text_param.clone(), json!("x".repeat(n)));
        }
        self.price(&values).unwrap_or(0.0)
    }

    /// The row `models` lists for this entry. Strings and slices are leaked:
    /// `ModelInfo` borrows `'static`, and each entry is converted once per process.
    pub fn model_info(&self) -> ModelInfo {
        let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
        let durations = self
            .durations
            .as_ref()
            .map_or(Durations::NONE, |d| Durations {
                durations_s: d
                    .durations_s
                    .clone()
                    .map(|v| &*Box::leak(v.into_boxed_slice())),
                min_duration_s: d.min_duration_s,
                max_duration_s: d.max_duration_s,
                default_duration_s: d.default_duration_s,
            });
        ModelInfo {
            provider: leak(&self.provider),
            model: leak(&self.model),
            kind: self.kind,
            label: leak(&self.label),
            est_usd: self.default_estimate(),
            durations,
        }
    }

    /// The input whose media length prices this entry (a `derived` timed price), when it has one.
    /// A caller that holds the source as bytes measures it with [`crate::media::probe_file`] and
    /// prices with [`Entry::price_with`]. The declared adapters take https URLs only and have no
    /// upload step, so they cannot measure and reserve the entry's cap through [`Entry::price`].
    pub fn derived_source_param(&self) -> Option<&str> {
        self.pricing
            .timed()?
            .derived
            .map(|d| d.source_param.as_str())
    }

    /// Reference images this entry accepts, in the shape built-in adapters declare for the model
    /// list: its first sent `url_list` input with role `reference` (its `max_items` is the cap).
    /// Single `url` references (a motion-control character image) are not a list and declare
    /// nothing. Declared entries take https URLs only, so there is no local upload.
    pub fn references(&self) -> Option<ReferenceInput> {
        let i = self
            .inputs
            .iter()
            .find(|i| i.send && i.ty == InputType::UrlList && i.role == Some(Role::Reference))?;
        Some(ReferenceInput {
            param: intern(&i.param),
            max: i.max_items.map(|m| m as u32),
            local_upload: "none",
        })
    }

    /// Extra `media modalities` fields for a declared model: `roles`, `settings`, `pricing`.
    pub fn describe(&self, today: Date) -> Map<String, Value> {
        let roles: Vec<Value> = self
            .inputs
            .iter()
            .filter_map(|i| {
                let role = i.role?;
                let mut r = json!({"role": role.name(), "param": i.param, "required": i.required});
                if let Some(m) = i.max_items {
                    r["max_items"] = json!(m);
                }
                Some(r)
            })
            .collect();
        let settings: Vec<Value> = self
            .inputs
            .iter()
            .filter(|i| i.role.is_none() && i.param != "prompt")
            .map(|i| {
                let mut s = json!({"param": i.param, "type": i.ty.name(), "required": i.required});
                if let Some(e) = &i.enum_values {
                    s["enum"] = json!(e);
                }
                if let Some(m) = i.min {
                    s["min"] = json!(m);
                }
                if let Some(m) = i.max {
                    s["max"] = json!(m);
                }
                if let Some(d) = i.default.as_ref().filter(|d| !d.is_null()) {
                    s["default"] = d.clone();
                }
                if !i.send {
                    s["sent"] = json!(false);
                }
                if let Some(b) = i.max_bytes {
                    s["max_bytes"] = json!(b);
                }
                if let Some(m) = i.max_len {
                    s["max_len"] = json!(m);
                }
                if !i.seconds_for.is_empty() {
                    s["seconds_for"] = json!(i.seconds_for);
                }
                s
            })
            .collect();
        let mut pricing = json!({
            "kind": self.pricing.name(),
            "priced_at": self.priced_at,
            "price_source": self.price_source,
            "stale": self.is_stale_on(today),
        });
        if let Some(t) = self.pricing.timed() {
            pricing["seconds_param"] = json!(t.seconds_param);
            // A caller-stated length: the reservation is only as good as the stated seconds.
            pricing["seconds_stated_by_caller"] =
                json!(t.derived.is_none() && self.input(t.seconds_param).is_some_and(|i| !i.send));
            if let Some(d) = t.derived {
                // The seconds are derived by the engine from the source media; the stated number is a hint that is never priced.
                pricing["seconds_derived_from"] = json!(d.source_param);
                pricing["seconds_hint_param"] = json!(t.seconds_param);
                pricing["seconds_hint_priced"] = json!(false);
                pricing["measured_source_rounds_up_to_s"] =
                    json!(t.round_up_to_s.filter(|s| *s > 0.0).unwrap_or(1.0));
                pricing["unmeasured_source_reserves"] = json!(if d.reserve_cap_seconds.is_some() {
                    "reserve_cap_seconds"
                } else {
                    "refused"
                });
                pricing["reserve_cap_seconds"] = json!(d.reserve_cap_seconds);
                pricing["reserve_cap_basis"] = json!(d.reserve_cap_basis.map(CapBasis::name));
            }
        }
        match &self.pricing {
            Pricing::PerCharacter { text_param, .. } => {
                pricing["chars_param"] = json!(text_param);
                pricing["unit_chars"] = json!(CHARS_PER_UNIT as u32);
                pricing["chars_counted_as"] = json!("unicode_scalar_values");
                // The count is read from the text that is sent, never stated by the caller.
                pricing["chars_stated_by_caller"] = json!(false);
            }
            Pricing::PerMegapixel {
                megapixels_param,
                megapixels,
                derived,
                ..
            } => match (derived, megapixels_param) {
                (Some(d), _) => {
                    pricing["megapixels_stated_by_caller"] = json!(false);
                    pricing["megapixels_derived_from"] = match &d.size {
                        SizeSource::Enum { param, sizes } => json!({"enum": param, "sizes": sizes}),
                        SizeSource::WidthHeight {
                            width_param,
                            height_param,
                        } => json!({"width": width_param, "height": height_param}),
                    };
                    pricing["round_up_to_mp"] =
                        json!(d.round_up_to_mp.unwrap_or(DEFAULT_ROUND_UP_MP));
                    pricing["rounding"] = json!(d.rounding.name());
                    if let Some(img) = &d.input_images {
                        pricing["input_images"] =
                            json!({"param": img.param, "megapixels_each": img.megapixels_each});
                    }
                }
                (None, Some(p)) => {
                    pricing["megapixels_param"] = json!(p);
                    pricing["megapixels_stated_by_caller"] =
                        json!(self.input(p).is_some_and(|i| !i.send));
                }
                (None, None) => {
                    if let Some(m) = megapixels {
                        // A constant is reserved for every request and is never a number the caller states.
                        pricing["megapixels_stated_by_caller"] = json!(false);
                        pricing["megapixels_reserved"] = json!(m);
                    }
                }
            },
            _ => {}
        }
        let mut out = Map::new();
        out.insert("roles".into(), Value::Array(roles));
        out.insert("settings".into(), Value::Array(settings));
        out.insert("pricing".into(), pricing);
        if let Some(r) = self.reconciled {
            out.insert("reconciled".into(), json!(r));
        }
        if let Some(m) = self.input("prompt").and_then(|i| i.max_len) {
            out.insert("prompt_max_len".into(), json!(m));
        }
        out
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    schema_version: u32,
    #[serde(default)]
    models: Vec<Value>,
}

fn entry_id(v: &Value, index: usize) -> String {
    match (
        v.get("provider").and_then(Value::as_str),
        v.get("model").and_then(Value::as_str),
    ) {
        (Some(p), Some(m)) => format!("{p}/{m}"),
        (None, Some(m)) => m.to_string(),
        _ => format!("#{index}"),
    }
}

/// Parses one catalogue file. A file that does not parse (or has the wrong
/// schema version) yields no entries and one error; an invalid entry is
/// reported by name and the valid ones still load.
fn parse(
    text: &str,
    source: &str,
    builtin: &dyn Fn(&str, &str) -> bool,
) -> (Vec<Entry>, Vec<String>) {
    let doc: Doc = match serde_json::from_str(text) {
        Ok(d) => d,
        Err(e) => return (Vec::new(), vec![format!("{source}: {e}")]),
    };
    if doc.schema_version != SCHEMA_VERSION {
        return (
            Vec::new(),
            vec![format!(
                "{source}: schema_version {} is not supported (expected {SCHEMA_VERSION})",
                doc.schema_version
            )],
        );
    }
    let (mut entries, mut errors): (Vec<Entry>, Vec<String>) = (Vec::new(), Vec::new());
    for (n, v) in doc.models.into_iter().enumerate() {
        let id = entry_id(&v, n);
        let entry = match serde_json::from_value::<Entry>(v) {
            Ok(e) => e,
            Err(e) => {
                errors.push(format!("{source}: entry {id}: {e}"));
                continue;
            }
        };
        if let Err(m) = entry.validate() {
            errors.push(format!("{source}: entry {id}: {m}"));
        } else if builtin(&entry.provider, &entry.model) {
            errors.push(format!(
                "{source}: entry {id}: the model is built into the catalogue and cannot be redeclared"
            ));
        } else if entries
            .iter()
            .any(|e| e.provider == entry.provider && e.model == entry.model)
        {
            errors.push(format!("{source}: entry {id}: declared twice in this file"));
        } else {
            entries.push(entry);
        }
    }
    (entries, errors)
}

/// The loaded catalogue: declared entries plus whatever was reported while loading.
#[derive(Debug)]
pub struct Catalogue {
    entries: Vec<Entry>,
    user_file: Option<PathBuf>,
    errors: Vec<String>,
    /// Per provider (index in [`PROVIDERS`]): the Rust rows followed by declared rows.
    rows: [OnceLock<Vec<ModelInfo>>; 2],
}

impl Catalogue {
    /// Loads `embedded` then the user file at `user_path`; a user entry with the
    /// same (provider, model) replaces the embedded one. A user file that cannot
    /// be read or parsed is ignored as a whole and its error retained.
    pub fn load(embedded: &str, user_path: &Path, builtin: &dyn Fn(&str, &str) -> bool) -> Self {
        let (mut entries, mut errors) = parse(embedded, "embedded catalogue", builtin);
        let mut user_file = None;
        match std::fs::read_to_string(user_path) {
            Ok(text) => {
                user_file = Some(user_path.to_path_buf());
                let (user, errs) = parse(&text, &user_path.display().to_string(), builtin);
                errors.extend(errs);
                for e in user {
                    match entries
                        .iter_mut()
                        .find(|x| x.provider == e.provider && x.model == e.model)
                    {
                        Some(slot) => *slot = e,
                        None => entries.push(e),
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                user_file = Some(user_path.to_path_buf());
                errors.push(format!("{}: cannot read: {e}", user_path.display()));
            }
        }
        Self {
            entries,
            user_file,
            errors,
            rows: [OnceLock::new(), OnceLock::new()],
        }
    }

    /// The process catalogue, loaded on first use from the embedded file and
    /// `<app data root>/catalogue/models.json`.
    pub fn get() -> &'static Catalogue {
        static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
        CATALOGUE.get_or_init(|| {
            let user = crate::host::app_data_root()
                .join("catalogue")
                .join("models.json");
            Catalogue::load(EMBEDDED, &user, &crate::providers::is_builtin)
        })
    }

    pub fn find(&self, provider: &str, model: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|e| e.provider == provider && e.model == model)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// `rust` followed by this provider's declared rows, built once. With no
    /// declared entry the Rust slice is returned untouched.
    pub fn rows(&'static self, provider: &str, rust: &'static [ModelInfo]) -> &'static [ModelInfo] {
        let Some(slot) = PROVIDERS.iter().position(|p| *p == provider) else {
            return rust;
        };
        if !self.entries.iter().any(|e| e.provider == provider) {
            return rust;
        }
        self.rows[slot].get_or_init(|| {
            let declared = self
                .entries
                .iter()
                .filter(|e| e.provider == provider)
                .map(Entry::model_info);
            rust.iter().cloned().chain(declared).collect()
        })
    }

    /// The `catalogue` object of `media modalities`.
    pub fn summary(&self) -> Value {
        json!({
            "entries": self.entries.len(),
            "user_file": self.user_file.as_ref().map(|p| p.display().to_string()),
            "error": if self.errors.is_empty() { Value::Null } else { json!(self.errors) },
        })
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// ISO date `n` days before today.
    pub fn days_ago(n: i64) -> String {
        let d = Date::from_julian_day(today().to_julian_day() - n as i32).unwrap();
        format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
    }

    /// A valid entry for `provider` exercising every input type and a tiered per-second price.
    pub fn sample(provider: &str, model: &str) -> Value {
        json!({
            "provider": provider, "model": model, "kind": "motion_control", "label": "Declared test model",
            "endpoint": format!("{model}/v1"),
            "inputs": [
                {"param": "prompt", "json_key": "prompt", "type": "string"},
                {"param": "video_url", "json_key": "video_url", "type": "url", "role": "video", "required": true},
                {"param": "image_urls", "json_key": "image_urls", "type": "url_list", "role": "reference", "required": true, "max_items": 2},
                {"param": "duration", "json_key": "duration", "type": "integer", "default": 5, "min": 3, "max": 15},
                {"param": "resolution", "json_key": "resolution", "type": "string", "enum": ["720p", "1080p"], "default": "720p"},
                {"param": "keep_audio", "json_key": "keep_original_sound", "type": "boolean", "default": true},
                {"param": "strength", "json_key": "strength", "type": "number", "min": 0, "max": 1, "aliases": ["intensity"]}
            ],
            "constants": {"mode": "std"},
            "pricing": {"kind": "per_second", "usd": 0.1, "seconds_param": "duration",
                "by": {"param": "resolution", "values": {"720p": 0.1, "1080p": 0.2}}},
            "priced_at": days_ago(1),
            "price_source": "https://example.com/pricing",
            "durations": {"min_duration_s": 3, "max_duration_s": 15, "default_duration_s": 5}
        })
    }

    /// A leaked catalogue of `models` for adapter tests; panics when any entry is invalid.
    pub fn catalogue(models: Vec<Value>) -> &'static Catalogue {
        let dir = std::env::temp_dir().join("rightkit-catalogue-none");
        let doc = json!({"schema_version": 1, "models": models}).to_string();
        let c = Catalogue::load(&doc, &dir.join("models.json"), &|_, _| false);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        Box::leak(Box::new(c))
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn entry(v: Value) -> Result<Entry, String> {
        let e: Entry = serde_json::from_value(v).map_err(|e| e.to_string())?;
        e.validate().map(|_| e)
    }

    fn rejects(v: Value, needle: &str) {
        let e = entry(v).unwrap_err();
        assert!(e.contains(needle), "{e:?} lacks {needle:?}");
    }

    fn with(mutate: impl FnOnce(&mut Value)) -> Value {
        let mut v = sample("fal", "fal-ai/x/y");
        mutate(&mut v);
        v
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rightkit-catalogue-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn doc(models: Vec<Value>) -> String {
        json!({"schema_version": 1, "models": models}).to_string()
    }

    const EMPTY: &str = r#"{"schema_version":1,"models":[]}"#;

    #[test]
    fn catalogue_entry_round_trips() {
        let e = entry(sample("fal", "fal-ai/x/y")).unwrap();
        assert_eq!(
            (e.provider.as_str(), e.kind, e.endpoint.as_str()),
            ("fal", Kind::MotionControl, "fal-ai/x/y/v1")
        );
        assert_eq!(e.inputs.len(), 7);
        assert_eq!(e.inputs[2].role, Some(Role::Reference));
        assert_eq!(e.inputs[6].aliases, ["intensity"]);
        assert_eq!(e.constants["mode"], "std");
        let info = e.model_info();
        assert_eq!(
            (info.provider, info.model, info.kind),
            ("fal", "fal-ai/x/y", Kind::MotionControl)
        );
        assert!((info.est_usd - 0.5).abs() < 1e-12, "5 s at the 720p rate");
        assert_eq!(
            (
                info.durations.min_duration_s,
                info.durations.max_duration_s,
                info.durations.default_duration_s
            ),
            (Some(3), Some(15), Some(5))
        );
        let d = e.describe(today());
        assert_eq!(
            d["roles"],
            json!([
                {"role": "video", "param": "video_url", "required": true},
                {"role": "reference", "param": "image_urls", "required": true, "max_items": 2}
            ])
        );
        assert_eq!(
            d["settings"][0],
            json!({"param": "duration", "type": "integer", "required": false, "min": 3.0, "max": 15.0, "default": 5})
        );
        assert_eq!(d["settings"][1]["enum"], json!(["720p", "1080p"]));
        assert_eq!(d["pricing"]["kind"], "per_second");
        assert_eq!(d["pricing"]["stale"], false);
        assert!(d["settings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["param"] != "prompt"));
    }

    #[test]
    fn catalogue_declares_reference_list_inputs_to_the_model_list() {
        // A sent url_list input with role `reference` is declared, with its cap.
        let e = entry(sample("fal", "fal-ai/x/y")).unwrap();
        let r = e.references().expect("image_urls is a reference list");
        assert_eq!(
            (r.param, r.max, r.local_upload),
            ("image_urls", Some(2), "none")
        );
        assert_eq!(
            json!(r),
            json!({"param": "image_urls", "max": 2, "local_upload": "none"})
        );
        // No cap on the input: `max` is omitted, as for built-in adapters that check no count.
        let mut v = sample("fal", "fal-ai/x/y");
        v["inputs"][2].as_object_mut().unwrap().remove("max_items");
        assert_eq!(
            json!(entry(v).unwrap().references()),
            json!({"param": "image_urls", "local_upload": "none"})
        );
        // A single url reference, a non-sent list, and a list with another role declare nothing.
        let mut v = sample("fal", "fal-ai/x/y");
        v["inputs"][2] = json!({"param": "image_url", "json_key": "image_url", "type": "url", "role": "reference", "required": true});
        assert!(entry(v).unwrap().references().is_none());
        let mut v = sample("fal", "fal-ai/x/y");
        v["inputs"][2]["role"] = json!("video");
        v["inputs"][1]["role"] = json!("audio");
        assert!(entry(v).unwrap().references().is_none());
    }

    #[test]
    fn catalogue_embedded_reference_declarations_match_their_mappings() {
        // The entries that send a reference image list, with the provider's own cap as `max_items`.
        let c = Catalogue::load(
            EMBEDDED,
            &scratch("embedded-refs").join("models.json"),
            &crate::providers::is_builtin,
        );
        let declared: Vec<(String, String, Option<u32>)> = c
            .entries()
            .iter()
            .filter_map(|e| {
                e.references()
                    .map(|r| (e.provider.clone(), e.model.clone(), r.max))
            })
            .collect();
        assert_eq!(
            declared,
            [
                (
                    "fal".to_string(),
                    "alibaba/wan-3.0/reference-to-video".to_string(),
                    Some(10)
                ),
                (
                    "fal".to_string(),
                    "fal-ai/flux-2/lora/edit".to_string(),
                    Some(4)
                ),
                (
                    "fal".to_string(),
                    "bytedance/seedream/v5/lite/edit".to_string(),
                    Some(10)
                ),
                (
                    "fal".to_string(),
                    "fal-ai/flux-pro/kontext/max/multi".to_string(),
                    Some(4)
                ),
                (
                    "wavespeed".to_string(),
                    "bytedance/seedream-v4/edit".to_string(),
                    Some(10)
                ),
            ]
        );
    }

    #[test]
    fn catalogue_rejects_each_invalid_entry() {
        rejects(
            with(|v| v["surprise"] = json!(1)),
            "unknown field `surprise`",
        );
        rejects(
            with(|v| v["inputs"][1]["colour"] = json!("red")),
            "unknown field `colour`",
        );
        rejects(
            with(|v| v["endpoint"] = json!("fal-ai/../admin")),
            "field `endpoint`",
        );
        rejects(
            with(|v| v["endpoint"] = json!("https://evil.example/x")),
            "field `endpoint`",
        );
        rejects(
            with(|v| v["endpoint"] = json!("/fal-ai/x")),
            "field `endpoint`",
        );
        rejects(with(|v| v["endpoint"] = json!("a//b")), "field `endpoint`");
        rejects(with(|v| v["endpoint"] = json!("")), "field `endpoint`");
        rejects(
            with(|v| v["provider"] = json!("elevenlabs")),
            "field `provider`",
        );
        rejects(
            with(|v| {
                v.as_object_mut().unwrap().remove("priced_at");
            }),
            "missing field `priced_at`",
        );
        rejects(
            with(|v| v["priced_at"] = json!("2026-13-40")),
            "field `priced_at`",
        );
        rejects(
            with(|v| {
                v.as_object_mut().unwrap().remove("price_source");
            }),
            "missing field `price_source`",
        );
        rejects(
            with(|v| v["price_source"] = json!("ftp://x")),
            "field `price_source`",
        );
        rejects(
            with(|v| {
                v["inputs"][3]
                    .as_object_mut()
                    .unwrap()
                    .remove("default")
                    .map(drop)
                    .unwrap()
            }),
            "pricing depends on \"duration\", which has no default",
        );
        rejects(
            with(|v| {
                v["inputs"][4]
                    .as_object_mut()
                    .unwrap()
                    .remove("default")
                    .map(drop)
                    .unwrap()
            }),
            "pricing depends on \"resolution\"",
        );
        rejects(
            with(|v| v["pricing"]["seconds_param"] = json!("missing")),
            "not a declared input",
        );
        rejects(
            with(|v| v["inputs"][1]["default"] = json!("http://plain.example/v.mp4")),
            "field `inputs[1].default`: must be an https URL",
        );
        rejects(
            with(|v| v["inputs"][3]["default"] = json!(99)),
            "field `inputs[3].default`",
        );
        rejects(
            with(|v| v["inputs"][4]["enum"] = json!(["720p", "4k"])),
            "no price for allowed value",
        );
        rejects(
            with(|v| v["inputs"][2]["role"] = json!("backdrop")),
            "unknown variant",
        );
        rejects(
            with(|v| v["inputs"][3]["role"] = json!("video")),
            "a role needs type url or url_list",
        );
        rejects(
            with(|v| v["inputs"][5]["json_key"] = json!("prompt")),
            "sent more than once",
        );
        rejects(
            with(|v| v["constants"]["duration"] = json!(5)),
            "sent more than once",
        );
        rejects(
            with(|v| v["pricing"] = json!({"kind": "flat", "usd": -1})),
            "field `pricing.usd`",
        );
        rejects(
            with(|v| v["pricing"] = json!({"kind": "flat", "usd": 1, "extra": 1})),
            "unknown field `extra`",
        );
    }

    #[test]
    fn catalogue_prices_each_kind() {
        let price = |pricing: Value, params: Value| {
            let mut v = sample("fal", "fal-ai/x/y");
            v["pricing"] = pricing;
            let e = entry(v).unwrap();
            let req = GenRequest {
                kind: Kind::MotionControl,
                provider: "fal".into(),
                model: e.model.clone(),
                prompt: "p".into(),
                params,
            };
            let p = e.prepare(&req).unwrap();
            (e.price(&p.values).unwrap(), e.default_estimate())
        };
        let ok = json!({"video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"]});
        let mut with_duration = ok.clone();
        with_duration["duration"] = json!(8);
        assert_eq!(
            price(json!({"kind": "flat", "usd": 0.75}), ok.clone()),
            (0.75, 0.75)
        );
        assert_eq!(
            price(
                json!({"kind": "per_second", "usd": 0.125, "seconds_param": "duration"}),
                with_duration.clone()
            ),
            (1.0, 0.625)
        );
        let by = json!({"kind": "per_second", "usd": 9.0, "seconds_param": "duration",
            "by": {"param": "resolution", "values": {"720p": 0.1, "1080p": 0.2}}});
        with_duration["resolution"] = json!("1080p");
        assert_eq!(price(by, with_duration).0, 1.6);
        assert_eq!(
            price(
                json!({"kind": "per_item", "usd": 0.04, "count_param": "duration"}),
                ok.clone()
            )
            .0,
            0.04 * 5.0
        );
        assert_eq!(
            price(
                json!({"kind": "per_megapixel", "usd": 0.006, "megapixels": 1.05}),
                ok
            )
            .0,
            0.006 * 1.05
        );
    }

    #[test]
    fn catalogue_user_entry_overrides_embedded() {
        let dir = scratch("override");
        let mut embedded = sample("fal", "fal-ai/x/y");
        embedded["label"] = json!("embedded");
        let mut user = sample("fal", "fal-ai/x/y");
        user["label"] = json!("user");
        let other = sample("wavespeed", "vendor/other");
        std::fs::write(dir.join("models.json"), doc(vec![user, other])).unwrap();
        let c = Catalogue::load(&doc(vec![embedded]), &dir.join("models.json"), &|_, _| {
            false
        });
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        assert_eq!(c.entries().len(), 2);
        assert_eq!(c.find("fal", "fal-ai/x/y").unwrap().label, "user");
        assert!(c.find("wavespeed", "vendor/other").is_some());
        let s = c.summary();
        assert_eq!(s["entries"], 2);
        assert_eq!(
            s["user_file"],
            dir.join("models.json").display().to_string()
        );
        assert!(s["error"].is_null());
    }

    #[test]
    fn catalogue_malformed_user_file_is_ignored_and_reported() {
        let dir = scratch("malformed");
        let path = dir.join("models.json");
        let kept = sample("fal", "fal-ai/x/y");
        std::fs::write(&path, "{ not json").unwrap();
        let c = Catalogue::load(&doc(vec![kept.clone()]), &path, &|_, _| false);
        assert_eq!(c.entries().len(), 1, "embedded entries survive");
        assert_eq!(c.errors().len(), 1);
        assert!(
            c.errors()[0].contains(&path.display().to_string()),
            "{:?}",
            c.errors()
        );
        assert_eq!(c.summary()["error"].as_array().unwrap().len(), 1);
        // A wrong schema version is the same: whole file ignored.
        std::fs::write(&path, r#"{"schema_version":2,"models":[]}"#).unwrap();
        let c = Catalogue::load(EMPTY, &path, &|_, _| false);
        assert!(
            c.errors()[0].contains("schema_version 2"),
            "{:?}",
            c.errors()
        );
    }

    #[test]
    fn catalogue_invalid_entry_is_reported_while_valid_ones_load() {
        let dir = scratch("partial");
        let path = dir.join("models.json");
        let mut bad = sample("fal", "fal-ai/bad");
        bad["endpoint"] = json!("../up");
        let mut builtin = sample("fal", "fal-ai/built-in");
        builtin["label"] = json!("shadow");
        let twin = sample("fal", "fal-ai/good");
        std::fs::write(&path, doc(vec![bad, builtin, twin.clone(), twin])).unwrap();
        let c = Catalogue::load(EMPTY, &path, &|_, m| m == "fal-ai/built-in");
        assert_eq!(c.entries().len(), 1);
        assert_eq!(c.entries()[0].model, "fal-ai/good");
        assert_eq!(c.errors().len(), 3, "{:?}", c.errors());
        assert!(
            c.errors()[0].contains("entry fal/fal-ai/bad")
                && c.errors()[0].contains("field `endpoint`")
        );
        assert!(c.errors()[1].contains("built into the catalogue"));
        assert!(c.errors()[2].contains("declared twice"));
    }

    #[test]
    fn catalogue_missing_user_file_is_not_an_error() {
        let c = Catalogue::load(EMPTY, &scratch("missing").join("models.json"), &|_, _| {
            false
        });
        assert!(c.errors().is_empty());
        assert_eq!(
            c.summary(),
            json!({"entries": 0, "user_file": null, "error": null})
        );
    }

    #[test]
    fn catalogue_staleness_boundary_is_sixty_days() {
        let mut v = sample("fal", "fal-ai/x/y");
        let today = today();
        for (age, stale) in [
            (0, false),
            (59, false),
            (60, false),
            (61, true),
            (400, true),
        ] {
            v["priced_at"] = json!(days_ago(age));
            let e = entry(v.clone()).unwrap();
            assert_eq!(e.is_stale_on(today), stale, "{age} days old");
            assert_eq!(e.describe(today)["pricing"]["stale"], stale);
        }
        v["priced_at"] = json!(days_ago(61));
        let e = entry(v).unwrap();
        let req = GenRequest {
            kind: Kind::MotionControl,
            provider: "fal".into(),
            model: e.model.clone(),
            prompt: "p".into(),
            params: json!({"video_url": "https://x/v.mp4", "image_urls": ["https://x/a.png"]}),
        };
        let err = e.prepare(&req).unwrap_err();
        assert_eq!(err.code, "price_stale");
        assert!(
            err.message.contains("fal-ai/x/y") && err.message.contains(&e.priced_at),
            "{}",
            err.message
        );
    }

    // ---- extended entry format: enum_seconds, adjust, unsent inputs, per_minute,
    // ---- tiered megapixels, required `by`, fractional durations, json inputs ----

    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} is not {b}");
    }

    fn merge(mut base: Value, extra: Value) -> Value {
        for (k, v) in extra.as_object().unwrap() {
            base[k.as_str()] = v.clone();
        }
        base
    }

    fn prompt_in() -> Value {
        json!({"param": "prompt", "json_key": "prompt", "type": "string", "required": true})
    }

    fn mini(inputs: Value, pricing: Value) -> Value {
        json!({
            "provider": "fal", "model": "fal-ai/t/m", "kind": "video", "label": "Test", "endpoint": "fal-ai/t/m",
            "inputs": inputs, "pricing": pricing, "priced_at": days_ago(1), "price_source": "https://example.com/p"
        })
    }

    fn req(e: &Entry, params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::Video,
            provider: "fal".into(),
            model: e.model.clone(),
            prompt: "p".into(),
            params,
        }
    }

    /// Prepare then price one request, as the adapters do.
    fn quote(e: &Entry, params: Value) -> Result<(Prepared, f64), ProviderError> {
        let p = e.prepare(&req(e, params))?;
        let usd = e.price(&p.values)?;
        Ok((p, usd))
    }

    fn seconds_in(en: Value) -> Value {
        json!({"param": "duration", "json_key": "duration", "type": "enum_seconds", "enum": en, "default": "5"})
    }

    fn per_s() -> Value {
        json!({"kind": "per_second", "usd": 0.1, "seconds_param": "duration"})
    }

    #[test]
    fn catalogue_enum_seconds_sends_the_string_and_prices_its_number() {
        let e = entry(mini(
            json!([prompt_in(), seconds_in(json!(["3", "5", "10"]))]),
            per_s(),
        ))
        .unwrap();
        let (p, usd) = quote(&e, json!({})).unwrap();
        assert_eq!(p.body["duration"], json!("5"));
        near(usd, 0.5);
        for given in [json!("10"), json!(10), json!(10.0)] {
            let (p, usd) = quote(&e, json!({"duration": given})).unwrap();
            assert_eq!(p.body["duration"], json!("10"), "always sent as the string");
            near(usd, 1.0);
        }
        for wrong in [
            json!(7),
            json!("7"),
            json!("5.0"),
            json!(true),
            json!(["5"]),
        ] {
            let err = quote(&e, json!({"duration": wrong})).unwrap_err();
            assert_eq!(err.code, "invalid_request");
            assert!(err.message.contains("params.duration"), "{}", err.message);
        }
        near(e.default_estimate(), 0.5);
        assert_eq!(e.describe(today())["settings"][0]["type"], "enum_seconds");
    }

    #[test]
    fn catalogue_enum_seconds_non_numeric_value_needs_a_billed_length() {
        let dur = |extra: Value| merge(seconds_in(json!(["5", "auto"])), extra);
        rejects(
            mini(json!([prompt_in(), dur(json!({}))]), per_s()),
            "\"auto\" is not a number of seconds",
        );
        let e = entry(mini(
            json!([prompt_in(), dur(json!({"seconds_for": {"auto": 10}}))]),
            per_s(),
        ))
        .unwrap();
        let (p, usd) = quote(&e, json!({"duration": "auto"})).unwrap();
        assert_eq!(p.body["duration"], json!("auto"));
        near(usd, 1.0);
        assert_eq!(
            quote(&e, json!({"duration": 10})).unwrap_err().code,
            "invalid_request",
            "a number never selects `auto`"
        );
        near(quote(&e, json!({})).unwrap().1, 0.5);
        for (extra, needle) in [
            (
                json!({"seconds_for": {"auto": 10, "ghost": 3}}),
                "\"ghost\" is not in the enum",
            ),
            (
                json!({"seconds_for": {"auto": 10, "5": 6}}),
                "numeric and is billed as itself",
            ),
            (
                json!({"seconds_for": {"auto": -1}}),
                "field `inputs[1].enum`",
            ),
            (
                json!({"seconds_for": {"auto": 10}, "enum": ["5", 6]}),
                "must be a string",
            ),
            (
                json!({"seconds_for": {"auto": 10}, "enum": ["5", "5", "auto"]}),
                "listed twice",
            ),
            (
                json!({"seconds_for": {"auto": 10}, "min": 1}),
                "min and max need type integer or number",
            ),
        ] {
            rejects(mini(json!([prompt_in(), dur(extra)]), per_s()), needle);
        }
        let mut no_enum = seconds_in(json!(["5"]));
        no_enum.as_object_mut().unwrap().remove("enum");
        rejects(
            mini(json!([prompt_in(), no_enum]), per_s()),
            "needs an enum of strings",
        );
        let numeric = json!({"param": "n", "json_key": "n", "type": "number", "default": 1, "seconds_for": {"a": 1}});
        rejects(
            mini(
                json!([prompt_in(), numeric]),
                json!({"kind": "flat", "usd": 1}),
            ),
            "only enum_seconds inputs take seconds_for",
        );
    }

    fn audio_inputs() -> Value {
        json!([
            prompt_in(),
            seconds_in(json!(["5", "10"])),
            {"param": "generate_audio", "json_key": "generate_audio", "type": "boolean", "default": false},
            {"param": "ref_url", "json_key": "ref_url", "type": "url"},
            {"param": "quality", "json_key": "quality", "type": "string", "enum": ["fast", "best"]},
            {"param": "mode", "json_key": "mode", "type": "string", "enum": ["std", "pro"], "default": "std"}
        ])
    }

    #[test]
    fn catalogue_adjust_applies_in_order_on_defaults_and_requests() {
        let timed = |adjust: Value| {
            entry(mini(audio_inputs(), json!({"kind": "per_second", "usd": 0.112, "seconds_param": "duration", "adjust": adjust}))).unwrap()
        };
        let e =
            timed(json!([{"when": {"param": "generate_audio", "equals": true}, "multiply": 1.5}]));
        near(quote(&e, json!({})).unwrap().1, 0.56);
        near(quote(&e, json!({"generate_audio": true})).unwrap().1, 0.84);
        near(
            quote(&e, json!({"generate_audio": true, "duration": 10}))
                .unwrap()
                .1,
            1.68,
        );
        near(e.default_estimate(), 0.56);
        // An omitted input counts at its default.
        let e =
            timed(json!([{"when": {"param": "generate_audio", "equals": false}, "multiply": 3}]));
        near(quote(&e, json!({})).unwrap().1, 1.68);
        near(quote(&e, json!({"generate_audio": true})).unwrap().1, 0.56);

        let flat = |adjust: Value| {
            entry(mini(
                audio_inputs(),
                json!({"kind": "flat", "usd": 1.0, "adjust": adjust}),
            ))
            .unwrap()
        };
        let on = json!({"param": "generate_audio", "equals": true});
        let e = flat(json!([{"when": on, "add_usd": 0.5}, {"when": on, "multiply": 2}]));
        near(quote(&e, json!({"generate_audio": true})).unwrap().1, 3.0);
        let e = flat(json!([{"when": on, "multiply": 2}, {"when": on, "add_usd": 0.5}]));
        near(quote(&e, json!({"generate_audio": true})).unwrap().1, 2.5);
        near(quote(&e, json!({})).unwrap().1, 1.0);

        let e = flat(json!([{"when": {"param": "mode", "equals": "pro"}, "add_usd": 0.25}]));
        near(quote(&e, json!({})).unwrap().1, 1.0);
        near(quote(&e, json!({"mode": "pro"})).unwrap().1, 1.25);
        // An enum_seconds condition matches the string, whether the request gave "10" or 10.
        let e = flat(json!([{"when": {"param": "duration", "equals": "10"}, "multiply": 2}]));
        near(quote(&e, json!({})).unwrap().1, 1.0);
        near(quote(&e, json!({"duration": "10"})).unwrap().1, 2.0);
        near(quote(&e, json!({"duration": 10})).unwrap().1, 2.0);
        // Reference inputs: present means a non-empty value was supplied.
        let e = flat(json!([{"when": {"param": "ref_url", "present": true}, "add_usd": 0.25}]));
        near(quote(&e, json!({})).unwrap().1, 1.0);
        near(
            quote(&e, json!({"ref_url": "https://x/r.png"})).unwrap().1,
            1.25,
        );
        let refs = json!([prompt_in(), {"param": "refs", "json_key": "refs", "type": "url_list"}]);
        let list = json!({"kind": "flat", "usd": 1.0, "adjust": [{"when": {"param": "refs", "present": true}, "multiply": 2}]});
        let e = entry(mini(refs, list)).unwrap();
        near(quote(&e, json!({})).unwrap().1, 1.0);
        near(
            quote(&e, json!({"refs": ["https://x/a.png"]})).unwrap().1,
            2.0,
        );
    }

    #[test]
    fn catalogue_rejects_inconsistent_adjust_by_field() {
        let mult = json!({"multiply": 2});
        let item = |when: Value, extra: &Value| {
            mini(
                audio_inputs(),
                json!({"kind": "flat", "usd": 1.0, "adjust": [merge(json!({"when": when}), extra.clone())]}),
            )
        };
        let on = json!({"param": "generate_audio", "equals": true});
        rejects(
            item(on.clone(), &json!({})),
            "field `pricing.adjust[0].multiply`: give exactly one of multiply and add_usd",
        );
        rejects(
            item(on.clone(), &json!({"multiply": 2, "add_usd": 1})),
            "give exactly one of multiply and add_usd",
        );
        rejects(
            item(on.clone(), &json!({"multiply": -1})),
            "field `pricing.adjust[0].multiply`: must be a finite number of at least 0",
        );
        rejects(
            item(on.clone(), &json!({"add_usd": -0.5})),
            "field `pricing.adjust[0].add_usd`",
        );
        for (when, needle) in [
            (
                json!({"param": "nope", "equals": true}),
                "\"nope\" is not a declared input",
            ),
            (
                json!({"param": "prompt", "equals": "x"}),
                "is not a declared input",
            ),
            (
                json!({"param": "quality", "equals": "fast"}),
                "which has no default and is not required",
            ),
            (
                json!({"param": "generate_audio", "equals": "yes"}),
                "does not fit the boolean input",
            ),
            (
                json!({"param": "mode", "equals": "turbo"}),
                "is not one of the values",
            ),
            (
                json!({"param": "duration", "equals": "7"}),
                "does not fit the enum_seconds input",
            ),
            (
                json!({"param": "generate_audio", "present": true}),
                "present needs a url or url_list input",
            ),
            (
                json!({"param": "ref_url", "equals": "https://x"}),
                "cannot be tested with equals",
            ),
            (
                json!({"param": "generate_audio"}),
                "give exactly one of equals and present",
            ),
            (
                json!({"param": "generate_audio", "equals": true, "present": true}),
                "give exactly one of equals and present",
            ),
            (
                json!({"param": "generate_audio", "equals": true, "colour": 1}),
                "unknown field `colour`",
            ),
        ] {
            rejects(item(when, &mult), needle);
        }
    }

    fn src_inputs(extra: Value) -> Value {
        let src = merge(
            json!({"param": "source_seconds", "type": "number", "send": false, "required": true, "min": 1, "max": 600}),
            extra,
        );
        json!([prompt_in(), {"param": "video_url", "json_key": "video_url", "type": "url", "role": "video", "required": true}, src])
    }

    fn src_price(extra: Value) -> Value {
        merge(
            json!({"kind": "per_second", "usd": 1.0, "seconds_param": "source_seconds"}),
            extra,
        )
    }

    #[test]
    fn catalogue_unsent_seconds_input_is_caller_stated_and_never_sent() {
        let e = entry(mini(
            src_inputs(json!({})),
            merge(src_price(json!({})), json!({"usd": 0.168})),
        ))
        .unwrap();
        let v = "https://x/v.mp4";
        let err = quote(&e, json!({"video_url": v})).unwrap_err();
        assert_eq!(err.code, "invalid_request");
        assert!(
            err.message.contains("params.source_seconds is required"),
            "{}",
            err.message
        );
        let (p, usd) = quote(&e, json!({"video_url": v, "source_seconds": 10})).unwrap();
        near(usd, 1.68);
        assert!(p.body.get("source_seconds").is_none(), "{}", p.body);
        assert_eq!(p.body["video_url"], v);
        assert_eq!(p.values["source_seconds"], json!(10));
        for wrong in [json!(0.5), json!(-3), json!("10"), json!(601)] {
            let err = quote(&e, json!({"video_url": v, "source_seconds": wrong})).unwrap_err();
            assert_eq!(err.code, "invalid_request");
        }
        near(e.default_estimate(), 0.168);
        let d = e.describe(today());
        assert_eq!(d["settings"][0]["sent"], false);
        assert_eq!(d["pricing"]["seconds_param"], "source_seconds");
        assert_eq!(d["pricing"]["seconds_stated_by_caller"], true);
    }

    #[test]
    fn catalogue_billed_seconds_are_rounded_raised_and_capped() {
        let price = |pricing: Value, secs: f64| {
            let e = entry(mini(src_inputs(json!({})), src_price(pricing))).unwrap();
            quote(
                &e,
                json!({"video_url": "https://x/v.mp4", "source_seconds": secs}),
            )
            .unwrap()
            .1
        };
        near(price(json!({"min_billed_s": 3}), 1.0), 3.0);
        near(price(json!({"min_billed_s": 3}), 4.0), 4.0);
        near(price(json!({"max_billed_s": 10}), 25.0), 10.0);
        near(price(json!({"round_up_to_s": 5}), 5.0), 5.0);
        near(price(json!({"round_up_to_s": 5}), 5.1), 10.0);
        near(price(json!({"round_up_to_s": 0.5}), 1.2), 1.5);
        near(price(json!({"round_up_to_s": 0.2}), 1.2), 1.2);
        near(
            price(
                json!({"round_up_to_s": 5, "min_billed_s": 3, "max_billed_s": 10}),
                1.0,
            ),
            5.0,
        );
        near(
            price(json!({"round_up_to_s": 5, "max_billed_s": 10}), 12.0),
            10.0,
        );
    }

    #[test]
    fn catalogue_rejects_unsent_input_and_billing_misuse() {
        for (extra, needle) in [
            (json!({"default": 5}), "the caller states its length"),
            (
                json!({"required": false, "default": 5}),
                "the caller states its length",
            ),
            (
                json!({"required": false}),
                "which has no default and is not required",
            ),
        ] {
            rejects(mini(src_inputs(extra), src_price(json!({}))), needle);
        }
        let mut unused = src_inputs(json!({}));
        unused
            .as_array_mut()
            .unwrap()
            .push(json!({"param": "note", "type": "number", "send": false, "default": 1}));
        rejects(
            mini(unused, src_price(json!({}))),
            "field `inputs[3].send`: an input that is not sent must be read by pricing",
        );
        rejects(
            mini(
                json!([{"param": "prompt", "type": "string", "send": false}]),
                json!({"kind": "flat", "usd": 1}),
            ),
            "the prompt input must be sent",
        );
        rejects(
            mini(
                json!([prompt_in(), {"param": "n", "type": "number"}]),
                json!({"kind": "flat", "usd": 1}),
            ),
            "field `inputs[1].json_key`: must not be empty",
        );
        for (extra, needle) in [
            (
                json!({"round_up_to_s": 0}),
                "field `pricing.round_up_to_s`: must be above 0",
            ),
            (
                json!({"min_billed_s": 9, "max_billed_s": 3}),
                "is above max_billed_s",
            ),
            (
                json!({"min_billed_s": -1}),
                "field `pricing.min_billed_s`: must be a finite amount",
            ),
        ] {
            rejects(mini(src_inputs(json!({})), src_price(extra)), needle);
        }
        rejects(
            mini(
                audio_inputs(),
                json!({"kind": "per_item", "usd": 1, "count_param": "duration", "min_billed_s": 3}),
            ),
            "unknown field `min_billed_s`",
        );
        // An unsent input that pricing reads through `adjust` is legitimate.
        let ins = json!([prompt_in(), {"param": "premium", "type": "boolean", "send": false, "default": false}]);
        let pr = json!({"kind": "flat", "usd": 1.0, "adjust": [{"when": {"param": "premium", "equals": true}, "multiply": 2}]});
        let e = entry(mini(ins, pr)).unwrap();
        let (p, usd) = quote(&e, json!({"premium": true})).unwrap();
        near(usd, 2.0);
        assert!(p.body.get("premium").is_none());
    }

    // ---- derived billed seconds (2026-10-12): the engine measures the source, or reserves a cap ----

    fn derived_inputs(hint: Value) -> Value {
        let hint = merge(
            json!({"param": "source_seconds", "type": "number", "send": false, "min": 1, "max": 600}),
            hint,
        );
        json!([prompt_in(), {"param": "video_url", "json_key": "video_url", "type": "url", "role": "video", "required": true}, hint])
    }

    fn derived_price(extra: Value) -> Value {
        json!({"kind": "per_second", "usd": 1.0, "seconds_param": "source_seconds", "derived": merge(json!({"source_param": "video_url"}), extra)})
    }

    fn derived_entry(derived: Value) -> Entry {
        entry(mini(derived_inputs(json!({})), derived_price(derived))).unwrap()
    }

    const CLIP: &str = "https://x/v.mp4";

    /// Prepare, then price with the length the engine measured (None: an https URL it cannot measure).
    fn price_probed(e: &Entry, params: Value, measured: Option<f64>) -> Result<f64, ProviderError> {
        let p = e.prepare(&req(e, params))?;
        e.price_with(&p.values, measured)
    }

    /// A WAV file of `bytes` data bytes at 16000 bytes per second, so its length is `bytes / 16000` s.
    fn wav_file(dir: &Path, name: &str, bytes: usize) -> PathBuf {
        let mut b = b"RIFF".to_vec();
        b.extend_from_slice(&(36 + bytes as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        for v in [1u16, 1] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&16000u32.to_le_bytes());
        for v in [2u16, 16] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(bytes as u32).to_le_bytes());
        b.extend(std::iter::repeat_n(0u8, bytes));
        let path = dir.join(name);
        std::fs::write(&path, b).unwrap();
        path
    }

    #[test]
    fn catalogue_derived_seconds_bill_the_probed_local_file_rounded_up() {
        let dir = scratch("derived-local");
        let e = derived_entry(json!({"reserve_cap_seconds": 600, "reserve_cap_basis": "assumed"}));
        // 196000 bytes at 16000 bytes per second is 12.25 s of media: billed as 13 whole seconds.
        let clip = wav_file(&dir, "clip.wav", 196_000);
        let secs = crate::media::probe_file(&clip)
            .unwrap()
            .duration_s
            .expect("the engine measures the file");
        near(secs, 12.25);
        near(
            price_probed(&e, json!({"video_url": CLIP}), Some(secs)).unwrap(),
            13.0,
        );
        // The stated hint is checked against the measurement, within 1 s, and never priced.
        near(
            price_probed(
                &e,
                json!({"video_url": CLIP, "source_seconds": 12}),
                Some(secs),
            )
            .unwrap(),
            13.0,
        );
        near(
            price_probed(
                &e,
                json!({"video_url": CLIP, "source_seconds": 13}),
                Some(secs),
            )
            .unwrap(),
            13.0,
        );
        for wrong in [1, 3, 11, 14] {
            let err = price_probed(
                &e,
                json!({"video_url": CLIP, "source_seconds": wrong}),
                Some(secs),
            )
            .unwrap_err();
            assert_eq!(err.code, "invalid_request");
            assert!(
                err.message.contains("the engine measured 12.250 s"),
                "{}",
                err.message
            );
        }
        // An unreadable measurement is refused, never guessed.
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                price_probed(&e, json!({"video_url": CLIP}), Some(bad))
                    .unwrap_err()
                    .code,
                "invalid_request",
                "{bad}"
            );
        }
        // The entry's own rounding step and bounds apply to the measured length.
        let stepped = entry(mini(
            derived_inputs(json!({})),
            derived_price(json!({"reserve_cap_seconds": 60, "reserve_cap_basis": "stated"})),
        ))
        .unwrap();
        near(
            price_probed(&stepped, json!({"video_url": CLIP}), Some(0.2)).unwrap(),
            1.0,
        );
        let mut v = mini(
            derived_inputs(json!({})),
            derived_price(json!({"reserve_cap_seconds": 60, "reserve_cap_basis": "stated"})),
        );
        v["pricing"]["round_up_to_s"] = json!(5);
        v["pricing"]["min_billed_s"] = json!(10);
        let bounded = entry(v).unwrap();
        near(
            price_probed(&bounded, json!({"video_url": CLIP}), Some(12.2)).unwrap(),
            15.0,
        );
        near(
            price_probed(&bounded, json!({"video_url": CLIP}), Some(2.0)).unwrap(),
            10.0,
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn catalogue_derived_seconds_reserve_the_cap_for_a_remote_url_whatever_the_caller_states() {
        let e = derived_entry(json!({"reserve_cap_seconds": 30, "reserve_cap_basis": "stated"}));
        near(
            price_probed(&e, json!({"video_url": CLIP}), None).unwrap(),
            30.0,
        );
        // A lower stated number cannot lower the estimate; neither can a higher one raise it.
        for stated in [1, 5, 29, 30, 600] {
            near(
                price_probed(
                    &e,
                    json!({"video_url": CLIP, "source_seconds": stated}),
                    None,
                )
                .unwrap(),
                30.0,
            );
        }
        // `price` (what the adapters call) is the unmeasured case.
        let p = e
            .prepare(&req(&e, json!({"video_url": CLIP, "source_seconds": 1})))
            .unwrap();
        near(e.price(&p.values).unwrap(), 30.0);
        assert!(
            p.body.get("source_seconds").is_none(),
            "the hint is never sent: {}",
            p.body
        );
        // The hint keeps its own range checks; the number is optional.
        assert_eq!(
            price_probed(&e, json!({"video_url": CLIP, "source_seconds": 0.5}), None)
                .unwrap_err()
                .code,
            "invalid_request"
        );
        near(e.default_estimate(), 30.0);
        assert_eq!(e.derived_source_param(), Some("video_url"));
        // A per_minute entry reserves its cap per minute.
        let mut v = mini(
            derived_inputs(json!({})),
            derived_price(json!({"reserve_cap_seconds": 600, "reserve_cap_basis": "assumed"})),
        );
        v["pricing"]["kind"] = json!("per_minute");
        v["pricing"]["usd"] = json!(5.0);
        near(
            price_probed(&entry(v).unwrap(), json!({"video_url": CLIP}), None).unwrap(),
            50.0,
        );
        // The entry's billing maximum bounds the reservation.
        let mut v = mini(
            derived_inputs(json!({})),
            derived_price(json!({"reserve_cap_seconds": 30, "reserve_cap_basis": "stated"})),
        );
        v["pricing"]["max_billed_s"] = json!(20);
        near(
            price_probed(&entry(v).unwrap(), json!({"video_url": CLIP}), None).unwrap(),
            20.0,
        );
    }

    #[test]
    fn catalogue_derived_seconds_without_a_cap_refuse_a_remote_url_and_stated_caps_refuse_longer_media(
    ) {
        let none = derived_entry(json!({}));
        let err =
            price_probed(&none, json!({"video_url": CLIP, "source_seconds": 5}), None).unwrap_err();
        assert_eq!(err.code, "invalid_request");
        assert!(
            err.message.contains("declares no maximum billable length"),
            "{}",
            err.message
        );
        near(none.default_estimate(), 0.0); // unknown, never a guess
        near(
            price_probed(&none, json!({"video_url": CLIP}), Some(7.5)).unwrap(),
            8.0,
        ); // a measured source needs no cap
           // A stated cap is the provider's limit: media past it is refused. An assumed cap never refuses measured media.
        let stated =
            derived_entry(json!({"reserve_cap_seconds": 30, "reserve_cap_basis": "stated"}));
        near(
            price_probed(&stated, json!({"video_url": CLIP}), Some(30.0)).unwrap(),
            30.0,
        );
        let err = price_probed(&stated, json!({"video_url": CLIP}), Some(30.5)).unwrap_err();
        assert!(
            err.message.contains("the provider accepts at most 30 s"),
            "{}",
            err.message
        );
        let assumed =
            derived_entry(json!({"reserve_cap_seconds": 30, "reserve_cap_basis": "assumed"}));
        near(
            price_probed(&assumed, json!({"video_url": CLIP}), Some(95.0)).unwrap(),
            95.0,
        );
    }

    #[test]
    fn catalogue_derived_seconds_load_rules_and_report() {
        let cap = json!({"reserve_cap_seconds": 30, "reserve_cap_basis": "stated"});
        let edit = |hint: Value, derived: Value| mini(derived_inputs(hint), derived_price(derived));
        entry(edit(json!({}), cap.clone())).unwrap();
        rejects(edit(json!({"required": true}), cap.clone()), "only a hint");
        rejects(
            edit(json!({"send": true, "json_key": "s"}), cap.clone()),
            "only a hint",
        );
        rejects(edit(json!({"default": 5}), cap.clone()), "only a hint");
        rejects(
            edit(
                json!({"type": "string", "min": null, "max": null}),
                cap.clone(),
            ),
            "must be an integer or number",
        );
        rejects(
            edit(json!({}), json!({"source_param": "ghost"})),
            "is not a declared input",
        );
        rejects(
            edit(json!({}), json!({"reserve_cap_seconds": 30})),
            "needs its basis",
        );
        rejects(
            edit(json!({}), json!({"reserve_cap_basis": "assumed"})),
            "a basis without reserve_cap_seconds",
        );
        rejects(
            edit(
                json!({}),
                json!({"reserve_cap_seconds": 0, "reserve_cap_basis": "assumed"}),
            ),
            "above 0",
        );
        rejects(
            edit(
                json!({}),
                json!({"reserve_cap_seconds": 30, "reserve_cap_basis": "guessed"}),
            ),
            "unknown variant",
        );
        rejects(
            edit(
                json!({}),
                json!({"reserve_cap_seconds": 30, "reserve_cap_basis": "stated", "seconds": 5}),
            ),
            "unknown field",
        );
        let source = |patch: Value| {
            let mut v = edit(json!({}), cap.clone());
            let s = merge(v["inputs"][1].clone(), patch);
            v["inputs"][1] = s;
            v
        };
        rejects(source(json!({"required": false})), "is not required");
        rejects(source(json!({"role": "mask"})), "role video or audio");
        rejects(
            source(json!({"type": "string", "role": null})),
            "role video or audio",
        );
        let e = entry(source(json!({"role": "audio"}))).unwrap();
        near(
            price_probed(&e, json!({"video_url": CLIP}), None).unwrap(),
            30.0,
        );
        // The hint may be dropped from the entry's inputs only if pricing no longer reads it, which it always does.
        let mut gone = edit(json!({}), cap.clone());
        gone["inputs"].as_array_mut().unwrap().pop();
        rejects(gone, "is not a declared input");

        let d = derived_entry(cap).describe(today());
        let p = &d["pricing"];
        assert_eq!(p["seconds_stated_by_caller"], false);
        assert_eq!(p["seconds_derived_from"], "video_url");
        assert_eq!(
            (
                p["seconds_hint_param"].clone(),
                p["seconds_hint_priced"].clone()
            ),
            (json!("source_seconds"), json!(false))
        );
        assert_eq!(
            (
                p["reserve_cap_seconds"].clone(),
                p["reserve_cap_basis"].clone()
            ),
            (json!(30.0), json!("stated"))
        );
        assert_eq!(
            (
                p["unmeasured_source_reserves"].clone(),
                p["measured_source_rounds_up_to_s"].clone()
            ),
            (json!("reserve_cap_seconds"), json!(1.0))
        );
        let hint = d["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["param"] == "source_seconds")
            .unwrap();
        assert_eq!(
            (hint["required"].clone(), hint["sent"].clone()),
            (json!(false), json!(false))
        );
        let d = derived_entry(json!({"reserve_cap_seconds": 600, "reserve_cap_basis": "assumed"}))
            .describe(today());
        assert_eq!(d["pricing"]["reserve_cap_basis"], "assumed");
        let d = derived_entry(json!({})).describe(today());
        assert_eq!(
            (
                d["pricing"]["unmeasured_source_reserves"].clone(),
                d["pricing"]["reserve_cap_seconds"].clone()
            ),
            (json!("refused"), Value::Null)
        );
    }

    #[test]
    fn catalogue_embedded_derived_entries_declare_their_caps() {
        let c = Catalogue::load(
            EMBEDDED,
            &scratch("embedded-derived").join("models.json"),
            &crate::providers::is_builtin,
        );
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        // (provider, model, source input, cap seconds, where the cap comes from)
        let table = [
            (
                "fal",
                "fal-ai/kling-video/v3/pro/motion-control",
                "video_url",
                30.0,
                "stated",
            ),
            (
                "fal",
                "fal-ai/kling-video/v3/standard/motion-control",
                "video_url",
                30.0,
                "stated",
            ),
            (
                "wavespeed",
                "kwaivgi/kling-v3.0-std/motion-control",
                "video",
                30.0,
                "stated",
            ),
            (
                "wavespeed",
                "kwaivgi/kling-v3.0-pro/motion-control",
                "video",
                30.0,
                "stated",
            ),
            (
                "wavespeed",
                "wavespeed-ai/wan-2.2/animate-2",
                "video",
                120.0,
                "stated",
            ),
            (
                "fal",
                "fal-ai/sync-lipsync/v2/pro",
                "video_url",
                600.0,
                "assumed",
            ),
            (
                "fal",
                "fal-ai/bytedance/omnihuman/v1.5",
                "audio_url",
                60.0,
                "assumed",
            ),
            (
                "fal",
                "bytedance/omnihuman/v2",
                "audio_url",
                60.0,
                "assumed",
            ),
            ("fal", "veed/fabric-1.0", "audio_url", 60.0, "assumed"),
            ("fal", "veed/fabric-1.0/fast", "audio_url", 60.0, "assumed"),
            (
                "wavespeed",
                "bytedance/avatar-omni-human-1.5",
                "audio",
                60.0,
                "assumed",
            ),
            ("wavespeed", "veed/fabric-1.0", "audio", 60.0, "assumed"),
            (
                "wavespeed",
                "wavespeed-ai/seedvr2/video",
                "video",
                600.0,
                "stated",
            ),
        ];
        for (provider, model, source, cap, basis) in table {
            let e = c.find(provider, model).unwrap();
            let p = e.describe(today())["pricing"].clone();
            assert_eq!(e.derived_source_param(), Some(source), "{model}");
            assert_eq!(
                (
                    p["reserve_cap_seconds"].clone(),
                    p["reserve_cap_basis"].clone()
                ),
                (json!(cap), json!(basis)),
                "{model}"
            );
            assert_eq!(p["seconds_stated_by_caller"], false, "{model}");
            // The stated hint is the only unsent input, it is optional, and it is never part of the body.
            let hint = e.input("source_seconds").unwrap();
            assert!(
                !hint.send && !hint.required && hint.default.is_none(),
                "{model}"
            );
        }
        // The stated caps equal the largest length the entry's own hint accepts.
        for (model, max) in [
            ("fal-ai/kling-video/v3/pro/motion-control", 30.0),
            ("wavespeed-ai/wan-2.2/animate-2", 120.0),
        ] {
            let e = c.entries().iter().find(|e| e.model == model).unwrap();
            assert_eq!(e.input("source_seconds").unwrap().max, Some(max), "{model}");
        }
    }

    #[test]
    fn catalogue_per_minute_honours_by_adjust_and_bounds() {
        let plain = entry(mini(
            src_inputs(json!({})),
            merge(
                src_price(json!({})),
                json!({"kind": "per_minute", "usd": 5.0}),
            ),
        ))
        .unwrap();
        near(
            quote(
                &plain,
                json!({"video_url": "https://x/v.mp4", "source_seconds": 90}),
            )
            .unwrap()
            .1,
            7.5,
        );
        near(plain.default_estimate(), 5.0 / 60.0);

        let mut ins = src_inputs(json!({}));
        ins.as_array_mut().unwrap().push(json!({"param": "resolution", "json_key": "resolution", "type": "string", "enum": ["480p", "720p"], "required": true}));
        let pricing = merge(
            src_price(json!({})),
            json!({"kind": "per_minute", "usd": 6.0, "by": {"param": "resolution", "values": {"480p": 3.0, "720p": 6.0}},
                "min_billed_s": 60, "round_up_to_s": 15,
                "adjust": [{"when": {"param": "video_url", "present": true}, "add_usd": 0.5}]}),
        );
        let e = entry(mini(ins, pricing)).unwrap();
        let v = "https://x/v.mp4";
        near(
            quote(
                &e,
                json!({"video_url": v, "source_seconds": 100, "resolution": "720p"}),
            )
            .unwrap()
            .1,
            11.0,
        );
        near(
            quote(
                &e,
                json!({"video_url": v, "source_seconds": 30, "resolution": "480p"}),
            )
            .unwrap()
            .1,
            3.5,
        );
        near(e.default_estimate(), 3.5);
        let err = quote(&e, json!({"video_url": v, "source_seconds": 30})).unwrap_err();
        assert_eq!(err.code, "invalid_request");
        assert!(
            err.message.contains("params.resolution is required"),
            "{}",
            err.message
        );
    }

    #[test]
    fn catalogue_by_on_a_required_input_needs_no_default_but_needs_a_price_per_value() {
        let by = json!({"kind": "per_second", "usd": 1.0, "seconds_param": "duration", "by": {"param": "resolution", "values": {"480p": 1.0, "720p": 2.0}}});
        let res = |extra: Value| {
            let base = json!({"param": "resolution", "json_key": "resolution", "type": "string", "enum": ["480p", "720p"]});
            json!([prompt_in(), seconds_in(json!(["5"])), merge(base, extra)])
        };
        rejects(
            mini(res(json!({})), by.clone()),
            "which has no default and is not required",
        );
        let mut no_enum = res(json!({"required": true}));
        no_enum[2].as_object_mut().unwrap().remove("enum");
        rejects(
            mini(no_enum, by.clone()),
            "needs an enum and a price for every value",
        );
        rejects(
            mini(
                res(json!({"required": true, "enum": ["480p", "720p", "1080p"]})),
                by.clone(),
            ),
            "no price for allowed value",
        );
        let e = entry(mini(res(json!({"required": true})), by)).unwrap();
        let err = quote(&e, json!({})).unwrap_err();
        assert_eq!(err.code, "invalid_request");
        assert!(
            err.message.contains("params.resolution is required"),
            "{}",
            err.message
        );
        near(quote(&e, json!({"resolution": "720p"})).unwrap().1, 10.0);
        near(e.default_estimate(), 5.0);
    }

    fn mp_inputs() -> Value {
        json!([prompt_in(), {"param": "megapixels", "type": "number", "send": false, "default": 1, "min": 0.1, "max": 4}])
    }

    #[test]
    fn catalogue_per_megapixel_tiered_flat_and_constant() {
        let tier = json!({"kind": "per_megapixel", "first_usd": 0.07, "additional_usd": 0.03, "megapixels_param": "megapixels"});
        let e = entry(mini(mp_inputs(), tier)).unwrap();
        for (mp, want) in [
            (1.0, 0.07),
            (2.0, 0.10),
            (4.0, 0.16),
            (0.1, 0.07),
            (2.5, 0.115),
        ] {
            near(quote(&e, json!({"megapixels": mp})).unwrap().1, want);
        }
        near(quote(&e, json!({})).unwrap().1, 0.07);
        near(e.default_estimate(), 0.07);
        assert!(quote(&e, json!({"megapixels": 2}))
            .unwrap()
            .0
            .body
            .get("megapixels")
            .is_none());
        assert_eq!(
            quote(&e, json!({"megapixels": 5})).unwrap_err().code,
            "invalid_request"
        );

        let flat = entry(mini(
            mp_inputs(),
            json!({"kind": "per_megapixel", "usd": 0.006, "megapixels_param": "megapixels"}),
        ))
        .unwrap();
        near(quote(&flat, json!({})).unwrap().1, 0.006);
        near(quote(&flat, json!({"megapixels": 2})).unwrap().1, 0.012);
        let fixed = entry(mini(
            json!([prompt_in()]),
            json!({"kind": "per_megapixel", "usd": 0.006, "megapixels": 1.05}),
        ))
        .unwrap();
        near(quote(&fixed, json!({})).unwrap().1, 0.0063);
        near(fixed.default_estimate(), 0.0063);

        let text_in =
            json!([prompt_in(), {"param": "s", "json_key": "s", "type": "string", "default": "a"}]);
        let no_default =
            json!([prompt_in(), {"param": "megapixels", "type": "number", "send": false}]);
        for (inputs, pricing, needle) in [
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "usd": 0.006}),
                "needs megapixels_param or a megapixels constant",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "usd": 0.006, "megapixels_param": "megapixels", "megapixels": 1}),
                "not both",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "first_usd": 0.07, "additional_usd": 0.03, "megapixels": 1}),
                "an input that is not sent must be read by pricing",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "first_usd": 0.07, "additional_usd": 0.03, "megapixels_param": "megapixels", "megapixels": 1}),
                "not both",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "first_usd": 0.07, "additional_usd": 0.03}),
                "a tiered per_megapixel price needs megapixels_param",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "first_usd": 0.07, "megapixels_param": "megapixels"}),
                "either `usd` or both",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "usd": 0.006, "first_usd": 0.07, "additional_usd": 0.03, "megapixels_param": "megapixels"}),
                "either `usd` or both",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "usd": 0.006, "megapixels_param": "ghost"}),
                "not a declared input",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "usd": 0.006, "megapixels_param": "prompt"}),
                "not a declared input",
            ),
            (
                text_in,
                json!({"kind": "per_megapixel", "usd": 0.006, "megapixels_param": "s"}),
                "must be an integer or number",
            ),
            (
                no_default,
                json!({"kind": "per_megapixel", "usd": 0.006, "megapixels_param": "megapixels"}),
                "which has no default and is not required",
            ),
            (
                mp_inputs(),
                json!({"kind": "per_megapixel", "usd": 0.006, "megapixels": -1}),
                "field `pricing.megapixels`: must be a finite amount",
            ),
        ] {
            rejects(mini(inputs, pricing), needle);
        }
    }

    #[test]
    fn catalogue_fractional_durations_on_number_inputs() {
        let dur = json!({"param": "duration", "json_key": "duration", "type": "number", "default": 2.5, "min": 0.92, "max": 15});
        let pricing = json!({"kind": "per_second", "usd": 0.16, "seconds_param": "duration"});
        let e = entry(mini(json!([prompt_in(), dur]), pricing.clone())).unwrap();
        near(e.default_estimate(), 0.4);
        near(quote(&e, json!({"duration": 0.92})).unwrap().1, 0.1472);
        assert_eq!(
            quote(&e, json!({"duration": 0.5})).unwrap_err().code,
            "invalid_request"
        );
        assert_eq!(e.describe(today())["settings"][0]["min"], json!(0.92));
        // The `durations` block holds whole seconds; it is omitted for a fractional entry, never rounded.
        let mut with_block = mini(
            json!([
                prompt_in(),
                json!({"param": "duration", "json_key": "duration", "type": "number", "default": 2.5})
            ]),
            pricing.clone(),
        );
        with_block["durations"] = json!({"min_duration_s": 0.92});
        assert!(entry(with_block).is_err());
        let whole =
            json!({"param": "duration", "json_key": "duration", "type": "integer", "default": 2.5});
        rejects(
            mini(json!([prompt_in(), whole]), pricing),
            "field `inputs[1].default`",
        );
    }

    fn json_in(extra: Value) -> Value {
        merge(
            json!({"param": "extra", "json_key": "extra", "type": "json", "max_bytes": 64}),
            extra,
        )
    }

    #[test]
    fn catalogue_json_input_passes_through_unchanged_within_its_size() {
        let e = entry(mini(
            json!([prompt_in(), json_in(json!({}))]),
            json!({"kind": "flat", "usd": 1.0}),
        ))
        .unwrap();
        // URLs inside a json value are not validated.
        let obj = json!({"a": [1, 2, {"u": "ftp://not-validated"}], "b": null});
        assert_eq!(
            quote(&e, json!({"extra": obj.clone()})).unwrap().0.body["extra"],
            obj
        );
        let arr = json!([1, "two", {"three": 3}]);
        assert_eq!(
            quote(&e, json!({"extra": arr.clone()})).unwrap().0.body["extra"],
            arr
        );
        for wrong in [json!("text"), json!(5), json!(true)] {
            let err = quote(&e, json!({"extra": wrong})).unwrap_err();
            assert!(
                err.message.contains("JSON object or array"),
                "{}",
                err.message
            );
        }
        let err = quote(&e, json!({"extra": {"k": "x".repeat(100)}})).unwrap_err();
        assert_eq!(err.code, "invalid_request");
        assert!(err.message.contains("the limit is 64"), "{}", err.message);
        assert_eq!(e.describe(today())["settings"][0]["max_bytes"], 64);
        entry(mini(
            json!([prompt_in(), json_in(json!({"max_bytes": 65536}))]),
            json!({"kind": "flat", "usd": 1.0}),
        ))
        .unwrap();
    }

    #[test]
    fn catalogue_json_input_is_never_priced_conditioned_or_given_a_role() {
        let flat = json!({"kind": "flat", "usd": 1.0});
        let mut missing = json_in(json!({}));
        missing.as_object_mut().unwrap().remove("max_bytes");
        rejects(
            mini(json!([prompt_in(), missing]), flat.clone()),
            "needs max_bytes from 1 to 65536",
        );
        for (extra, needle) in [
            (
                json!({"max_bytes": 65537}),
                "needs max_bytes from 1 to 65536",
            ),
            (json!({"max_bytes": 0}), "needs max_bytes from 1 to 65536"),
            (
                json!({"role": "video"}),
                "a role needs type url or url_list",
            ),
            (
                json!({"enum": [{"a": 1}]}),
                "url and json inputs take no enum",
            ),
            (json!({"min": 1}), "min and max need type integer or number"),
            (
                json!({"default": {"k": "x".repeat(100)}}),
                "field `inputs[1].default`",
            ),
            (json!({"send": false}), "must be read by pricing"),
        ] {
            rejects(
                mini(json!([prompt_in(), json_in(extra)]), flat.clone()),
                needle,
            );
        }
        let string_with_bytes =
            json!({"param": "s", "json_key": "s", "type": "string", "max_bytes": 10});
        rejects(
            mini(json!([prompt_in(), string_with_bytes]), flat.clone()),
            "only json inputs take max_bytes",
        );
        let ins = json!([prompt_in(), json_in(json!({"default": {"a": 1}}))]);
        rejects(
            mini(
                ins.clone(),
                json!({"kind": "per_second", "usd": 1, "seconds_param": "extra"}),
            ),
            "must be an integer, number or enum_seconds",
        );
        rejects(
            mini(
                ins.clone(),
                json!({"kind": "per_item", "usd": 1, "count_param": "extra"}),
            ),
            "must be an integer or number",
        );
        rejects(
            mini(
                ins.clone(),
                json!({"kind": "flat", "usd": 1, "adjust": [{"when": {"param": "extra", "equals": {"a": 1}}, "multiply": 2}]}),
            ),
            "cannot be tested with equals",
        );
        // `present` is allowed on a json input (non-empty array or object); the value is still never priced by equality.
        let e = entry(mini(ins, json!({"kind": "flat", "usd": 1, "adjust": [{"when": {"param": "extra", "present": true}, "multiply": 2}]}))).unwrap();
        near(quote(&e, json!({})).unwrap().1, 2.0);
        near(quote(&e, json!({"extra": {}})).unwrap().1, 1.0);
        near(quote(&e, json!({"extra": []})).unwrap().1, 1.0);
        near(quote(&e, json!({"extra": [1]})).unwrap().1, 2.0);
    }

    #[test]
    fn catalogue_non_finite_and_negative_amounts_are_refused_at_load_and_at_price() {
        let base = entry(mini(
            json!([prompt_in(), seconds_in(json!(["5"]))]),
            per_s(),
        ))
        .unwrap();
        let check = |pricing: Pricing| {
            let mut e = base.clone();
            e.pricing = pricing;
            e.validate().unwrap_err()
        };
        let flat = |usd: f64, adjust: Vec<Adjust>| Pricing::Flat { usd, adjust };
        for usd in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01] {
            assert!(
                check(flat(usd, vec![])).contains("finite amount of at least 0"),
                "{usd}"
            );
        }
        let adj = |multiply: Option<f64>, add_usd: Option<f64>| Adjust {
            when: When {
                param: "duration".into(),
                equals: Some(json!("5")),
                present: None,
            },
            multiply,
            add_usd,
        };
        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(
                check(flat(1.0, vec![adj(Some(bad), None)])).contains("multiply"),
                "{bad}"
            );
            assert!(
                check(flat(1.0, vec![adj(None, Some(bad))])).contains("add_usd"),
                "{bad}"
            );
        }
        let timed = |min: Option<f64>, max: Option<f64>, round: Option<f64>| Pricing::PerSecond {
            usd: 1.0,
            seconds_param: "duration".into(),
            by: None,
            min_billed_s: min,
            max_billed_s: max,
            round_up_to_s: round,
            derived: None,
            adjust: vec![],
        };
        assert!(check(timed(Some(f64::NAN), None, None)).contains("min_billed_s"));
        assert!(check(timed(None, Some(f64::INFINITY), None)).contains("max_billed_s"));
        assert!(check(timed(None, None, Some(f64::NAN))).contains("round_up_to_s"));
        assert!(check(timed(None, None, Some(0.0))).contains("round_up_to_s"));
        let mp = |usd: Option<f64>, megapixels: Option<f64>| Pricing::PerMegapixel {
            usd,
            first_usd: None,
            additional_usd: None,
            megapixels_param: None,
            megapixels,
            derived: None,
            adjust: vec![],
        };
        assert!(check(mp(Some(f64::NAN), Some(1.0))).contains("pricing.usd"));
        assert!(check(mp(Some(1.0), Some(f64::INFINITY))).contains("pricing.megapixels"));

        // At price time a value that is negative, or that overflows the amount, is refused.
        let num_in =
            json!({"param": "duration", "json_key": "duration", "type": "number", "default": 5});
        let e = entry(mini(
            json!([prompt_in(), num_in]),
            json!({"kind": "per_second", "usd": 10.0, "seconds_param": "duration"}),
        ))
        .unwrap();
        assert_eq!(
            quote(&e, json!({"duration": -5})).unwrap_err().code,
            "invalid_request"
        );
        assert_eq!(
            quote(&e, json!({"duration": 1e308})).unwrap_err().code,
            "invalid_request"
        );
        near(quote(&e, json!({"duration": 0})).unwrap().1, 0.0);
        assert_eq!(
            e.price(&BTreeMap::new()).unwrap_err().code,
            "invalid_request"
        );
    }

    #[test]
    fn catalogue_embedded_entries_price_as_computed_by_hand() {
        let c = Catalogue::load(
            EMBEDDED,
            &scratch("embedded").join("models.json"),
            &crate::providers::is_builtin,
        );
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        // Estimate at defaults (required inputs without a default at their min or first enum value), by hand from each entry.
        let table: [(&str, &str, f64); 61] = [
            ("fal", "alibaba/wan-3.0/image-to-video", 1.0), // 1080p 0.20 x 5 s
            ("fal", "alibaba/wan-3.0/reference-to-video", 1.0), // 1080p 0.20 x 5 s
            ("fal", "minimax/h3-max/text-to-video", 0.4),   // 768P 0.08 x 5 s
            ("fal", "minimax/h3/text-to-video", 0.65),      // 2K 0.13 x 5 s
            ("wavespeed", "bytedance/seedance-2.5/text-to-video", 1.8), // 720p 0.36 x 5 s
            ("wavespeed", "bytedance/seedance-2.5/image-to-video", 1.8),
            ("wavespeed", "alibaba/wan-3.0/text-to-video", 0.5), // 720p 0.10 x 5 s
            ("wavespeed", "alibaba/wan-3.0/image-to-video", 0.5),
            ("wavespeed", "wavespeed-ai/minimax-h3/text-to-video", 0.2), // 480p 0.04 x 5 s
            (
                "wavespeed",
                "kwaivgi/kling-video-o3-pro/text-to-video",
                0.56,
            ), // 0.112 x 5 s
            (
                "wavespeed",
                "kwaivgi/kling-video-o3-pro/image-to-video",
                0.56,
            ),
            ("fal", "fal-ai/kling-video/v3/pro/text-to-video", 0.56), // 0.112 x 5 s, no audio
            ("fal", "fal-ai/kling-video/v3/standard/text-to-video", 0.42), // 0.084 x 5 s
            ("fal", "fal-ai/kling-video/o3/pro/text-to-video", 0.56), // 0.112 x 5 s
            ("fal", "fal-ai/kling-video/v3/pro/motion-control", 5.04), // 0.168 x 30 s: an unmeasured source reserves the 30 s cap
            ("fal", "fal-ai/kling-video/v3/standard/motion-control", 3.78), // 0.126 x 30 s
            ("wavespeed", "kwaivgi/kling-v3.0-std/motion-control", 3.78), // 0.126 x 30 s
            ("wavespeed", "kwaivgi/kling-v3.0-pro/motion-control", 5.04), // 0.168 x 30 s
            ("fal", "fal-ai/sync-lipsync/v2/pro", 50.0), // $5 per minute x the assumed 600 s cap
            ("fal", "fal-ai/flux-2-max", 0.16),          // 4 MP reserved: 0.07 + 3 x 0.03
            ("wavespeed", "wavespeed-ai/wan-2.2/animate-2", 4.8), // 480p 0.04 x 120 s cap
            // Added 2026-10-12.
            ("fal", "fal-ai/ltx-2/retake-video", 0.5), // $0.10 x 5 s default duration
            ("fal", "fal-ai/veo3.1", 1.6),             // 720p 0.20 x 8 s default, silent
            ("fal", "fal-ai/flux-2/lora", 0.02205), // 1024x1024 = 1.048576 MP up to 1.05 x 0.021, no loras
            ("fal", "fal-ai/flux-2/lora/edit", 0.04305), // (1.05 output + 1 input image) MP x 0.021
            ("fal", "bytedance/seedream/v5/lite/text-to-image", 0.035),
            ("fal", "bytedance/seedream/v5/lite/edit", 0.035),
            ("fal", "fal-ai/hidream-o1-image", 0.0105), // 1.05 MP x 0.01
            ("fal", "google/gemini-3.8-flash-tts", 0.045), // 1000 characters assumed
            ("fal", "fal-ai/inworld-tts-2", 0.01),      // 1000 characters assumed
            ("wavespeed", "inworld/realtime-tts-2", 0.05), // 1000 characters assumed
            ("fal", "fal-ai/lyria3", 0.04),
            ("wavespeed", "google/lyria-3.5/music", 0.10),
            ("wavespeed", "google/lyria-3-pro/music", 0.08),
            ("wavespeed", "google/lyria-3-clip/music", 0.04),
            (
                "fal",
                "fal-ai/stable-audio-3/small/sfx/text-to-audio",
                0.0206,
            ),
            // Added with the edit, identity and Seedance 2.5 routes.
            ("fal", "bytedance/seedance-2.5/text-to-video", 2.365), // 720p 0.473 x 5 s
            ("fal", "bytedance/seedance-2.5/image-to-video", 2.365), // 720p 0.473 x 5 s
            ("fal", "fal-ai/kling-video/v3/pro/image-to-video", 0.56), // 0.112 x 5 s, no audio
            ("fal", "fal-ai/kling-video/v3/standard/image-to-video", 0.42), // 0.084 x 5 s, no audio
            ("fal", "fal-ai/pixverse/swap", 1.2), // 720p 0.04 x 30 s stated cap reserved
            ("wavespeed", "pixverse/swap", 3.6),  // 720p 0.12 x 30 s stated cap reserved
            ("fal", "fal-ai/face-swap", 0.01),    // flat
            ("fal", "fal-ai/flux-pro/v1/fill", 0.05), // 1 image x 0.05
            ("fal", "fal-ai/iclight-v2", 0.04),   // 1 image x 0.04
            ("fal", "fal-ai/flux-pro/kontext/max/multi", 0.08), // 1 image x 0.08
            ("fal", "fal-ai/pulid", 0.03),        // 1 image x 0.03
            ("wavespeed", "bytedance/seedream-v4/edit", 0.027), // flat
            ("fal", "fal-ai/kling-video/ai-avatar/v2/pro", 6.9), // 0.115 x 60 s assumed cap
            ("wavespeed", "kwaivgi/kling-v2-ai-avatar-pro", 6.72), // 0.112 x 60 s assumed cap
            ("fal", "bytedance/seedream/v5/pro/text-to-image", 0.135), // tentative top tier, flat
            ("fal", "fal-ai/mai-image-2.5-pro", 0.17), // ~$0.17 average per image, unverified
            // Added with #61-63 (2026-10-17): avatars reserve the assumed 60 s cap, SeedVR2 the stated 600 s cap.
            ("fal", "fal-ai/bytedance/omnihuman/v1.5", 9.6), // 1080p default, 0.16 x 60 s
            ("fal", "bytedance/omnihuman/v2", 6.3),          // 0.105 x 60 s
            ("fal", "veed/fabric-1.0", 4.8), // required resolution takes first enum 480p, 0.08 x 60 s
            ("fal", "veed/fabric-1.0/fast", 6.0), // 480p 0.10 x 60 s
            ("wavespeed", "veed/fabric-1.0", 4.2), // 480p default, 0.07 x 60 s
            ("wavespeed", "bytedance/avatar-omni-human-1.5", 15.0), // 0.25 x 60 s
            (
                "fal",
                "fal-ai/stable-audio-3/small/music/text-to-audio",
                0.0217,
            ), // flat
            ("fal", "fal-ai/stable-audio-3/medium/text-to-audio", 0.0376), // flat
            ("wavespeed", "wavespeed-ai/seedvr2/video", 24.0), // 1080p default, 0.04 x 600 s
        ];
        assert_eq!(
            c.entries().len(),
            table.len(),
            "every embedded entry is in the table"
        );
        for (provider, model, want) in table {
            let e = c
                .find(provider, model)
                .unwrap_or_else(|| panic!("{provider}/{model} is not embedded"));
            let got = e.default_estimate();
            assert!(
                (got - want).abs() < 1e-9,
                "{provider}/{model}: estimate {got}, by hand {want}"
            );
            assert!(
                (e.model_info().est_usd - want).abs() < 1e-9,
                "{provider}/{model}: listed est_usd"
            );
        }

        // Requests against the added entries, on a fixed date so staleness cannot interfere.
        let on = parse_date("2026-10-12").unwrap();
        let go = |provider: &str, model: &str, params: Value| {
            let e = c.find(provider, model).unwrap();
            let r = GenRequest {
                kind: e.kind,
                provider: provider.into(),
                model: model.into(),
                prompt: "p".into(),
                params,
            };
            e.prepare_on(&r, on)
                .and_then(|p| e.price(&p.values).map(|usd| (p, usd)))
        };
        let (p, usd) = go(
            "fal",
            "fal-ai/kling-video/v3/pro/text-to-video",
            json!({"generate_audio": true}),
        )
        .unwrap();
        near(usd, 0.84);
        assert_eq!(p.body["duration"], json!("5"));
        near(
            go(
                "fal",
                "fal-ai/kling-video/v3/pro/text-to-video",
                json!({"generate_audio": true, "duration": 10}),
            )
            .unwrap()
            .1,
            1.68,
        );
        near(
            go(
                "fal",
                "fal-ai/kling-video/v3/standard/text-to-video",
                json!({"generate_audio": true, "duration": "15"}),
            )
            .unwrap()
            .1,
            1.89,
        );
        near(
            go(
                "fal",
                "fal-ai/kling-video/o3/pro/text-to-video",
                json!({"generate_audio": true, "duration": 10}),
            )
            .unwrap()
            .1,
            1.4,
        );
        // The engine derives the billed seconds. A remote video cannot be measured, so the 30 s cap is
        // reserved whatever length the caller states; the stated length is a hint that is never sent or priced.
        let motion = |extra: Value| {
            merge(
                json!({"video_url": "https://x/v.mp4", "image_url": "https://x/i.png"}),
                extra,
            )
        };
        let (p, usd) = go(
            "fal",
            "fal-ai/kling-video/v3/pro/motion-control",
            motion(json!({"source_seconds": 12})),
        )
        .unwrap();
        near(usd, 0.168 * 30.0); // was 2.016 (0.168 x the stated 12 s)
        assert!(p.body.get("source_seconds").is_none());
        near(
            go(
                "fal",
                "fal-ai/kling-video/v3/pro/motion-control",
                motion(json!({"source_seconds": 3})),
            )
            .unwrap()
            .1,
            0.168 * 30.0,
        );
        near(
            go(
                "fal",
                "fal-ai/kling-video/v3/pro/motion-control",
                motion(json!({})),
            )
            .unwrap()
            .1,
            0.168 * 30.0,
        );
        let ws_motion =
            json!({"video": "https://x/v.mp4", "image": "https://x/i.png", "source_seconds": 2});
        assert!(
            go(
                "wavespeed",
                "kwaivgi/kling-v3.0-std/motion-control",
                ws_motion
            )
            .is_err(),
            "a hint below the 3 s minimum is still refused"
        );
        let ws_motion = json!({"video": "https://x/v.mp4", "image": "https://x/i.png"});
        near(
            go(
                "wavespeed",
                "kwaivgi/kling-v3.0-std/motion-control",
                ws_motion,
            )
            .unwrap()
            .1,
            0.126 * 30.0,
        ); // was refused: length not stated
        let lipsync = |extra: Value| {
            merge(
                json!({"video_url": "https://x/v.mp4", "audio_url": "https://x/a.wav"}),
                extra,
            )
        };
        near(
            go(
                "fal",
                "fal-ai/sync-lipsync/v2/pro",
                lipsync(json!({"source_seconds": 90})),
            )
            .unwrap()
            .1,
            50.0,
        ); // was 7.5 (the stated 90 s)
        near(
            go(
                "fal",
                "fal-ai/sync-lipsync/v2/pro",
                lipsync(json!({"source_seconds": 1})),
            )
            .unwrap()
            .1,
            50.0,
        );
        // FLUX.2 Max sends no size, so each image is reserved at the 4 MP the route allows: 0.07 + 3 x 0.03.
        near(go("fal", "fal-ai/flux-2-max", json!({})).unwrap().1, 0.16);
        let err = go("fal", "fal-ai/flux-2-max", json!({"megapixels": 2})).unwrap_err(); // was priced at 2 MP = 0.10
        assert!(
            err.message.contains("is not an input of this model"),
            "{}",
            err.message
        );
        let animate = |extra: Value| {
            merge(
                json!({"video": "https://x/v.mp4", "image": "https://x/i.png", "source_seconds": 100}),
                extra,
            )
        };
        near(
            go(
                "wavespeed",
                "wavespeed-ai/wan-2.2/animate-2",
                animate(json!({"resolution": "720p"})),
            )
            .unwrap()
            .1,
            0.08 * 120.0,
        ); // was 8.0
        near(
            go(
                "wavespeed",
                "wavespeed-ai/wan-2.2/animate-2",
                animate(json!({"resolution": "480p", "source_seconds": 3})),
            )
            .unwrap()
            .1,
            0.04 * 120.0,
        );
        let err = go(
            "wavespeed",
            "wavespeed-ai/wan-2.2/animate-2",
            animate(json!({})),
        )
        .unwrap_err();
        assert!(
            err.message.contains("params.resolution is required"),
            "{}",
            err.message
        );

        // A measured source bills its real length, rounded up to a whole second; a hint more than 1 s off is refused.
        let measured = |provider: &str, model: &str, params: Value, secs: f64| {
            let e = c.find(provider, model).unwrap();
            let r = GenRequest {
                kind: e.kind,
                provider: provider.into(),
                model: model.into(),
                prompt: "p".into(),
                params,
            };
            e.prepare_on(&r, on)
                .and_then(|p| e.price_with(&p.values, Some(secs)))
        };
        near(
            measured(
                "fal",
                "fal-ai/kling-video/v3/pro/motion-control",
                motion(json!({})),
                12.2,
            )
            .unwrap(),
            0.168 * 13.0,
        );
        near(
            measured(
                "fal",
                "fal-ai/kling-video/v3/pro/motion-control",
                motion(json!({"source_seconds": 13})),
                12.2,
            )
            .unwrap(),
            0.168 * 13.0,
        );
        assert!(measured(
            "fal",
            "fal-ai/kling-video/v3/pro/motion-control",
            motion(json!({"source_seconds": 3})),
            12.2
        )
        .is_err());
        assert!(
            measured(
                "fal",
                "fal-ai/kling-video/v3/pro/motion-control",
                motion(json!({})),
                31.0
            )
            .is_err(),
            "over the stated 30 s cap"
        );
        near(
            measured(
                "fal",
                "fal-ai/sync-lipsync/v2/pro",
                lipsync(json!({})),
                700.0,
            )
            .unwrap(),
            5.0 * 700.0 / 60.0,
        ); // an assumed cap never refuses a measured source
        near(
            measured(
                "wavespeed",
                "kwaivgi/kling-v3.0-std/motion-control",
                json!({"video": "https://x/v.mp4", "image": "https://x/i.png"}),
                1.0,
            )
            .unwrap(),
            0.126 * 3.0,
        ); // 3 s minimum
    }

    // ---- derived price bases (2026-10-12): characters, megapixels from the sent size, nested size ----

    /// Prepare then price one request with an explicit prompt, in the entry's own kind.
    fn say(e: &Entry, text: &str, params: Value) -> Result<(Prepared, f64), ProviderError> {
        let r = GenRequest {
            kind: e.kind,
            provider: e.provider.clone(),
            model: e.model.clone(),
            prompt: text.into(),
            params,
        };
        let p = e.prepare(&r)?;
        let usd = e.price(&p.values)?;
        Ok((p, usd))
    }

    fn obj(k: &str, v: Value) -> Value {
        let mut o = json!({});
        o[k] = v;
        o
    }

    fn tts(max_len: Value, pricing_extra: Value) -> Value {
        let mut prompt =
            json!({"param": "prompt", "json_key": "text", "type": "string", "required": true});
        if !max_len.is_null() {
            prompt["max_len"] = max_len;
        }
        let style = json!({"param": "style", "json_key": "style", "type": "string", "max_len": 3});
        let pricing = merge(
            json!({"kind": "per_character", "usd": 0.01, "text_param": "prompt"}),
            pricing_extra,
        );
        let mut v = mini(json!([prompt, style]), pricing);
        v["kind"] = json!("voice");
        v
    }

    #[test]
    fn catalogue_per_character_prices_the_sent_text_in_unicode_scalars() {
        let e = entry(tts(json!(2000), json!({}))).unwrap();
        let (p, usd) = say(&e, &"a".repeat(1000), json!({})).unwrap();
        near(usd, 0.01);
        assert_eq!(
            p.body["text"].as_str().unwrap().len(),
            1000,
            "the priced text is the sent text"
        );
        near(say(&e, "a", json!({})).unwrap().1, 0.00001);
        near(say(&e, &"a".repeat(2000), json!({})).unwrap().1, 0.02);
        // Characters are Unicode scalar values, not bytes: 2-byte and 4-byte characters count once.
        near(say(&e, &"\u{e9}".repeat(1000), json!({})).unwrap().1, 0.01);
        near(
            say(&e, &"\u{1f600}".repeat(500), json!({})).unwrap().1,
            0.005,
        );
        near(say(&e, "e\u{301}", json!({})).unwrap().1, 0.00002);
        // Over the cap is refused before submit, counted in scalars (2001 four-byte characters, 8004 bytes).
        for text in ["a".repeat(2001), "\u{1f600}".repeat(2001)] {
            let err = say(&e, &text, json!({})).unwrap_err();
            assert_eq!(err.code, "invalid_request");
            assert!(
                err.message
                    .contains("params.prompt is 2001 characters; the limit is 2000"),
                "{}",
                err.message
            );
        }
        // Any string input may carry a cap, not only the prompt.
        say(&e, "x", obj("style", json!("abc"))).unwrap();
        let err = say(&e, "x", obj("style", json!("abcd"))).unwrap_err();
        assert!(
            err.message
                .contains("params.style is 4 characters; the limit is 3"),
            "{}",
            err.message
        );
        // A caller cannot state a length or a count: no such input exists.
        for k in ["thousand_chars", "chars", "characters", "count"] {
            let err = say(&e, "x", obj(k, json!(0.001))).unwrap_err();
            assert!(
                err.message.contains("is not an input of this model"),
                "{k}: {}",
                err.message
            );
        }
        // Empty text is never priced as free.
        assert_eq!(say(&e, "", json!({})).unwrap_err().code, "invalid_request");
        assert_eq!(
            e.price(&BTreeMap::new()).unwrap_err().code,
            "invalid_request"
        );
        // The advertised estimate assumes 1000 characters, `est_chars` when given, never above the cap.
        near(e.default_estimate(), 0.01);
        near(
            entry(tts(json!(2000), json!({"est_chars": 250})))
                .unwrap()
                .default_estimate(),
            0.0025,
        );
        near(
            entry(tts(json!(400), json!({})))
                .unwrap()
                .default_estimate(),
            0.004,
        );
        rejects(
            tts(json!(400), json!({"est_chars": 401})),
            "field `pricing.est_chars`",
        );
        rejects(
            tts(json!(400), json!({"est_chars": 0})),
            "field `pricing.est_chars`",
        );
        // Reported by `media modalities`.
        let d = e.describe(today());
        assert_eq!(d["pricing"]["kind"], "per_character");
        assert_eq!(
            (
                d["pricing"]["chars_param"].clone(),
                d["pricing"]["unit_chars"].clone(),
                d["pricing"]["chars_counted_as"].clone(),
                d["pricing"]["chars_stated_by_caller"].clone()
            ),
            (
                json!("prompt"),
                json!(1000),
                json!("unicode_scalar_values"),
                json!(false)
            )
        );
        assert_eq!(d["prompt_max_len"], 2000);
        assert_eq!(
            d["settings"][0],
            json!({"param": "style", "type": "string", "required": false, "max_len": 3})
        );
    }

    #[test]
    fn catalogue_per_character_and_max_len_load_rules() {
        let with_input = |input: Value, pricing_extra: Value| {
            let mut v = tts(json!(null), pricing_extra);
            v["inputs"].as_array_mut().unwrap().push(input);
            v
        };
        rejects(
            with_input(
                json!({"param": "note", "type": "string", "send": false, "default": "a"}),
                json!({"text_param": "note"}),
            ),
            "is not sent, so the caller would state its length",
        );
        rejects(
            with_input(
                json!({"param": "n", "json_key": "n", "type": "number", "default": 1}),
                json!({"text_param": "n"}),
            ),
            "must be a string",
        );
        rejects(
            tts(json!(null), json!({"text_param": "ghost"})),
            "not a declared input",
        );
        rejects(
            tts(json!(null), json!({"text_param": "style"})),
            "which has no default and is not required",
        );
        rejects(tts(json!(null), json!({"usd": -1})), "field `pricing.usd`");
        rejects(
            tts(json!(null), json!({"surprise": 1})),
            "unknown field `surprise`",
        );
        rejects(
            with_input(
                json!({"param": "n", "json_key": "n", "type": "number", "max_len": 3}),
                json!({}),
            ),
            "field `inputs[2].max_len`: only string inputs take max_len",
        );
        rejects(
            tts(json!(0), json!({})),
            "field `inputs[0].max_len`: must be at least 1",
        );
        // The prompt may be priced; an unsent prompt never validates.
        entry(tts(json!(null), json!({}))).unwrap();
    }

    fn sized(images: bool) -> Value {
        let dim = |p: &str| json!({"param": p, "json_key": p, "nest": "image_size", "type": "integer", "default": 1024, "min": 512, "max": 2048});
        let mut derived = json!({"size": {"from": "width_height", "width_param": "width", "height_param": "height"}, "rounding": "assumed"});
        if images {
            derived["input_images"] = json!({"param": "image_urls", "megapixels_each": 1.0});
        }
        let inputs = json!([
            prompt_in(),
            {"param": "image_urls", "json_key": "image_urls", "type": "url_list", "role": "reference", "max_items": 4},
            dim("width"),
            dim("height"),
            {"param": "loras", "json_key": "loras", "type": "json", "max_bytes": 512}
        ]);
        let pricing = json!({"kind": "per_megapixel", "usd": 0.021, "derived": derived,
            "adjust": [{"when": {"param": "loras", "present": true}, "multiply": 2.5}]});
        let mut v = mini(inputs, pricing);
        v["kind"] = json!("image");
        v
    }

    #[test]
    fn catalogue_per_megapixel_derives_megapixels_from_the_sent_size() {
        let e = entry(sized(false)).unwrap();
        // 1024 x 1024 = 1.048576 MP, rounded up to 0.01 MP = 1.05 MP, at $0.021.
        let (p, usd) = say(&e, "p", json!({})).unwrap();
        near(usd, 0.02205);
        near(e.default_estimate(), 0.02205);
        assert_eq!(
            p.body["image_size"],
            json!({"width": 1024, "height": 1024}),
            "the size is sent nested, as the provider takes it"
        );
        assert!(p.body.get("width").is_none() && p.body.get("height").is_none());
        for (w, h, mp) in [
            (2048, 2048, 4.20),
            (512, 512, 0.27),
            (1000, 1000, 1.00),
            (1920, 1080, 2.08),
            (1536, 1024, 1.58),
            (512, 2048, 1.05),
        ] {
            let (p, usd) = say(&e, "p", json!({"width": w, "height": h})).unwrap();
            near(usd, 0.021 * mp);
            assert_eq!(p.body["image_size"], json!({"width": w, "height": h}));
        }
        // A size the provider would refuse is refused here, so the priced size is always the sent size.
        for (k, v, needle) in [
            ("width", json!(3000), "must be at most 2048"),
            ("height", json!(511), "must be at least 512"),
            ("width", json!(1024.5), "must be an integer"),
            ("width", json!("1024"), "must be an integer"),
        ] {
            let err = say(&e, "p", obj(k, v)).unwrap_err();
            assert!(err.message.contains(needle), "{k}: {}", err.message);
        }
        // No number the caller states can move the price.
        for k in ["megapixels", "lora_size_class", "thousand_chars"] {
            let err = say(&e, "p", obj(k, json!(0.001))).unwrap_err();
            assert!(
                err.message.contains("is not an input of this model"),
                "{k}: {}",
                err.message
            );
        }
        // A LoRA's file size is unknowable: any LoRA reserves at the highest multiplier (2.5), none at 1.
        near(
            say(
                &e,
                "p",
                obj(
                    "loras",
                    json!([{"path": "https://x/l.safetensors", "scale": 1}]),
                ),
            )
            .unwrap()
            .1,
            0.02205 * 2.5,
        );
        near(
            say(
                &e,
                "p",
                obj("loras", json!({"path": "https://x/l.safetensors"})),
            )
            .unwrap()
            .1,
            0.02205 * 2.5,
        );
        near(say(&e, "p", obj("loras", json!([]))).unwrap().1, 0.02205);
        let err = say(
            &e,
            "p",
            json!({"lora_size_class": "up_to_2gb", "loras": [{"path": "x"}]}),
        )
        .unwrap_err();
        assert!(
            err.message.contains("is not an input of this model"),
            "{}",
            err.message
        );

        // Input images: a declared megapixel each, counted from the URLs actually sent, then the LoRA multiplier.
        let e = entry(sized(true)).unwrap();
        let urls = |n: usize| {
            json!((0..n)
                .map(|i| format!("https://x/{i}.png"))
                .collect::<Vec<_>>())
        };
        near(say(&e, "p", json!({})).unwrap().1, 0.021 * 1.05);
        near(
            say(&e, "p", obj("image_urls", urls(3))).unwrap().1,
            0.021 * (1.05 + 3.0),
        );
        near(say(&e, "p", json!({"image_urls": urls(2), "loras": [{"path": "x"}], "width": 1536, "height": 1024})).unwrap().1, 0.021 * (1.58 + 2.0) * 2.5);
        assert_eq!(
            say(&e, "p", obj("image_urls", urls(5))).unwrap_err().code,
            "invalid_request",
            "over max_items"
        );

        // A stated step replaces the default; the marking is reported.
        let mut v = sized(false);
        v["pricing"]["derived"]["round_up_to_mp"] = json!(0.5);
        v["pricing"]["derived"]["rounding"] = json!("stated");
        let e = entry(v).unwrap();
        near(say(&e, "p", json!({})).unwrap().1, 0.021 * 1.5);
        near(
            say(&e, "p", json!({"width": 1000, "height": 1000}))
                .unwrap()
                .1,
            0.021 * 1.0,
        );
        let d = e.describe(today());
        assert_eq!(
            (
                d["pricing"]["rounding"].clone(),
                d["pricing"]["round_up_to_mp"].clone()
            ),
            (json!("stated"), json!(0.5))
        );

        // Tiered rates work on the derived total: 2048 x 2048 = 4.20 MP.
        let mut v = sized(false);
        v["pricing"] = json!({"kind": "per_megapixel", "first_usd": 0.07, "additional_usd": 0.03,
            "derived": {"size": {"from": "width_height", "width_param": "width", "height_param": "height"}, "rounding": "assumed"}});
        let e = entry(v).unwrap();
        near(
            say(&e, "p", json!({"width": 2048, "height": 2048}))
                .unwrap()
                .1,
            0.07 + 0.03 * 3.2,
        );

        // Reported by `media modalities`: derived, never caller-stated, and the rounding is assumed.
        let d = entry(sized(true)).unwrap().describe(today());
        assert_eq!(d["pricing"]["kind"], "per_megapixel");
        assert_eq!(d["pricing"]["megapixels_stated_by_caller"], false);
        assert_eq!(
            d["pricing"]["megapixels_derived_from"],
            json!({"width": "width", "height": "height"})
        );
        assert_eq!(
            (
                d["pricing"]["rounding"].clone(),
                d["pricing"]["round_up_to_mp"].clone()
            ),
            (json!("assumed"), json!(0.01))
        );
        assert_eq!(
            d["pricing"]["input_images"],
            json!({"param": "image_urls", "megapixels_each": 1.0})
        );
        // The older caller-stated form says so.
        let d = entry(mini(
            mp_inputs(),
            json!({"kind": "per_megapixel", "usd": 0.006, "megapixels_param": "megapixels"}),
        ))
        .unwrap()
        .describe(today());
        assert_eq!(
            (
                d["pricing"]["megapixels_param"].clone(),
                d["pricing"]["megapixels_stated_by_caller"].clone()
            ),
            (json!("megapixels"), json!(true))
        );
    }

    fn enum_sized(sizes: Value, allowed: Value) -> Value {
        let size = json!({"param": "image_size", "json_key": "image_size", "type": "string", "enum": allowed, "default": "sq"});
        let derived = json!({"size": {"from": "enum", "param": "image_size", "sizes": sizes}, "rounding": "stated"});
        let mut v = mini(
            json!([prompt_in(), size]),
            json!({"kind": "per_megapixel", "usd": 0.01, "derived": derived}),
        );
        v["kind"] = json!("image");
        v
    }

    #[test]
    fn catalogue_per_megapixel_derives_megapixels_from_a_sent_enum_with_a_data_map() {
        let sizes = json!({"sq": [1024, 1024], "wide": [1536, 1024]});
        let e = entry(enum_sized(sizes.clone(), json!(["sq", "wide"]))).unwrap();
        near(say(&e, "p", json!({})).unwrap().1, 0.0105);
        near(e.default_estimate(), 0.0105);
        let (p, usd) = say(&e, "p", obj("image_size", json!("wide"))).unwrap();
        near(usd, 0.0158);
        assert_eq!(
            p.body["image_size"], "wide",
            "the enum value is what is sent"
        );
        assert!(say(&e, "p", obj("image_size", json!("tall")))
            .unwrap_err()
            .message
            .contains("must be one of"));

        // A value with no published pixel size cannot be offered; a size for a value that is not offered is a mistake.
        rejects(
            enum_sized(sizes.clone(), json!(["sq", "wide", "tall"])),
            "no pixel size for allowed value \"tall\"",
        );
        rejects(
            enum_sized(sizes.clone(), json!(["sq"])),
            "\"wide\" is not in the enum",
        );
        rejects(
            enum_sized(
                json!({"sq": [0, 1024], "wide": [1, 1]}),
                json!(["sq", "wide"]),
            ),
            "width and height must be at least 1",
        );
        rejects(enum_sized(json!({}), json!(["sq"])), "must not be empty");
        // The size input must be sent: an unsent one is a number the caller states.
        let mut v = enum_sized(sizes.clone(), json!(["sq", "wide"]));
        v["inputs"][1]["send"] = json!(false);
        rejects(v, "is not sent, so the caller would state the price basis");
        let mut v = enum_sized(sizes.clone(), json!(["sq", "wide"]));
        v["inputs"][1].as_object_mut().unwrap().remove("default");
        rejects(v, "which has no default and is not required");
        let mut v = enum_sized(sizes.clone(), json!(["sq", "wide"]));
        v["inputs"][1].as_object_mut().unwrap().remove("enum");
        v["inputs"][1].as_object_mut().unwrap().remove("default");
        v["inputs"][1]["required"] = json!(true);
        rejects(v, "must be a string with an enum");
    }

    #[test]
    fn catalogue_derived_megapixels_load_rules() {
        let edit = |mutate: &dyn Fn(&mut Value)| {
            let mut v = sized(true);
            mutate(&mut v);
            v
        };
        rejects(
            edit(&|v| {
                v["inputs"][2].as_object_mut().unwrap().remove("max");
            }),
            "needs a min of at least 1 and a max",
        );
        rejects(
            edit(&|v| v["inputs"][3]["min"] = json!(0)),
            "needs a min of at least 1 and a max",
        );
        rejects(
            edit(&|v| v["inputs"][2]["type"] = json!("number")),
            "must be an integer",
        );
        rejects(
            edit(&|v| v["pricing"]["derived"]["size"]["height_param"] = json!("width")),
            "width_param and height_param must differ",
        );
        rejects(
            edit(&|v| v["pricing"]["derived"]["size"]["height_param"] = json!("ghost")),
            "\"ghost\" is not a declared input",
        );
        rejects(
            edit(&|v| v["pricing"]["derived"]["round_up_to_mp"] = json!(0)),
            "field `pricing.derived.round_up_to_mp`",
        );
        rejects(
            edit(&|v| {
                v["pricing"]["derived"]
                    .as_object_mut()
                    .unwrap()
                    .remove("rounding");
            }),
            "missing field `rounding`",
        );
        rejects(
            edit(&|v| v["pricing"]["derived"]["rounding"] = json!("guess")),
            "unknown variant",
        );
        rejects(
            edit(&|v| v["pricing"]["derived"]["input_images"]["param"] = json!("loras")),
            "must be a sent url_list",
        );
        rejects(
            edit(&|v| v["pricing"]["derived"]["input_images"]["megapixels_each"] = json!(-1)),
            "megapixels_each",
        );
        rejects(
            edit(&|v| v["pricing"]["megapixels"] = json!(1)),
            "one source only",
        );
        rejects(
            edit(&|v| v["pricing"]["megapixels_param"] = json!("width")),
            "one source only",
        );
        rejects(
            edit(&|v| {
                v["pricing"]["first_usd"] = json!(0.07);
            }),
            "either `usd` or both",
        );
        rejects(
            edit(&|v| v["pricing"]["surprise"] = json!(1)),
            "unknown field `surprise`",
        );
        rejects(
            edit(&|v| v["pricing"]["derived"]["surprise"] = json!(1)),
            "unknown field `surprise`",
        );
        // An input image count is unbounded without max_items, but still priced per URL actually sent.
        entry(edit(&|v| {
            v["inputs"][1].as_object_mut().unwrap().remove("max_items");
        }))
        .unwrap();
    }

    #[test]
    fn catalogue_nested_inputs_share_one_object_and_cannot_collide() {
        let dim = |p: &str, extra: Value| {
            merge(
                json!({"param": p, "json_key": p, "nest": "size", "type": "integer", "default": 1}),
                extra,
            )
        };
        let flat = json!({"kind": "flat", "usd": 1.0});
        let e = entry(mini(json!([prompt_in(), dim("width", json!({})), dim("height", json!({})), {"param": "n", "json_key": "n", "type": "integer", "default": 2}]), flat.clone())).unwrap();
        let (p, _) = quote(&e, json!({"width": 640})).unwrap();
        assert_eq!(p.body["size"], json!({"width": 640, "height": 1}));
        assert_eq!(p.body["n"], 2);
        assert!(p.body.get("width").is_none());
        // The same key under different parents is two inputs; the same path twice is one input sent twice.
        let other = merge(
            json!({"param": "w2", "json_key": "width", "nest": "other", "type": "integer", "default": 1}),
            json!({}),
        );
        entry(mini(
            json!([prompt_in(), dim("width", json!({})), other]),
            flat.clone(),
        ))
        .unwrap();
        let twice = merge(
            json!({"param": "w2", "json_key": "width", "nest": "size", "type": "integer", "default": 1}),
            json!({}),
        );
        rejects(
            mini(
                json!([prompt_in(), dim("width", json!({})), twice]),
                flat.clone(),
            ),
            "sent more than once",
        );
        let top = json!({"param": "size", "json_key": "size", "type": "integer", "default": 1});
        rejects(
            mini(
                json!([prompt_in(), top, dim("width", json!({}))]),
                flat.clone(),
            ),
            "is also a top-level key of the body",
        );
        let mut with_constant = mini(json!([prompt_in(), dim("width", json!({}))]), flat.clone());
        with_constant["constants"] = json!({"size": 3});
        rejects(with_constant, "is also a top-level key of the body");
        rejects(
            mini(
                json!([prompt_in(), dim("width", json!({"send": false}))]),
                flat.clone(),
            ),
            "a nest needs a non-empty name and a sent input",
        );
        rejects(
            mini(
                json!([prompt_in(), dim("width", json!({"nest": ""}))]),
                flat,
            ),
            "a nest needs a non-empty name and a sent input",
        );
    }

    #[test]
    fn catalogue_embedded_entries_added_on_2026_10_12_are_unreconciled_and_price_only_sent_inputs()
    {
        let c = Catalogue::load(
            EMBEDDED,
            &scratch("embedded-added").join("models.json"),
            &crate::providers::is_builtin,
        );
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        let added: Vec<&Entry> = c
            .entries()
            .iter()
            .filter(|e| e.priced_at == "2026-10-12")
            .collect();
        assert_eq!(added.len(), 15);
        for e in added {
            let id = format!("{}/{}", e.provider, e.model);
            assert_eq!(
                e.reconciled,
                Some(false),
                "{id}: not yet confirmed by a live charge"
            );
            assert_eq!(e.describe(today())["reconciled"], false, "{id}");
            // The estimate never rests on a number the caller states: every input pricing reads is sent.
            for read in e.pricing_reads() {
                let i = e
                    .input(read)
                    .unwrap_or_else(|| panic!("{id}: pricing reads undeclared {read}"));
                assert!(i.send, "{id}: pricing reads {read}, which is not sent");
            }
        }
        // Entries that predate the field say nothing, so their reported shape is unchanged.
        assert_eq!(
            c.find("fal", "fal-ai/flux-2-max")
                .unwrap()
                .describe(today())
                .get("reconciled"),
            None
        );
        // FLUX.2 Max no longer prices a caller-stated number: it reserves its 4 MP maximum, and says so.
        let d = c
            .find("fal", "fal-ai/flux-2-max")
            .unwrap()
            .describe(today());
        assert_eq!(d["pricing"]["megapixels_stated_by_caller"], false);
        assert_eq!(d["pricing"]["megapixels_reserved"], 4.0);
        // No shipped entry prices a number the caller states and the provider never receives.
        for e in c.entries() {
            let d = e.describe(today());
            assert_ne!(
                d["pricing"]["seconds_stated_by_caller"], true,
                "{}/{}",
                e.provider, e.model
            );
            assert_ne!(
                d["pricing"]["megapixels_stated_by_caller"], true,
                "{}/{}",
                e.provider, e.model
            );
        }
    }

    #[test]
    fn catalogue_embedded_additions_price_and_refuse_as_computed_by_hand() {
        let c = Catalogue::load(
            EMBEDDED,
            &scratch("embedded-additions").join("models.json"),
            &crate::providers::is_builtin,
        );
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        let on = parse_date("2026-10-12").unwrap();
        let go = |provider: &str, model: &str, text: &str, params: Value| {
            let e = c
                .find(provider, model)
                .unwrap_or_else(|| panic!("{provider}/{model} is not embedded"));
            let r = GenRequest {
                kind: e.kind,
                provider: provider.into(),
                model: model.into(),
                prompt: text.into(),
                params,
            };
            e.prepare_on(&r, on)
                .and_then(|p| e.price(&p.values).map(|usd| (p, usd)))
        };
        let refused = |r: Result<(Prepared, f64), ProviderError>, needle: &str| {
            let err = r.unwrap_err();
            assert_eq!(err.code, "invalid_request");
            assert!(
                err.message.contains(needle),
                "{} lacks {needle:?}",
                err.message
            );
        };

        // Veo 3.1 standard text-to-video, silent: $0.20/s at 720p and 1080p, $0.40/s at 4k.
        let veo = "fal-ai/veo3.1";
        let (p, usd) = go("fal", veo, "p", json!({"resolution": "4k"})).unwrap();
        near(usd, 8.0 * 0.40);
        assert_eq!(
            (
                p.body["resolution"].clone(),
                p.body["duration"].clone(),
                p.body["generate_audio"].clone()
            ),
            (json!("4k"), json!("8s"), json!(false))
        );
        near(
            go(
                "fal",
                veo,
                "p",
                json!({"resolution": "1080p", "duration": "6s"}),
            )
            .unwrap()
            .1,
            6.0 * 0.20,
        );
        near(
            go("fal", veo, "p", json!({"duration": "4s"})).unwrap().1,
            4.0 * 0.20,
        );
        near(
            go(
                "fal",
                veo,
                "p",
                json!({"resolution": "4k", "duration": "4s"}),
            )
            .unwrap()
            .1,
            4.0 * 0.40,
        );
        refused(
            go("fal", veo, "p", json!({"resolution": "2160p"})),
            "params.resolution must be one of",
        );
        refused(
            go("fal", veo, "p", json!({"generate_audio": true})),
            "is not an input of this model",
        );
        refused(
            go("fal", veo, "p", json!({"duration": 8})),
            "params.duration must be one of",
        );
        refused(
            go("fal", veo, &"a".repeat(20001), json!({})),
            "the limit is 20000",
        );

        // LTX-2 retake: $0.10 per second of the sent duration.
        let ltx = "fal-ai/ltx-2/retake-video";
        let video = json!({"video_url": "https://x/v.mp4"});
        near(go("fal", ltx, "p", video.clone()).unwrap().1, 0.5);
        near(
            go(
                "fal",
                ltx,
                "p",
                merge(video.clone(), json!({"duration": 12})),
            )
            .unwrap()
            .1,
            1.2,
        );
        refused(
            go(
                "fal",
                ltx,
                "p",
                merge(video.clone(), json!({"duration": 1})),
            ),
            "params.duration must be at least 2",
        );
        refused(
            go(
                "fal",
                ltx,
                "p",
                merge(video.clone(), json!({"duration": 21})),
            ),
            "params.duration must be at most 20",
        );
        refused(
            go("fal", ltx, "p", json!({})),
            "params.video_url is required",
        );

        // FLUX.2 Dev LoRA: $0.021 per megapixel of the sent size; any LoRA reserves at x2.5.
        let lora = "fal-ai/flux-2/lora";
        near(go("fal", lora, "p", json!({})).unwrap().1, 0.02205);
        near(
            go("fal", lora, "p", json!({"width": 2048, "height": 2048}))
                .unwrap()
                .1,
            0.021 * 4.2,
        );
        near(
            go(
                "fal",
                lora,
                "p",
                json!({"loras": [{"path": "https://x/l.safetensors"}]}),
            )
            .unwrap()
            .1,
            0.02205 * 2.5,
        );
        refused(
            go("fal", lora, "p", json!({"megapixels": 0.1})),
            "is not an input of this model",
        );
        refused(
            go("fal", lora, "p", json!({"lora_size_class": "up_to_2gb"})),
            "is not an input of this model",
        );
        refused(
            go("fal", lora, "p", json!({"width": 4096})),
            "params.width must be at most 2048",
        );
        let edit = "fal-ai/flux-2/lora/edit";
        let imgs = |n: usize| {
            json!((0..n)
                .map(|i| format!("https://x/{i}.png"))
                .collect::<Vec<_>>())
        };
        near(
            go("fal", edit, "p", json!({"image_urls": imgs(1)}))
                .unwrap()
                .1,
            0.021 * 2.05,
        );
        near(
            go(
                "fal",
                edit,
                "p",
                json!({"image_urls": imgs(2), "width": 1536, "height": 1024}),
            )
            .unwrap()
            .1,
            0.021 * 3.58,
        );
        near(go("fal", edit, "p", json!({"image_urls": imgs(2), "width": 1536, "height": 1024, "loras": [{"path": "x"}]})).unwrap().1, 0.021 * 3.58 * 2.5);
        refused(
            go("fal", edit, "p", json!({"image_urls": imgs(5)})),
            "the limit is 4",
        );
        refused(
            go("fal", edit, "p", json!({})),
            "params.image_urls is required",
        );

        // HiDream-O1-Image: $0.01 per megapixel of the sent size.
        near(
            go("fal", "fal-ai/hidream-o1-image", "p", json!({}))
                .unwrap()
                .1,
            0.0105,
        );
        near(
            go(
                "fal",
                "fal-ai/hidream-o1-image",
                "p",
                json!({"width": 2048, "height": 2048}),
            )
            .unwrap()
            .1,
            0.042,
        );
        refused(
            go(
                "fal",
                "fal-ai/hidream-o1-image",
                "p",
                json!({"reference_image_urls": imgs(1)}),
            ),
            "is not an input of this model",
        );

        // Seedream 5.0 Lite: flat $0.035 per image.
        near(
            go(
                "fal",
                "bytedance/seedream/v5/lite/text-to-image",
                "p",
                json!({"image_size": "auto_4K"}),
            )
            .unwrap()
            .1,
            0.035,
        );
        near(
            go(
                "fal",
                "bytedance/seedream/v5/lite/edit",
                "p",
                json!({"image_urls": imgs(10)}),
            )
            .unwrap()
            .1,
            0.035,
        );
        refused(
            go(
                "fal",
                "bytedance/seedream/v5/lite/edit",
                "p",
                json!({"image_urls": imgs(11)}),
            ),
            "the limit is 10",
        );

        // Per-character voices: price follows the sent text; the provider's cap is enforced before submit.
        near(
            go(
                "fal",
                "google/gemini-3.8-flash-tts",
                &"a".repeat(1000),
                json!({}),
            )
            .unwrap()
            .1,
            0.045,
        );
        near(
            go(
                "fal",
                "google/gemini-3.8-flash-tts",
                "hello world",
                json!({"voice": "Puck"}),
            )
            .unwrap()
            .1,
            0.045 * 11.0 / 1000.0,
        );
        let (p, usd) = go(
            "fal",
            "fal-ai/inworld-tts-2",
            &"a".repeat(2000),
            json!({"voice": "Hank (en)"}),
        )
        .unwrap();
        near(usd, 0.02);
        assert_eq!(
            (
                p.body["text"].as_str().map(str::len),
                p.body["voice"].clone()
            ),
            (Some(2000), json!("Hank (en)"))
        );
        assert!(
            p.body.get("prompt").is_none(),
            "the text goes to the provider as `text`"
        );
        refused(
            go("fal", "fal-ai/inworld-tts-2", &"a".repeat(2001), json!({})),
            "params.prompt is 2001 characters; the limit is 2000",
        );
        near(
            go(
                "wavespeed",
                "inworld/realtime-tts-2",
                &"a".repeat(2000),
                json!({"voice_id": "Hana"}),
            )
            .unwrap()
            .1,
            0.10,
        );
        refused(
            go(
                "wavespeed",
                "inworld/realtime-tts-2",
                &"\u{1f600}".repeat(2001),
                json!({}),
            ),
            "the limit is 2000",
        );
        refused(
            go(
                "wavespeed",
                "inworld/realtime-tts-2",
                "x",
                json!({"voice_id": "Craig (en)"}),
            ),
            "params.voice_id must be one of",
        );
        refused(
            go(
                "fal",
                "fal-ai/inworld-tts-2",
                "x",
                json!({"voice": "Dennis"}),
            ),
            "params.voice must be one of",
        );

        // Music and sound effects.
        near(
            go(
                "fal",
                "fal-ai/lyria3",
                "p",
                json!({"image_url": "https://x/a.png"}),
            )
            .unwrap()
            .1,
            0.04,
        );
        refused(
            go("fal", "fal-ai/lyria3", &"a".repeat(5001), json!({})),
            "the limit is 5000",
        );
        near(
            go("wavespeed", "google/lyria-3.5/music", "p", json!({}))
                .unwrap()
                .1,
            0.10,
        );
        refused(
            go(
                "wavespeed",
                "google/lyria-3.5/music",
                &"a".repeat(5001),
                json!({}),
            ),
            "the limit is 5000",
        );
        near(
            go(
                "wavespeed",
                "google/lyria-3-pro/music",
                &"a".repeat(6000),
                json!({}),
            )
            .unwrap()
            .1,
            0.08,
        );
        near(
            go(
                "wavespeed",
                "google/lyria-3-clip/music",
                "p",
                json!({"image": "https://x/a.png"}),
            )
            .unwrap()
            .1,
            0.04,
        );
        let sfx = "fal-ai/stable-audio-3/small/sfx/text-to-audio";
        near(
            go("fal", sfx, "p", json!({"duration": 90})).unwrap().1,
            0.0206,
        );
        refused(
            go("fal", sfx, "p", json!({"duration": 121})),
            "params.duration must be at most 120",
        );
        refused(
            go("fal", sfx, "p", json!({"output_format": "aiff"})),
            "params.output_format must be one of",
        );
    }

    #[test]
    fn catalogue_embedded_additions_of_2026_10_17_price_and_refuse_as_computed_by_hand() {
        let c = Catalogue::load(
            EMBEDDED,
            &scratch("embedded-added-1017").join("models.json"),
            &crate::providers::is_builtin,
        );
        assert!(c.errors().is_empty(), "{:?}", c.errors());
        let on = parse_date("2026-10-17").unwrap();
        let ids = [
            ("fal", "fal-ai/bytedance/omnihuman/v1.5"),
            ("fal", "bytedance/omnihuman/v2"),
            ("fal", "veed/fabric-1.0"),
            ("fal", "veed/fabric-1.0/fast"),
            ("wavespeed", "veed/fabric-1.0"),
            ("wavespeed", "bytedance/avatar-omni-human-1.5"),
            ("fal", "fal-ai/stable-audio-3/small/music/text-to-audio"),
            ("fal", "fal-ai/stable-audio-3/medium/text-to-audio"),
            ("wavespeed", "wavespeed-ai/seedvr2/video"),
        ];
        let added: Vec<&Entry> = ids
            .iter()
            .map(|(p, m)| {
                c.find(p, m)
                    .unwrap_or_else(|| panic!("{p}/{m} is not embedded"))
            })
            .collect();
        assert_eq!(added.len(), 9);
        for e in &added {
            let id = format!("{}/{}", e.provider, e.model);
            assert_eq!(
                e.reconciled,
                Some(false),
                "{id}: not yet confirmed by a live charge"
            );
            assert!(
                e.price_source.starts_with("https://") && !e.price_source.contains(' '),
                "{id}: {}",
                e.price_source
            );
            // A derived seconds param is measured from the sent source media, so it is never sent itself.
            let measured = e
                .pricing
                .timed()
                .filter(|t| t.derived.is_some())
                .map(|t| t.seconds_param);
            for read in e.pricing_reads() {
                let input = e
                    .input(read)
                    .unwrap_or_else(|| panic!("{id}: pricing reads undeclared {read}"));
                assert!(
                    input.send || measured == Some(read),
                    "{id}: pricing reads {read}, which is not sent"
                );
            }
        }
        let go = |provider: &str, model: &str, params: Value| {
            let e = c
                .find(provider, model)
                .unwrap_or_else(|| panic!("{provider}/{model} is not embedded"));
            let r = GenRequest {
                kind: e.kind,
                provider: provider.into(),
                model: model.into(),
                prompt: "p".into(),
                params,
            };
            e.prepare_on(&r, on)
                .and_then(|p| e.price(&p.values).map(|usd| (p, usd)))
        };
        let refused = |r: Result<(Prepared, f64), ProviderError>, needle: &str| {
            let err = r.unwrap_err();
            assert_eq!(err.code, "invalid_request");
            assert!(
                err.message.contains(needle),
                "{} lacks {needle:?}",
                err.message
            );
        };
        let (img, aud, clip) = ("https://x/a.png", "https://x/a.mp3", "https://x/v.mp4");

        // Avatars: a remote audio file is not measured, so the assumed 60 s cap is reserved at the per-second rate.
        let omni = "fal-ai/bytedance/omnihuman/v1.5";
        let (p, usd) = go(
            "fal",
            omni,
            json!({"image_url": img, "audio_url": aud, "source_seconds": 3}),
        )
        .unwrap();
        near(usd, 60.0 * 0.16);
        assert_eq!(
            (
                p.body["resolution"].clone(),
                p.body.get("source_seconds").is_none()
            ),
            (json!("1080p"), true)
        );
        refused(
            go(
                "fal",
                omni,
                json!({"image_url": img, "audio_url": aud, "resolution": "480p"}),
            ),
            "params.resolution must be one of",
        );
        refused(
            go("fal", omni, json!({"image_url": img})),
            "params.audio_url is required",
        );
        near(
            go(
                "fal",
                "bytedance/omnihuman/v2",
                json!({"image_url": img, "audio_url": aud}),
            )
            .unwrap()
            .1,
            60.0 * 0.105,
        );
        near(
            go(
                "wavespeed",
                "bytedance/avatar-omni-human-1.5",
                json!({"image": img, "audio": aud}),
            )
            .unwrap()
            .1,
            60.0 * 0.25,
        );
        refused(
            go(
                "wavespeed",
                "bytedance/avatar-omni-human-1.5",
                json!({"image": img}),
            ),
            "params.audio is required",
        );
        // VEED Fabric 1.0: the fal rows require a resolution and price each; the WaveSpeed row defaults to 480p.
        let fab = |model: &str, res: &str| {
            go(
                "fal",
                model,
                json!({"image_url": img, "audio_url": aud, "resolution": res}),
            )
            .unwrap()
            .1
        };
        near(fab("veed/fabric-1.0", "480p"), 60.0 * 0.08);
        near(fab("veed/fabric-1.0", "720p"), 60.0 * 0.15);
        near(fab("veed/fabric-1.0/fast", "480p"), 60.0 * 0.10);
        near(fab("veed/fabric-1.0/fast", "720p"), 60.0 * 0.20);
        refused(
            go(
                "fal",
                "veed/fabric-1.0",
                json!({"image_url": img, "audio_url": aud}),
            ),
            "params.resolution is required",
        );
        refused(
            go(
                "fal",
                "veed/fabric-1.0",
                json!({"image_url": img, "audio_url": aud, "resolution": "1080p"}),
            ),
            "params.resolution must be one of",
        );
        near(
            go(
                "wavespeed",
                "veed/fabric-1.0",
                json!({"image": img, "audio": aud}),
            )
            .unwrap()
            .1,
            60.0 * 0.07,
        );
        near(
            go(
                "wavespeed",
                "veed/fabric-1.0",
                json!({"image": img, "audio": aud, "resolution": "720p"}),
            )
            .unwrap()
            .1,
            60.0 * 0.14,
        );

        // SeedVR2 video upscale on WaveSpeed: $0.02/$0.04/$0.06/$0.08 per second by target resolution; the stated 600 s cap is reserved.
        let seedvr = "wavespeed-ai/seedvr2/video";
        near(
            go("wavespeed", seedvr, json!({"video": clip})).unwrap().1,
            600.0 * 0.04,
        );
        for (res, rate) in [("720p", 0.02), ("1080p", 0.04), ("2k", 0.06), ("4k", 0.08)] {
            near(
                go(
                    "wavespeed",
                    seedvr,
                    json!({"video": clip, "target_resolution": res}),
                )
                .unwrap()
                .1,
                600.0 * rate,
            );
        }
        refused(
            go(
                "wavespeed",
                seedvr,
                json!({"video": clip, "target_resolution": "8k"}),
            ),
            "params.target_resolution must be one of",
        );
        refused(
            go(
                "wavespeed",
                seedvr,
                json!({"video": clip, "source_seconds": 601}),
            ),
            "params.source_seconds must be at most 600",
        );
        refused(
            go("wavespeed", seedvr, json!({})),
            "params.video is required",
        );

        // Stable Audio 3 music and medium text-to-audio: a flat price per audio.
        let small = "fal-ai/stable-audio-3/small/music/text-to-audio";
        near(
            go("fal", small, json!({"duration": 120})).unwrap().1,
            0.0217,
        );
        refused(
            go("fal", small, json!({"duration": 121})),
            "params.duration must be at most 120",
        );
        let medium = "fal-ai/stable-audio-3/medium/text-to-audio";
        near(
            go("fal", medium, json!({"duration": 380})).unwrap().1,
            0.0376,
        );
        refused(
            go("fal", medium, json!({"duration": 381})),
            "params.duration must be at most 380",
        );
        refused(
            go("fal", medium, json!({"output_format": "aiff"})),
            "params.output_format must be one of",
        );
    }

    fn video_with_extras(model: &str, extra: &str) -> Entry {
        let mut v = sample("fal", model);
        v["kind"] = json!("video");
        v["provider_params"] = json!([extra]);
        v["inputs"] = json!([
            {"param": "prompt", "json_key": "prompt", "type": "string", "required": true},
            {"param": "duration", "json_key": "duration", "type": "integer", "default": 5, "min": 3, "max": 15}
        ]);
        v["pricing"] = json!({"kind": "per_second", "usd": 0.1, "seconds_param": "duration"});
        entry(v).unwrap()
    }

    fn video_req(e: &Entry, params: Value) -> GenRequest {
        GenRequest {
            kind: Kind::Video,
            provider: "fal".into(),
            model: e.model.clone(),
            prompt: "p".into(),
            params,
        }
    }

    #[test]
    fn provider_params_kling_camera_control_merges_only_documented_keys() {
        let e = video_with_extras("fal-ai/kling-video/v3/pro/image-to-video", "camera_control");
        let cam = json!({"type": "simple", "config": {"zoom": -5}});
        let body = e
            .prepare(&video_req(
                &e,
                json!({"provider_params": {"camera_control": cam, "bogus": 1}}),
            ))
            .unwrap()
            .body;
        assert_eq!(body["camera_control"], cam);
        assert!(body.get("bogus").is_none(), "unknown key dropped: {body}");
        assert!(e
            .prepare(&video_req(&e, json!({"provider_params": "x"})))
            .is_err());
        assert!(
            e.prepare(&video_req(&e, json!({"camera_control": cam})))
                .is_err(),
            "top-level undeclared params stay rejected"
        );
    }

    #[test]
    fn provider_params_seedance_camera_fixed_merges_only_documented_keys() {
        let e = video_with_extras("bytedance/seedance-2.5/text-to-video", "camera_fixed");
        let body = e
            .prepare(&video_req(
                &e,
                json!({"provider_params": {"camera_fixed": true, "camera_control": {}}}),
            ))
            .unwrap()
            .body;
        assert_eq!(body["camera_fixed"], true);
        assert!(body.get("camera_control").is_none(), "{body}");
    }
}
