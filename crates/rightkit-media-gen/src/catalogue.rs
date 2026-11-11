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
        || (crate::fal_storage::local_path(s).is_some() && !s.contains(' ') && s.trim() == s)
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
    /// A caller that holds the source as bytes measures it with [`rightkit_ffmpeg::quickprobe::probe_file`] and
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
