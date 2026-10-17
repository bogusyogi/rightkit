//! AcroForm: list fields, set values, regenerate appearances (feature `forms`).
//!
//! [`list_fields`] reads the form; [`set_values`] changes values and returns new document bytes
//! (the file is written whole, so a digital signature does not survive: see [`FormError::
//! SignaturesPresent`]). Design reference: pdfcraft `crates/forms` (MIT OR Apache-2.0; no code is
//! copied). Auto-detection of fields in unlabelled printed forms is not part of this module.
//!
//! # Typed refusals, no silent substitution
//!
//! A request that cannot be honoured exactly is an error and nothing is written:
//!
//! - unknown or ambiguous field name, wrong value kind for the field, read-only field, value longer
//!   than `/MaxLen`, a choice that is not an option (an editable combo box accepts free text),
//!   several values for a field that is not multi-select;
//! - a character the field's font cannot show ([`FormError::UnsupportedCharacters`]): the text is
//!   never replaced by `?` or dropped;
//! - password, file-select and comb fields, and buttons ([`FormError::UnsupportedField`]);
//! - an XFA form ([`FormError::Xfa`]) and a signed document ([`FormError::SignaturesPresent`])
//!   unless the caller says in [`FormOptions`] that dropping them is acceptable.
//!
//! # Appearances and `/NeedAppearances`
//!
//! Text and combo-box fields get a generated appearance stream (background and border from `/MK`,
//! clipped text, `/Q` alignment, auto font size, line wrapping for multiline fields) so the value
//! shows in every viewer. This needs a simple font (Type1 or TrueType with `WinAnsiEncoding`, or
//! no `/Encoding` for ASCII) named in the field's `/DA` and present in `/AcroForm /DR`, and, for
//! alignment and wrapping, glyph widths (`/Widths` or a Standard-14 face). When a field cannot get
//! one for a reason that is about the font and not about the text (an embedded Type0 font, a
//! rotated widget, a list box, no `/DA`), the value is still stored and the field is named in
//! [`FillOutcome::without_appearance`], and `/AcroForm /NeedAppearances` is set so a viewer draws
//! it. Check boxes and radio buttons only switch `/V` and `/AS` between appearance states the file
//! already has ([`FormError::NoOnState`] when there is none).
//!
//! Text values are stored as ASCII or UTF-16BE; values read back are decoded from UTF-16BE/LE,
//! UTF-8 or (otherwise) as Latin-1, which differs from PDFDocEncoding only in a few punctuation
//! codes.

use std::collections::HashSet;

use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::Serialize;

use crate::inspect::{deref_obj, load_pdf, object_number, resolve_inherited, standard14_width};

#[derive(Debug, thiserror::Error)]
pub enum FormError {
    #[error("document has no AcroForm")]
    NoForm,
    #[error("PDF could not be parsed: {0}")]
    Parse(String),
    #[error("encrypted PDF cannot be filled")]
    Encrypted,
    #[error(
        "the form is an XFA form; filling its AcroForm fields would not change what a viewer shows"
    )]
    Xfa,
    #[error("the document is signed; rewriting it invalidates the signature")]
    SignaturesPresent,
    #[error("no field named {0:?}")]
    FieldNotFound(String),
    #[error("more than one field is named {0:?}")]
    AmbiguousName(String),
    #[error("field {name:?} is a {found}, not a {expected}")]
    WrongKind {
        name: String,
        expected: &'static str,
        found: &'static str,
    },
    #[error("field {0:?} is read-only")]
    ReadOnly(String),
    #[error("field {name:?} holds at most {max} characters, the value has {len}")]
    TooLong { name: String, max: u32, len: usize },
    #[error("{value:?} is not an option of field {name:?}")]
    NotAnOption { name: String, value: String },
    #[error("field {name:?}: {reason}")]
    InvalidValue { name: String, reason: &'static str },
    #[error("field {name:?} is not supported: {reason}")]
    UnsupportedField { name: String, reason: &'static str },
    #[error("field {name:?}: its font cannot show {chars:?}")]
    UnsupportedCharacters { name: String, chars: Vec<char> },
    #[error("field {0:?} has no appearance state for the requested value")]
    NoOnState(String),
    #[error("written form failed verification: {0}")]
    Verification(String),
    #[error("serialize failed: {0}")]
    Write(String),
}

impl FormError {
    /// Stable snake_case code.
    pub fn code(&self) -> &'static str {
        match self {
            FormError::NoForm => "no_form",
            FormError::Parse(_) => "parse",
            FormError::Encrypted => "encrypted",
            FormError::Xfa => "xfa",
            FormError::SignaturesPresent => "signatures_present",
            FormError::FieldNotFound(_) => "field_not_found",
            FormError::AmbiguousName(_) => "ambiguous_name",
            FormError::WrongKind { .. } => "wrong_kind",
            FormError::ReadOnly(_) => "read_only",
            FormError::TooLong { .. } => "too_long",
            FormError::NotAnOption { .. } => "not_an_option",
            FormError::InvalidValue { .. } => "invalid_value",
            FormError::UnsupportedField { .. } => "unsupported_field",
            FormError::UnsupportedCharacters { .. } => "unsupported_characters",
            FormError::NoOnState(_) => "no_on_state",
            FormError::Verification(_) => "verification_failed",
            FormError::Write(_) => "write",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Checkbox,
    Radio,
    Combo,
    List,
    PushButton,
    Signature,
    Unknown,
}

impl FieldKind {
    fn label(self) -> &'static str {
        match self {
            FieldKind::Text => "text field",
            FieldKind::Checkbox => "check box",
            FieldKind::Radio => "radio group",
            FieldKind::Combo => "combo box",
            FieldKind::List => "list box",
            FieldKind::PushButton => "push button",
            FieldKind::Signature => "signature field",
            FieldKind::Unknown => "field of unknown type",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FieldOption {
    pub export: String,
    pub label: String,
}

/// One terminal field of the form.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    /// Fully qualified name: the partial names from the root, joined by `.`.
    pub name: String,
    pub kind: FieldKind,
    /// Text, the selected export value of a choice, or the state name of a button (`Off` when off).
    pub value: Option<String>,
    pub default_value: Option<String>,
    /// Selected export values of a multi-select list box.
    pub selected: Vec<String>,
    /// Buttons: whether the field is in an on state.
    pub checked: Option<bool>,
    pub options: Vec<FieldOption>,
    /// Buttons: appearance states other than `Off`.
    pub on_states: Vec<String>,
    pub read_only: bool,
    pub required: bool,
    pub multiline: bool,
    pub password: bool,
    pub multi_select: bool,
    pub editable_combo: bool,
    pub max_len: Option<u32>,
    pub widget_count: usize,
}

/// A value to set.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    /// Text field text, or the text of an editable combo box.
    Text(String),
    /// Check box on or off.
    Checked(bool),
    /// One option (export value) of a combo box, list box or radio group, or a check box state name.
    Choice(String),
    /// Several options of a multi-select list box.
    Choices(Vec<String>),
    /// Back to the field's `/DV` (or empty / off when it has none).
    Reset,
}

#[derive(Clone, Debug, Default)]
pub struct FormOptions {
    /// Remove `/AcroForm /XFA` instead of refusing an XFA form.
    pub drop_xfa: bool,
    /// Write a signed document anyway (the signature stops validating).
    pub allow_invalidating_signatures: bool,
}

#[derive(Debug)]
pub struct FillOutcome {
    pub bytes: Vec<u8>,
    /// Fields whose value was set and an appearance stream generated.
    pub regenerated: Vec<String>,
    /// Fields whose value was set but that got no generated appearance (see the module docs).
    pub without_appearance: Vec<String>,
    /// True when `/NeedAppearances` was set because of `without_appearance`.
    pub need_appearances: bool,
}

// ---------------------------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------------------------

const FF_READ_ONLY: i64 = 1;
const FF_REQUIRED: i64 = 1 << 1;
const FF_MULTILINE: i64 = 1 << 12;
const FF_PASSWORD: i64 = 1 << 13;
const FF_NO_TOGGLE_TO_OFF: i64 = 1 << 14;
const FF_RADIO: i64 = 1 << 15;
const FF_PUSHBUTTON: i64 = 1 << 16;
const FF_COMBO: i64 = 1 << 17;
const FF_EDIT: i64 = 1 << 18;
const FF_FILE_SELECT: i64 = 1 << 20;
const FF_MULTI_SELECT: i64 = 1 << 21;
const FF_COMB: i64 = 1 << 24;

const MAX_DEPTH: usize = 32;
const MAX_FIELDS: usize = 200_000;

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// Decodes a PDF text string (see the module docs for the PDFDocEncoding approximation).
fn text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes(*p))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes(*p))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    bytes.iter().map(|b| char::from(*b)).collect()
}

/// A text value as the PDF object to store: ASCII as is, anything else as UTF-16BE with a BOM.
fn text_object(text: &str) -> Object {
    if text
        .chars()
        .all(|c| matches!(c, '\n' | '\r' | '\t') || (' '..='~').contains(&c))
    {
        return Object::String(text.as_bytes().to_vec(), StringFormat::Literal);
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Literal)
}

fn object_text(doc: &Document, object: &Object) -> Option<String> {
    match deref_obj(doc, object)? {
        Object::String(bytes, _) => Some(text_string(bytes)),
        Object::Name(name) => Some(String::from_utf8_lossy(name).into_owned()),
        Object::Stream(stream) => stream.get_plain_content().ok().map(|b| text_string(&b)),
        _ => None,
    }
}

struct Terminal {
    id: ObjectId,
    name: String,
    ft: Vec<u8>,
    ff: i64,
    widgets: Vec<ObjectId>,
}

fn acroform(doc: &Document) -> Result<(Option<ObjectId>, Dictionary), FormError> {
    let catalog = doc.catalog().map_err(|_| FormError::NoForm)?;
    let entry = catalog.get(b"AcroForm").map_err(|_| FormError::NoForm)?;
    let id = entry.as_reference().ok();
    let dict = deref_obj(doc, entry)
        .and_then(|o| o.as_dict().ok())
        .ok_or(FormError::NoForm)?;
    Ok((id, dict.clone()))
}

fn dict_of(doc: &Document, id: ObjectId) -> Option<&Dictionary> {
    doc.get_object(id).ok().and_then(|o| o.as_dict().ok())
}

fn is_widget(dict: &Dictionary) -> bool {
    dict.get(b"Subtype")
        .ok()
        .and_then(|s| s.as_name().ok())
        .is_some_and(|name| name == &b"Widget"[..])
}

fn kids_of(doc: &Document, dict: &Dictionary) -> Vec<ObjectId> {
    match dict.get(b"Kids").ok().and_then(|o| deref_obj(doc, o)) {
        Some(Object::Array(items)) => items.iter().filter_map(|o| o.as_reference().ok()).collect(),
        _ => Vec::new(),
    }
}

fn collect_fields(doc: &Document, acro: &Dictionary) -> Result<Vec<Terminal>, FormError> {
    let mut out: Vec<Terminal> = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let roots: Vec<ObjectId> = match acro.get(b"Fields").ok().and_then(|o| deref_obj(doc, o)) {
        Some(Object::Array(items)) => items.iter().filter_map(|o| o.as_reference().ok()).collect(),
        _ => Vec::new(),
    };
    // (field id, qualified name of its parent, depth)
    let mut stack: Vec<(ObjectId, String, usize)> = roots
        .into_iter()
        .rev()
        .map(|id| (id, String::new(), 0))
        .collect();
    while let Some((id, parent_name, depth)) = stack.pop() {
        if depth > MAX_DEPTH || !seen.insert(id) {
            continue;
        }
        if out.len() >= MAX_FIELDS {
            return Err(FormError::Parse("too many form fields".to_string()));
        }
        let Some(dict) = dict_of(doc, id) else {
            continue;
        };
        let partial = dict
            .get(b"T")
            .ok()
            .and_then(|t| object_text(doc, t))
            .unwrap_or_default();
        let name = match (parent_name.is_empty(), partial.is_empty()) {
            (_, true) => parent_name.clone(),
            (true, false) => partial.clone(),
            (false, false) => format!("{parent_name}.{partial}"),
        };
        let kids = kids_of(doc, dict);
        let field_kids: Vec<ObjectId> = kids
            .iter()
            .copied()
            .filter(|kid| dict_of(doc, *kid).is_some_and(|k| k.has(b"T")))
            .collect();
        if !field_kids.is_empty() {
            for kid in kids.iter().rev() {
                stack.push((*kid, name.clone(), depth + 1));
            }
            continue;
        }
        let ft = resolve_inherited(doc, dict, b"FT")
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_name().ok())
            .map(|n| n.to_vec())
            .unwrap_or_default();
        let ff = resolve_inherited(doc, dict, b"Ff")
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_i64().ok())
            .unwrap_or(0);
        let mut widgets: Vec<ObjectId> = Vec::new();
        if is_widget(dict) || dict.has(b"Rect") {
            widgets.push(id);
        }
        for kid in &kids {
            if dict_of(doc, *kid).is_some_and(is_widget) {
                widgets.push(*kid);
            }
        }
        out.push(Terminal {
            id,
            name,
            ft,
            ff,
            widgets,
        });
    }
    Ok(out)
}

fn kind_of(terminal: &Terminal) -> FieldKind {
    match terminal.ft.as_slice() {
        b"Tx" => FieldKind::Text,
        b"Ch" if terminal.ff & FF_COMBO != 0 => FieldKind::Combo,
        b"Ch" => FieldKind::List,
        b"Btn" if terminal.ff & FF_PUSHBUTTON != 0 => FieldKind::PushButton,
        b"Btn" if terminal.ff & FF_RADIO != 0 => FieldKind::Radio,
        b"Btn" => FieldKind::Checkbox,
        b"Sig" => FieldKind::Signature,
        _ => FieldKind::Unknown,
    }
}

/// Appearance states other than `Off` that the widgets of a button field define.
fn on_states(doc: &Document, terminal: &Terminal) -> Vec<String> {
    let mut states: Vec<String> = Vec::new();
    for widget in &terminal.widgets {
        for name in widget_states(doc, *widget) {
            if name != "Off" && !states.contains(&name) {
                states.push(name);
            }
        }
    }
    states
}

fn widget_states(doc: &Document, widget: ObjectId) -> Vec<String> {
    let Some(dict) = dict_of(doc, widget) else {
        return Vec::new();
    };
    let normal = dict
        .get(b"AP")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
        .and_then(|ap| ap.get(b"N").ok())
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok());
    normal
        .map(|n| {
            n.iter()
                .map(|(key, _)| String::from_utf8_lossy(key).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn options_of(doc: &Document, dict: &Dictionary) -> Vec<FieldOption> {
    let Some(Object::Array(items)) =
        resolve_inherited(doc, dict, b"Opt").and_then(|o| deref_obj(doc, o))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match deref_obj(doc, item)? {
            Object::Array(pair) if pair.len() >= 2 => Some(FieldOption {
                export: object_text(doc, &pair[0])?,
                label: object_text(doc, &pair[1])?,
            }),
            other => {
                let text = object_text(doc, other)?;
                Some(FieldOption {
                    export: text.clone(),
                    label: text,
                })
            }
        })
        .collect()
}

fn describe(doc: &Document, terminal: &Terminal) -> FormField {
    let dict = dict_of(doc, terminal.id);
    let kind = kind_of(terminal);
    let value_object = dict.and_then(|d| resolve_inherited(doc, d, b"V"));
    let mut selected: Vec<String> = Vec::new();
    let mut value = None;
    if let Some(object) = value_object.and_then(|o| deref_obj(doc, o)) {
        if let Object::Array(items) = object {
            selected = items.iter().filter_map(|i| object_text(doc, i)).collect();
            value = selected.first().cloned();
        } else {
            value = object_text(doc, object);
        }
    }
    let default_value = dict
        .and_then(|d| resolve_inherited(doc, d, b"DV"))
        .and_then(|o| object_text(doc, o));
    let states = on_states(doc, terminal);
    let mut checked = None;
    if matches!(kind, FieldKind::Checkbox | FieldKind::Radio) {
        let state = value.clone().or_else(|| {
            terminal.widgets.iter().find_map(|w| {
                dict_of(doc, *w)
                    .and_then(|d| d.get(b"AS").ok())
                    .and_then(|a| a.as_name().ok())
                    .map(|n| String::from_utf8_lossy(n).into_owned())
            })
        });
        checked = Some(state.as_deref().is_some_and(|s| s != "Off"));
        if value.is_none() {
            value = state;
        }
    }
    FormField {
        name: terminal.name.clone(),
        kind,
        value,
        default_value,
        selected,
        checked,
        options: dict.map(|d| options_of(doc, d)).unwrap_or_default(),
        on_states: states,
        read_only: terminal.ff & FF_READ_ONLY != 0,
        required: terminal.ff & FF_REQUIRED != 0,
        multiline: terminal.ff & FF_MULTILINE != 0,
        password: terminal.ff & FF_PASSWORD != 0,
        multi_select: terminal.ff & FF_MULTI_SELECT != 0,
        editable_combo: kind == FieldKind::Combo && terminal.ff & FF_EDIT != 0,
        max_len: dict
            .and_then(|d| resolve_inherited(doc, d, b"MaxLen"))
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_i64().ok())
            .and_then(|n| u32::try_from(n).ok()),
        widget_count: terminal.widgets.len(),
    }
}

fn load(bytes: &[u8]) -> Result<Document, FormError> {
    let doc = load_pdf(bytes).map_err(|e| FormError::Parse(e.to_string()))?;
    if doc.is_encrypted() {
        return Err(FormError::Encrypted);
    }
    Ok(doc)
}

/// Lists every terminal field of the form, in document order.
pub fn list_fields(bytes: &[u8]) -> Result<Vec<FormField>, FormError> {
    let doc = load(bytes)?;
    let (_, acro) = acroform(&doc)?;
    let terminals = collect_fields(&doc, &acro)?;
    Ok(terminals.iter().map(|t| describe(&doc, t)).collect())
}

// ---------------------------------------------------------------------------------------------
// Appearance generation
// ---------------------------------------------------------------------------------------------

/// Why an appearance was not generated.
enum AppearanceFailure {
    /// The request itself is wrong (characters the font cannot show): an error.
    Hard(FormError),
    /// This module cannot draw this field: the value is kept and a viewer is asked to draw it.
    Soft,
}

#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    WinAnsi,
    AsciiOnly,
}

const WIN_ANSI_EXTRAS: [(char, u8); 27] = [
    ('\u{20AC}', 0x80),
    ('\u{201A}', 0x82),
    ('\u{0192}', 0x83),
    ('\u{201E}', 0x84),
    ('\u{2026}', 0x85),
    ('\u{2020}', 0x86),
    ('\u{2021}', 0x87),
    ('\u{02C6}', 0x88),
    ('\u{2030}', 0x89),
    ('\u{0160}', 0x8A),
    ('\u{2039}', 0x8B),
    ('\u{0152}', 0x8C),
    ('\u{017D}', 0x8E),
    ('\u{2018}', 0x91),
    ('\u{2019}', 0x92),
    ('\u{201C}', 0x93),
    ('\u{201D}', 0x94),
    ('\u{2022}', 0x95),
    ('\u{2013}', 0x96),
    ('\u{2014}', 0x97),
    ('\u{02DC}', 0x98),
    ('\u{2122}', 0x99),
    ('\u{0161}', 0x9A),
    ('\u{203A}', 0x9B),
    ('\u{0153}', 0x9C),
    ('\u{017E}', 0x9E),
    ('\u{0178}', 0x9F),
];

/// Character codes for `text`, or the characters the encoding cannot show.
fn encode_text(text: &str, encoding: Encoding) -> Result<Vec<u8>, Vec<char>> {
    let mut out = Vec::with_capacity(text.len());
    let mut missing: Vec<char> = Vec::new();
    for c in text.chars() {
        let code = if (' '..='~').contains(&c) {
            Some(c as u8)
        } else if encoding == Encoding::WinAnsi {
            if ('\u{A0}'..='\u{FF}').contains(&c) {
                Some(c as u32 as u8)
            } else {
                WIN_ANSI_EXTRAS
                    .iter()
                    .find(|(ch, _)| *ch == c)
                    .map(|(_, code)| *code)
            }
        } else {
            None
        };
        match code {
            Some(code) => out.push(code),
            None => {
                if !missing.contains(&c) {
                    missing.push(c);
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(missing)
    }
}

struct FontInfo {
    id: ObjectId,
    name: Vec<u8>,
    encoding: Encoding,
    base_font: Vec<u8>,
    first_char: i64,
    widths: Vec<f64>,
}

impl FontInfo {
    /// Advance of `code` in 1000ths of a text-space unit.
    fn width(&self, code: u8) -> Option<f64> {
        let index = i64::from(code) - self.first_char;
        if index >= 0 {
            if let Some(width) = self.widths.get(index as usize) {
                return Some(*width);
            }
        }
        standard14_width(&self.base_font, u32::from(code))
    }

    fn text_width(&self, codes: &[u8]) -> Option<f64> {
        codes.iter().map(|c| self.width(*c)).sum::<Option<f64>>()
    }
}

fn font_info(doc: &Document, acro: &Dictionary, name: &[u8]) -> Option<FontInfo> {
    let fonts = acro
        .get(b"DR")
        .ok()
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())
        .and_then(|dr| dr.get(b"Font").ok())
        .and_then(|o| deref_obj(doc, o))
        .and_then(|o| o.as_dict().ok())?;
    let entry = fonts.get(name).ok()?;
    let id = entry.as_reference().ok()?;
    let font = deref_obj(doc, entry)?.as_dict().ok()?;
    match font.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) {
        Some(b"Type1") | Some(b"TrueType") | Some(b"MMType1") => {}
        _ => return None,
    }
    let base_font = font
        .get(b"BaseFont")
        .ok()
        .and_then(|b| b.as_name().ok())
        .unwrap_or_default()
        .to_vec();
    if base_font.starts_with(b"ZapfDingbats") || base_font.starts_with(b"Symbol") {
        return None;
    }
    let encoding = match font.get(b"Encoding").ok().and_then(|o| deref_obj(doc, o)) {
        None => Encoding::AsciiOnly,
        Some(Object::Name(n)) if n == b"WinAnsiEncoding" => Encoding::WinAnsi,
        Some(Object::Dictionary(d))
            if !d.has(b"Differences")
                && d.get(b"BaseEncoding")
                    .ok()
                    .and_then(|b| b.as_name().ok())
                    .is_some_and(|b| b == &b"WinAnsiEncoding"[..]) =>
        {
            Encoding::WinAnsi
        }
        _ => return None,
    };
    let first_char = font
        .get(b"FirstChar")
        .ok()
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    let widths = match font.get(b"Widths").ok().and_then(|o| deref_obj(doc, o)) {
        Some(Object::Array(items)) => items
            .iter()
            .map(|i| deref_obj(doc, i).and_then(object_number).unwrap_or(0.0))
            .collect(),
        _ => Vec::new(),
    };
    Some(FontInfo {
        id,
        name: name.to_vec(),
        encoding,
        base_font,
        first_char,
        widths,
    })
}

/// Default appearance string parsed into font name, size and colour operators.
struct DefaultAppearance {
    font: Vec<u8>,
    size: f64,
    colour: Vec<Operation>,
}

fn parse_da(da: &[u8]) -> Option<DefaultAppearance> {
    let text = String::from_utf8_lossy(da).into_owned();
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let position = tokens.iter().position(|t| *t == "Tf")?;
    if position < 2 {
        return None;
    }
    let font = tokens[position - 2].strip_prefix('/')?.as_bytes().to_vec();
    let size: f64 = tokens[position - 1].parse().ok()?;
    let mut colour = Vec::new();
    let mut operands: Vec<Object> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if index + 2 >= position && index <= position {
            continue;
        }
        if let Ok(number) = token.parse::<f64>() {
            operands.push(Object::Real(number as f32));
        } else if matches!(*token, "g" | "rg" | "k") {
            colour.push(Operation::new(token, std::mem::take(&mut operands)));
        } else {
            operands.clear();
        }
    }
    Some(DefaultAppearance { font, size, colour })
}

fn colour_ops(array: &Object, doc: &Document, fill: bool) -> Option<Operation> {
    let items = deref_obj(doc, array)?.as_array().ok()?;
    let values: Vec<Object> = items
        .iter()
        .filter_map(|i| deref_obj(doc, i).and_then(object_number))
        .map(|v| Object::Real(v as f32))
        .collect();
    let op = match (values.len(), fill) {
        (1, true) => "g",
        (1, false) => "G",
        (3, true) => "rg",
        (3, false) => "RG",
        (4, true) => "k",
        (4, false) => "K",
        _ => return None,
    };
    Some(Operation::new(op, values))
}

fn real(v: f64) -> Object {
    Object::Real(v as f32)
}

/// Lines for a multiline value: hard breaks, then wrapping at spaces to `max_width` points.
fn wrap_lines(text: &str, font: &FontInfo, size: f64, max_width: f64) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    for paragraph in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ') {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            let codes = encode_text(&candidate, font.encoding).ok()?;
            let width = font.text_width(&codes)? * size / 1000.0;
            if width > max_width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    Some(lines)
}

/// Builds and attaches `/AP /N` for one widget showing `text`.
#[allow(clippy::too_many_arguments)]
fn generate_text_appearance(
    doc: &mut Document,
    acro: &Dictionary,
    field_name: &str,
    field_id: ObjectId,
    widget_id: ObjectId,
    flags: i64,
    text: &str,
) -> Result<(), AppearanceFailure> {
    let soft = |_: ()| AppearanceFailure::Soft;
    let (width, height, mk, border_width, quadding, da_bytes) = {
        let widget = dict_of(doc, widget_id).ok_or(AppearanceFailure::Soft)?;
        let rect = widget
            .get(b"Rect")
            .ok()
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_array().ok())
            .map(|a| a.iter().filter_map(object_number).collect::<Vec<f64>>())
            .filter(|n| n.len() == 4)
            .ok_or(AppearanceFailure::Soft)?;
        let width = (rect[2] - rect[0]).abs();
        let height = (rect[3] - rect[1]).abs();
        if !(width > 1.0 && height > 1.0) || !width.is_finite() || !height.is_finite() {
            return Err(AppearanceFailure::Soft);
        }
        let mk = widget
            .get(b"MK")
            .ok()
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_default();
        // A rotated widget needs a rotated matrix: left to the viewer.
        if mk.get(b"R").ok().and_then(|r| r.as_i64().ok()).unwrap_or(0) % 360 != 0 {
            return Err(AppearanceFailure::Soft);
        }
        let border_width = widget
            .get(b"BS")
            .ok()
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_dict().ok())
            .and_then(|bs| bs.get(b"W").ok())
            .and_then(object_number)
            .unwrap_or(if mk.has(b"BC") { 1.0 } else { 0.0 })
            .clamp(0.0, 10.0);
        let quadding = resolve_inherited(doc, dict_of(doc, field_id).unwrap_or(widget), b"Q")
            .and_then(|o| deref_obj(doc, o))
            .and_then(|o| o.as_i64().ok())
            .or_else(|| acro.get(b"Q").ok().and_then(|o| o.as_i64().ok()))
            .unwrap_or(0);
        let da = resolve_inherited(doc, dict_of(doc, field_id).unwrap_or(widget), b"DA")
            .and_then(|o| deref_obj(doc, o))
            .or_else(|| acro.get(b"DA").ok())
            .and_then(|o| match o {
                Object::String(bytes, _) => Some(bytes.clone()),
                _ => None,
            })
            .ok_or(AppearanceFailure::Soft)?;
        (width, height, mk, border_width, quadding, da)
    };
    let da = parse_da(&da_bytes).ok_or(AppearanceFailure::Soft)?;
    let font = font_info(doc, acro, &da.font).ok_or(AppearanceFailure::Soft)?;
    let multiline = flags & FF_MULTILINE != 0;
    let codes = encode_text(text, font.encoding).map_err(|chars| {
        AppearanceFailure::Hard(FormError::UnsupportedCharacters {
            name: field_name.to_string(),
            chars,
        })
    })?;

    let pad = 2.0 + border_width;
    let inner_width = (width - 2.0 * pad).max(1.0);
    let inner_height = (height - 2.0 * border_width).max(1.0);
    let mut size = da.size;
    if size <= 0.0 {
        size = if multiline {
            12.0
        } else {
            ((height - 2.0 * border_width - 2.0) * 0.78).clamp(4.0, 12.0)
        };
        if !multiline {
            if let Some(natural) = font.text_width(&codes).filter(|w| *w > 0.0) {
                size = size.min((inner_width / natural * 1000.0).max(4.0));
            }
        }
    }

    let lines: Vec<String> = if multiline {
        wrap_lines(text, &font, size, inner_width).ok_or(AppearanceFailure::Soft)?
    } else {
        vec![text.to_string()]
    };
    let leading = size * 1.15;

    let mut ops: Vec<Operation> = vec![Operation::new("BMC", vec![Object::Name(b"Tx".to_vec())])];
    ops.push(Operation::new("q", vec![]));
    if let Some(bg) = mk.get(b"BG").ok().and_then(|a| colour_ops(a, doc, true)) {
        ops.push(bg);
        ops.push(Operation::new(
            "re",
            vec![real(0.0), real(0.0), real(width), real(height)],
        ));
        ops.push(Operation::new("f", vec![]));
    }
    if border_width > 0.0 {
        if let Some(bc) = mk.get(b"BC").ok().and_then(|a| colour_ops(a, doc, false)) {
            ops.push(bc);
            ops.push(Operation::new("w", vec![real(border_width)]));
            ops.push(Operation::new(
                "re",
                vec![
                    real(border_width / 2.0),
                    real(border_width / 2.0),
                    real(width - border_width),
                    real(height - border_width),
                ],
            ));
            ops.push(Operation::new("S", vec![]));
        }
    }
    ops.push(Operation::new(
        "re",
        vec![
            real(border_width),
            real(border_width),
            real(width - 2.0 * border_width),
            real(inner_height),
        ],
    ));
    ops.push(Operation::new("W", vec![]));
    ops.push(Operation::new("n", vec![]));
    ops.push(Operation::new("BT", vec![]));
    ops.push(Operation::new(
        "Tf",
        vec![Object::Name(font.name.clone()), real(size)],
    ));
    ops.extend(da.colour.iter().cloned());
    let mut previous = (0.0, 0.0);
    for (index, line) in lines.iter().enumerate() {
        let line_codes = encode_text(line, font.encoding).map_err(|chars| {
            AppearanceFailure::Hard(FormError::UnsupportedCharacters {
                name: field_name.to_string(),
                chars,
            })
        })?;
        let line_width = if quadding == 0 {
            0.0
        } else {
            font.text_width(&line_codes)
                .ok_or(AppearanceFailure::Soft)?
                * size
                / 1000.0
        };
        let x = match quadding {
            1 => (inner_width - line_width) / 2.0 + pad,
            2 => inner_width - line_width + pad,
            _ => pad,
        };
        let y = if multiline {
            height - pad - size * 0.9 - leading * index as f64
        } else {
            height / 2.0 - size * 0.255
        };
        ops.push(Operation::new(
            "Td",
            vec![real(x - previous.0), real(y - previous.1)],
        ));
        previous = (x, y);
        ops.push(Operation::new(
            "Tj",
            vec![Object::String(line_codes, StringFormat::Literal)],
        ));
    }
    ops.push(Operation::new("ET", vec![]));
    ops.push(Operation::new("Q", vec![]));
    ops.push(Operation::new("EMC", vec![]));
    let content = Content { operations: ops }.encode().map_err(|_| soft(()))?;

    let mut form = Dictionary::new();
    form.set("Type", Object::Name(b"XObject".to_vec()));
    form.set("Subtype", Object::Name(b"Form".to_vec()));
    form.set(
        "BBox",
        Object::Array(vec![real(0.0), real(0.0), real(width), real(height)]),
    );
    let mut font_resources = Dictionary::new();
    font_resources.set(font.name.clone(), Object::Reference(font.id));
    let mut resources = Dictionary::new();
    resources.set("Font", Object::Dictionary(font_resources));
    form.set("Resources", Object::Dictionary(resources));
    let stream_id = doc.add_object(Stream::new(form, content));
    let mut ap = Dictionary::new();
    ap.set("N", Object::Reference(stream_id));
    match doc.get_object_mut(widget_id) {
        Ok(Object::Dictionary(widget)) => {
            widget.set("AP", Object::Dictionary(ap));
            Ok(())
        }
        _ => Err(AppearanceFailure::Soft),
    }
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

/// What reading the written file must show for a field.
enum Expect {
    Text(String),
    State(String),
    Choices(Vec<String>),
    Empty,
}

fn field_dict_mut(doc: &mut Document, id: ObjectId) -> Result<&mut Dictionary, FormError> {
    match doc.get_object_mut(id) {
        Ok(Object::Dictionary(dict)) => Ok(dict),
        _ => Err(FormError::Write("field vanished".to_string())),
    }
}

fn set_widget_state(doc: &mut Document, widget: ObjectId, state: &str) {
    let available = widget_states(doc, widget);
    let target = if available.iter().any(|s| s == state) {
        state
    } else {
        "Off"
    };
    if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(widget) {
        dict.set("AS", Object::Name(target.as_bytes().to_vec()));
    }
}

/// Sets `values` and returns the new bytes. See the module documentation for what is refused.
pub fn set_values(
    bytes: &[u8],
    values: &[(String, FieldValue)],
    options: &FormOptions,
) -> Result<FillOutcome, FormError> {
    let mut doc = load(bytes)?;
    let (acro_id, acro) = acroform(&doc)?;
    if acro.has(b"XFA") && !options.drop_xfa {
        return Err(FormError::Xfa);
    }
    let signed = acro
        .get(b"SigFlags")
        .ok()
        .and_then(|f| f.as_i64().ok())
        .is_some_and(|flags| flags & 1 != 0);
    if signed && !options.allow_invalidating_signatures {
        return Err(FormError::SignaturesPresent);
    }
    let terminals = collect_fields(&doc, &acro)?;
    let mut expectations: Vec<(String, Expect)> = Vec::new();
    let mut regenerated: Vec<String> = Vec::new();
    let mut without_appearance: Vec<String> = Vec::new();

    for (name, value) in values {
        let mut matching = terminals.iter().filter(|t| &t.name == name);
        let terminal = matching
            .next()
            .ok_or_else(|| FormError::FieldNotFound(name.clone()))?;
        if matching.next().is_some() {
            return Err(FormError::AmbiguousName(name.clone()));
        }
        let field = describe(&doc, terminal);
        if field.read_only {
            return Err(FormError::ReadOnly(name.clone()));
        }
        let wrong = |expected: &'static str| FormError::WrongKind {
            name: name.clone(),
            expected,
            found: field.kind.label(),
        };
        let unsupported = |reason: &'static str| FormError::UnsupportedField {
            name: name.clone(),
            reason,
        };
        match field.kind {
            FieldKind::PushButton => return Err(unsupported("push buttons hold no value")),
            FieldKind::Signature => return Err(unsupported("signature fields cannot be filled")),
            FieldKind::Unknown => return Err(unsupported("unknown field type")),
            _ => {}
        }
        if terminal.ff & (FF_PASSWORD | FF_FILE_SELECT | FF_COMB) != 0
            && field.kind == FieldKind::Text
        {
            return Err(unsupported("password, file-select and comb fields"));
        }

        // The value to store and the text to draw (when the field draws text).
        let mut draw: Option<String> = None;
        match (field.kind, value) {
            (FieldKind::Text, FieldValue::Text(text)) => {
                if let Some(max) = field.max_len {
                    let len = text.chars().count();
                    if len > max as usize {
                        return Err(FormError::TooLong {
                            name: name.clone(),
                            max,
                            len,
                        });
                    }
                }
                if !field.multiline && text.contains(['\n', '\r']) {
                    return Err(FormError::InvalidValue {
                        name: name.clone(),
                        reason: "line break in a single-line field",
                    });
                }
                let dict = field_dict_mut(&mut doc, terminal.id)?;
                dict.set("V", text_object(text));
                dict.remove(b"RV");
                draw = Some(text.clone());
                expectations.push((name.clone(), Expect::Text(text.clone())));
            }
            (FieldKind::Text, FieldValue::Reset) => {
                let default = field.default_value.clone().unwrap_or_default();
                let dict = field_dict_mut(&mut doc, terminal.id)?;
                match &field.default_value {
                    Some(dv) => dict.set("V", text_object(dv)),
                    None => {
                        dict.remove(b"V");
                    }
                }
                dict.remove(b"RV");
                draw = Some(default.clone());
                expectations.push((
                    name.clone(),
                    if default.is_empty() {
                        Expect::Empty
                    } else {
                        Expect::Text(default)
                    },
                ));
            }
            (FieldKind::Text, _) => return Err(wrong("text value")),
            (FieldKind::Combo, FieldValue::Choice(choice))
            | (FieldKind::Combo, FieldValue::Text(choice)) => {
                let option = field.options.iter().find(|o| &o.export == choice);
                if option.is_none() && !field.editable_combo {
                    return Err(FormError::NotAnOption {
                        name: name.clone(),
                        value: choice.clone(),
                    });
                }
                let shown = option
                    .map(|o| o.label.clone())
                    .unwrap_or_else(|| choice.clone());
                let dict = field_dict_mut(&mut doc, terminal.id)?;
                dict.set("V", text_object(choice));
                dict.remove(b"I");
                draw = Some(shown);
                expectations.push((name.clone(), Expect::Text(choice.clone())));
            }
            (FieldKind::Combo | FieldKind::List, FieldValue::Reset) => {
                let dict = field_dict_mut(&mut doc, terminal.id)?;
                dict.remove(b"V");
                dict.remove(b"I");
                draw = Some(String::new());
                expectations.push((name.clone(), Expect::Empty));
            }
            (FieldKind::List, FieldValue::Choice(_) | FieldValue::Choices(_)) => {
                let chosen: Vec<String> = match value {
                    FieldValue::Choice(c) => vec![c.clone()],
                    FieldValue::Choices(list) => list.clone(),
                    _ => Vec::new(),
                };
                if chosen.len() > 1 && !field.multi_select {
                    return Err(FormError::InvalidValue {
                        name: name.clone(),
                        reason: "several values for a list box that is not multi-select",
                    });
                }
                let mut indices: Vec<i64> = Vec::new();
                for choice in &chosen {
                    let index = field
                        .options
                        .iter()
                        .position(|o| &o.export == choice)
                        .ok_or_else(|| FormError::NotAnOption {
                            name: name.clone(),
                            value: choice.clone(),
                        })?;
                    indices.push(index as i64);
                }
                indices.sort_unstable();
                indices.dedup();
                let dict = field_dict_mut(&mut doc, terminal.id)?;
                match chosen.as_slice() {
                    [] => {
                        dict.remove(b"V");
                    }
                    [one] => dict.set("V", text_object(one)),
                    many => dict.set(
                        "V",
                        Object::Array(many.iter().map(|c| text_object(c)).collect()),
                    ),
                }
                if indices.is_empty() {
                    dict.remove(b"I");
                } else {
                    dict.set(
                        "I",
                        Object::Array(indices.into_iter().map(Object::Integer).collect()),
                    );
                }
                expectations.push((
                    name.clone(),
                    if chosen.is_empty() {
                        Expect::Empty
                    } else {
                        Expect::Choices(chosen)
                    },
                ));
            }
            (FieldKind::Combo | FieldKind::List, _) => return Err(wrong("choice")),
            (FieldKind::Checkbox | FieldKind::Radio, requested) => {
                let target: String = match (field.kind, requested) {
                    (FieldKind::Checkbox, FieldValue::Checked(true)) => field
                        .on_states
                        .first()
                        .cloned()
                        .ok_or_else(|| FormError::NoOnState(name.clone()))?,
                    (_, FieldValue::Checked(false)) | (_, FieldValue::Reset) => {
                        match (&field.default_value, requested) {
                            (Some(dv), FieldValue::Reset) if dv != "Off" => dv.clone(),
                            _ => "Off".to_string(),
                        }
                    }
                    (_, FieldValue::Choice(state)) => state.clone(),
                    _ => return Err(wrong("on/off state")),
                };
                if target != "Off" && !field.on_states.contains(&target) {
                    return Err(FormError::NoOnState(name.clone()));
                }
                if target == "Off"
                    && terminal.ff & FF_NO_TOGGLE_TO_OFF != 0
                    && field.kind == FieldKind::Radio
                    && !matches!(requested, FieldValue::Reset)
                {
                    return Err(unsupported("this radio group cannot be switched off"));
                }
                field_dict_mut(&mut doc, terminal.id)?
                    .set("V", Object::Name(target.as_bytes().to_vec()));
                for widget in &terminal.widgets {
                    set_widget_state(&mut doc, *widget, &target);
                }
                expectations.push((name.clone(), Expect::State(target)));
            }
            (FieldKind::PushButton | FieldKind::Signature | FieldKind::Unknown, _) => {
                return Err(unsupported("field type"))
            }
        }

        // Appearance of a field that draws text.
        let draws_text = matches!(field.kind, FieldKind::Text | FieldKind::Combo);
        if draws_text {
            let text = draw.unwrap_or_default();
            let mut all_ok = !terminal.widgets.is_empty();
            for widget in &terminal.widgets {
                match generate_text_appearance(
                    &mut doc,
                    &acro,
                    name,
                    terminal.id,
                    *widget,
                    terminal.ff,
                    &text,
                ) {
                    Ok(()) => {}
                    Err(AppearanceFailure::Hard(error)) => return Err(error),
                    Err(AppearanceFailure::Soft) => all_ok = false,
                }
            }
            if all_ok {
                regenerated.push(name.clone());
            } else {
                without_appearance.push(name.clone());
            }
        } else if field.kind == FieldKind::List {
            without_appearance.push(name.clone());
        }
    }

    let need_appearances = !without_appearance.is_empty();
    if let Some(id) = acro_id {
        if let Ok(Object::Dictionary(form)) = doc.get_object_mut(id) {
            if need_appearances {
                form.set("NeedAppearances", Object::Boolean(true));
            }
            if options.drop_xfa {
                form.remove(b"XFA");
            }
        }
    } else if let Ok(catalog_id) = doc
        .trailer
        .get(b"Root")
        .map_err(|_| FormError::NoForm)
        .and_then(|r| r.as_reference().map_err(|_| FormError::NoForm))
    {
        // A direct /AcroForm dictionary lives in the catalog.
        if let Ok(Object::Dictionary(catalog)) = doc.get_object_mut(catalog_id) {
            if let Ok(Object::Dictionary(form)) = catalog.get_mut(b"AcroForm") {
                if need_appearances {
                    form.set("NeedAppearances", Object::Boolean(true));
                }
                if options.drop_xfa {
                    form.remove(b"XFA");
                }
            }
        }
    }
    doc.prune_objects();
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| FormError::Write(e.to_string()))?;
    verify_written(&out, &expectations)?;
    Ok(FillOutcome {
        bytes: out,
        regenerated,
        without_appearance,
        need_appearances,
    })
}

/// Sets every field back to its default (`/DV`, or empty / off). `names` limits it to those fields.
pub fn reset_fields(
    bytes: &[u8],
    names: Option<&[String]>,
    options: &FormOptions,
) -> Result<FillOutcome, FormError> {
    let fields = list_fields(bytes)?;
    let values: Vec<(String, FieldValue)> = fields
        .into_iter()
        .filter(|f| {
            !f.read_only
                && matches!(
                    f.kind,
                    FieldKind::Text
                        | FieldKind::Checkbox
                        | FieldKind::Radio
                        | FieldKind::Combo
                        | FieldKind::List
                )
                && names.is_none_or(|list| list.contains(&f.name))
        })
        .map(|f| (f.name, FieldValue::Reset))
        .collect();
    set_values(bytes, &values, options)
}

/// Reads the written file back and compares what it holds with what was requested.
fn verify_written(bytes: &[u8], expectations: &[(String, Expect)]) -> Result<(), FormError> {
    let fields = list_fields(bytes).map_err(|e| FormError::Verification(e.to_string()))?;
    for (name, expect) in expectations {
        let field = fields
            .iter()
            .find(|f| &f.name == name)
            .ok_or_else(|| FormError::Verification(format!("field {name:?} is gone")))?;
        let ok = match expect {
            Expect::Text(text) => field.value.as_deref() == Some(text.as_str()),
            Expect::State(state) => {
                field.value.as_deref() == Some(state.as_str())
                    || (state == "Off" && field.checked == Some(false))
            }
            Expect::Choices(chosen) => {
                if chosen.len() == 1 {
                    field.value.as_deref() == Some(chosen[0].as_str())
                } else {
                    &field.selected == chosen
                }
            }
            Expect::Empty => {
                field.value.as_deref().is_none_or(str::is_empty) && field.selected.is_empty()
            }
        };
        if !ok {
            return Err(FormError::Verification(format!(
                "field {name:?} does not read back as written"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    /// One page with: text field `name` (Helv 12, /DR font), check box `agree` (states Yes/Off),
    /// radio group `colour` (Red/Green), combo `size` (S, M, L), and a read-only text field `id`.
    fn form_pdf(extra_acro: Option<(&str, Object)>) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let helv = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
            "Encoding" => "WinAnsiEncoding",
        });
        let on_stream = |doc: &mut Document| {
            doc.add_object(Stream::new(
                dictionary! { "Subtype" => "Form", "BBox" => Object::Array(vec![
                    Object::Integer(0), Object::Integer(0), Object::Integer(10), Object::Integer(10)]) },
                b"0 0 10 10 re f".to_vec(),
            ))
        };
        let rect = |x: i64, y: i64| {
            Object::Array(vec![
                Object::Integer(x),
                Object::Integer(y),
                Object::Integer(x + 100),
                Object::Integer(y + 20),
            ])
        };
        let da = |s: &str| Object::String(s.as_bytes().to_vec(), StringFormat::Literal);
        let text = doc.add_object(dictionary! {
            "FT" => "Tx", "T" => da("name"), "Subtype" => "Widget", "Rect" => rect(50, 700),
            "DA" => da("/Helv 12 Tf 0 g"), "MaxLen" => 10,
        });
        let id_field = doc.add_object(dictionary! {
            "FT" => "Tx", "T" => da("id"), "Ff" => 1, "Subtype" => "Widget", "Rect" => rect(50, 660),
            "DA" => da("/Helv 12 Tf 0 g"),
        });
        let on = on_stream(&mut doc);
        let off = on_stream(&mut doc);
        let check = doc.add_object(dictionary! {
            "FT" => "Btn", "T" => da("agree"), "Subtype" => "Widget", "Rect" => rect(50, 620),
            "AS" => "Off",
            "AP" => dictionary! { "N" => dictionary! { "Yes" => on, "Off" => off } },
        });
        let radio = doc.new_object_id();
        let mut kids = Vec::new();
        for (index, state) in ["Red", "Green"].iter().enumerate() {
            let on = on_stream(&mut doc);
            let off = on_stream(&mut doc);
            kids.push(Object::Reference(doc.add_object(dictionary! {
                "Subtype" => "Widget", "Parent" => radio, "Rect" => rect(50, 560 - 30 * index as i64),
                "AS" => "Off",
                "AP" => dictionary! { "N" => dictionary! { *state => on, "Off" => off } },
            })));
        }
        doc.objects.insert(
            radio,
            Object::Dictionary(dictionary! {
                "FT" => "Btn", "Ff" => FF_RADIO, "T" => da("colour"), "Kids" => Object::Array(kids),
            }),
        );
        let combo = doc.add_object(dictionary! {
            "FT" => "Ch", "Ff" => FF_COMBO, "T" => da("size"), "Subtype" => "Widget", "Rect" => rect(50, 500),
            "DA" => da("/Helv 0 Tf 0 g"),
            "Opt" => Object::Array(vec![
                Object::Array(vec![da("S"), da("Small")]),
                Object::Array(vec![da("M"), da("Medium")]),
                Object::Array(vec![da("L"), da("Large")]),
            ]),
        });
        let fields = [text, id_field, check, radio, combo];
        let mut acro = dictionary! {
            "Fields" => Object::Array(fields.iter().map(|f| Object::Reference(*f)).collect()),
            "DA" => da("/Helv 0 Tf 0 g"),
            "DR" => dictionary! { "Font" => dictionary! { "Helv" => helv } },
        };
        if let Some((key, value)) = extra_acro {
            acro.set(key, value);
        }
        let mut annots: Vec<Object> = vec![text, id_field, check, combo]
            .into_iter()
            .map(Object::Reference)
            .collect();
        if let Ok(Object::Dictionary(radio_dict)) = doc.get_object(radio).cloned() {
            if let Ok(Object::Array(k)) = radio_dict.get(b"Kids").cloned() {
                annots.extend(k);
            }
        }
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Annots" => Object::Array(annots),
            "MediaBox" => Object::Array(vec![
                Object::Integer(0), Object::Integer(0), Object::Integer(612), Object::Integer(792)]),
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => Object::Array(vec![Object::Reference(page)]), "Count" => 1,
            }),
        );
        let catalog = doc.add_object(dictionary! {
            "Type" => "Catalog", "Pages" => pages_id, "AcroForm" => acro,
        });
        doc.trailer.set("Root", Object::Reference(catalog));
        let mut out = Vec::new();
        doc.save_to(&mut out).expect("fixture saves");
        out
    }

    fn set(bytes: &[u8], name: &str, value: FieldValue) -> Result<FillOutcome, FormError> {
        set_values(bytes, &[(name.to_string(), value)], &FormOptions::default())
    }

    fn field(bytes: &[u8], name: &str) -> FormField {
        list_fields(bytes)
            .unwrap()
            .into_iter()
            .find(|f| f.name == name)
            .unwrap()
    }

    #[test]
    fn lists_the_fields_with_kinds_and_states() {
        let fields = list_fields(&form_pdf(None)).unwrap();
        let kinds: Vec<(&str, FieldKind)> =
            fields.iter().map(|f| (f.name.as_str(), f.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("name", FieldKind::Text),
                ("id", FieldKind::Text),
                ("agree", FieldKind::Checkbox),
                ("colour", FieldKind::Radio),
                ("size", FieldKind::Combo),
            ]
        );
        assert!(fields[1].read_only);
        assert_eq!(fields[2].on_states, ["Yes"]);
        assert_eq!(fields[3].widget_count, 2);
        assert_eq!(fields[4].options.len(), 3);
    }

    #[test]
    fn text_value_is_stored_and_an_appearance_is_generated() {
        let outcome = set(&form_pdf(None), "name", FieldValue::Text("Ada".into())).unwrap();
        assert_eq!(outcome.regenerated, ["name"]);
        assert!(!outcome.need_appearances);
        assert_eq!(field(&outcome.bytes, "name").value.as_deref(), Some("Ada"));
        let doc = load_pdf(&outcome.bytes).unwrap();
        let drawn = doc.objects.values().any(|o| {
            o.as_stream()
                .ok()
                .and_then(|s| s.decompressed_content().ok())
                .is_some_and(|c| c.windows(5).any(|w| w == b"(Ada)"))
        });
        assert!(drawn, "the appearance stream shows the text");
    }

    #[test]
    fn refusals_are_typed() {
        let source = form_pdf(None);
        assert!(matches!(
            set(&source, "nope", FieldValue::Text("x".into())),
            Err(FormError::FieldNotFound(_))
        ));
        assert!(matches!(
            set(&source, "id", FieldValue::Text("x".into())),
            Err(FormError::ReadOnly(_))
        ));
        assert!(matches!(
            set(
                &source,
                "name",
                FieldValue::Text("far too long a value".into())
            ),
            Err(FormError::TooLong { max: 10, .. })
        ));
        assert!(matches!(
            set(&source, "name", FieldValue::Checked(true)),
            Err(FormError::WrongKind { .. })
        ));
        assert!(matches!(
            set(&source, "size", FieldValue::Choice("XL".into())),
            Err(FormError::NotAnOption { .. })
        ));
        // The font has no glyph for this character: refused, never replaced.
        match set(&source, "name", FieldValue::Text("\u{4e2d}".into())) {
            Err(FormError::UnsupportedCharacters { chars, .. }) => assert_eq!(chars, ['\u{4e2d}']),
            other => panic!("expected unsupported characters, got {other:?}"),
        }
    }

    #[test]
    fn check_box_and_radio_switch_appearance_states() {
        let source = form_pdf(None);
        let checked = set(&source, "agree", FieldValue::Checked(true)).unwrap();
        assert_eq!(field(&checked.bytes, "agree").value.as_deref(), Some("Yes"));
        assert_eq!(field(&checked.bytes, "agree").checked, Some(true));
        let radio = set(&source, "colour", FieldValue::Choice("Green".into())).unwrap();
        assert_eq!(
            field(&radio.bytes, "colour").value.as_deref(),
            Some("Green")
        );
        assert!(matches!(
            set(&source, "colour", FieldValue::Choice("Blue".into())),
            Err(FormError::NoOnState(_))
        ));
    }

    #[test]
    fn combo_shows_the_label_and_stores_the_export_value() {
        let outcome = set(&form_pdf(None), "size", FieldValue::Choice("M".into())).unwrap();
        assert_eq!(field(&outcome.bytes, "size").value.as_deref(), Some("M"));
        let doc = load_pdf(&outcome.bytes).unwrap();
        let drawn = doc.objects.values().any(|o| {
            o.as_stream()
                .ok()
                .and_then(|s| s.decompressed_content().ok())
                .is_some_and(|c| c.windows(8).any(|w| w == b"(Medium)"))
        });
        assert!(drawn);
    }

    #[test]
    fn reset_clears_a_value() {
        let filled = set(&form_pdf(None), "name", FieldValue::Text("Ada".into())).unwrap();
        let reset = set(&filled.bytes, "name", FieldValue::Reset).unwrap();
        assert!(field(&reset.bytes, "name").value.is_none());
    }

    #[test]
    fn xfa_and_signed_forms_are_refused_unless_allowed() {
        let xfa = form_pdf(Some(("XFA", Object::Array(vec![]))));
        assert!(matches!(
            set(&xfa, "name", FieldValue::Text("x".into())),
            Err(FormError::Xfa)
        ));
        let allowed = FormOptions {
            drop_xfa: true,
            ..FormOptions::default()
        };
        assert!(set_values(
            &xfa,
            &[("name".into(), FieldValue::Text("x".into()))],
            &allowed
        )
        .is_ok());
        let signed = form_pdf(Some(("SigFlags", Object::Integer(3))));
        assert!(matches!(
            set(&signed, "name", FieldValue::Text("x".into())),
            Err(FormError::SignaturesPresent)
        ));
    }

    #[test]
    fn a_font_the_module_cannot_draw_keeps_the_value_and_asks_the_viewer() {
        // Same form, but the field's /DA names a font that is not in /DR.
        let source = form_pdf(None);
        let mut doc = load_pdf(&source).unwrap();
        let id = collect_fields(&doc, &acroform(&doc).unwrap().1).unwrap()[0].id;
        if let Ok(Object::Dictionary(d)) = doc.get_object_mut(id) {
            d.set(
                "DA",
                Object::String(b"/Missing 12 Tf 0 g".to_vec(), StringFormat::Literal),
            );
        }
        let mut changed = Vec::new();
        doc.save_to(&mut changed).unwrap();
        let outcome = set(&changed, "name", FieldValue::Text("Ada".into())).unwrap();
        assert_eq!(outcome.without_appearance, ["name"]);
        assert!(outcome.need_appearances);
    }
}
