//! Best-effort view of a tool call's arguments while they are still streaming.
//!
//! The provider streams a call's arguments as JSON fragments. Only a complete generation may
//! dispatch a call (EFF-009), so nothing here is ever executed: the view carries the top-level
//! string fields read so far (for example the `path` of a `write_file` call before its long
//! `content` has finished) so a screen can say what is being prepared. The parser is incremental
//! and tolerant: it stops quietly at the end of the fragments it has and ignores anything that is
//! not a top-level string field.

use crate::agent_providers::client::ProviderStreamEvent;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Longest value kept per field. A long `content` argument is cut here; the view then stops
/// changing, which is what bounds the number of events a large call produces.
const PREVIEW_VALUE_CHARS: usize = 200;
/// Most fields kept per call.
const PREVIEW_MAX_FIELDS: usize = 16;

#[derive(Default)]
enum State {
    /// Before the opening `{`.
    #[default]
    Start,
    BeforeKey,
    Key,
    AfterKey,
    BeforeValue,
    StringValue,
    /// A number, literal, object or array: skipped, tracking nesting and strings inside it.
    Other,
    AfterValue,
    /// The closing `}` was read, or the text is not an object.
    Done,
}

/// A JSON string being decoded one character at a time.
#[derive(Default)]
struct Text {
    out: String,
    chars: usize,
    escape: bool,
    unicode: Option<String>,
    high_surrogate: Option<u16>,
}

impl Text {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn keep(&mut self, c: char) {
        if self.chars < PREVIEW_VALUE_CHARS {
            self.out.push(c);
            self.chars += 1;
        }
    }

    /// Feed one source character of the string body. Returns true at the closing quote.
    fn step(&mut self, c: char) -> bool {
        if let Some(hex) = self.unicode.as_mut() {
            hex.push(c);
            if hex.len() == 4 {
                let unit = u16::from_str_radix(hex, 16).unwrap_or(0xFFFD);
                self.unicode = None;
                match (self.high_surrogate.take(), unit) {
                    (Some(high), 0xDC00..=0xDFFF) => {
                        let code =
                            0x10000 + (((high as u32) - 0xD800) << 10) + (unit as u32 - 0xDC00);
                        self.keep(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    }
                    (_, 0xD800..=0xDBFF) => self.high_surrogate = Some(unit),
                    _ => self.keep(char::from_u32(unit as u32).unwrap_or('\u{FFFD}')),
                }
            }
            return false;
        }
        if self.escape {
            self.escape = false;
            match c {
                'n' => self.keep('\n'),
                't' => self.keep('\t'),
                'r' => self.keep('\r'),
                'b' => self.keep('\u{8}'),
                'f' => self.keep('\u{c}'),
                'u' => self.unicode = Some(String::new()),
                other => self.keep(other),
            }
            return false;
        }
        match c {
            '\\' => {
                self.escape = true;
                false
            }
            '"' => true,
            other => {
                self.keep(other);
                false
            }
        }
    }
}

/// Incremental reader of the top-level string fields of one JSON object.
#[derive(Default)]
pub struct PartialArgs {
    state: State,
    key: Text,
    value: Text,
    /// Nesting depth inside a skipped object or array.
    depth: usize,
    nested_in_string: bool,
    nested_escape: bool,
    fields: Vec<(String, String)>,
}

impl PartialArgs {
    pub fn push(&mut self, fragment: &str) {
        for c in fragment.chars() {
            self.step(c);
        }
    }

    fn finish_value(&mut self) {
        if self.fields.len() < PREVIEW_MAX_FIELDS {
            self.fields.push((
                std::mem::take(&mut self.key.out),
                std::mem::take(&mut self.value.out),
            ));
        }
        self.key.clear();
        self.value.clear();
        self.state = State::AfterValue;
    }

    fn step(&mut self, c: char) {
        match self.state {
            State::Start => {
                if c == '{' {
                    self.state = State::BeforeKey;
                } else if !c.is_whitespace() {
                    self.state = State::Done;
                }
            }
            State::BeforeKey => match c {
                '"' => self.state = State::Key,
                '}' => self.state = State::Done,
                _ => {}
            },
            State::Key => {
                if self.key.step(c) {
                    self.state = State::AfterKey;
                }
            }
            State::AfterKey => {
                if c == ':' {
                    self.state = State::BeforeValue;
                }
            }
            State::BeforeValue => {
                if c == '"' {
                    self.state = State::StringValue;
                } else if !c.is_whitespace() {
                    self.depth = usize::from(c == '{' || c == '[');
                    self.nested_in_string = false;
                    self.nested_escape = false;
                    self.state = State::Other;
                    self.key.clear();
                }
            }
            State::StringValue => {
                if self.value.step(c) {
                    self.finish_value();
                }
            }
            State::Other => {
                if self.nested_in_string {
                    if self.nested_escape {
                        self.nested_escape = false;
                    } else if c == '\\' {
                        self.nested_escape = true;
                    } else if c == '"' {
                        self.nested_in_string = false;
                    }
                } else {
                    match c {
                        '"' => self.nested_in_string = true,
                        '{' | '[' => self.depth += 1,
                        '}' | ']' if self.depth > 0 => self.depth -= 1,
                        ',' if self.depth == 0 => self.state = State::BeforeKey,
                        '}' => self.state = State::Done,
                        _ => {}
                    }
                }
            }
            State::AfterValue => match c {
                ',' => self.state = State::BeforeKey,
                '}' => self.state = State::Done,
                _ => {}
            },
            State::Done => {}
        }
    }

    /// The fields read so far as a JSON object of strings, the one being read included.
    pub fn view_json(&self) -> String {
        let mut object = Map::new();
        for (key, value) in &self.fields {
            object.insert(key.clone(), Value::String(value.clone()));
        }
        if matches!(self.state, State::StringValue) && self.fields.len() < PREVIEW_MAX_FIELDS {
            object.insert(self.key.out.clone(), Value::String(self.value.out.clone()));
        }
        Value::Object(object).to_string()
    }
}

/// Live argument views of every call of one streamed response.
#[derive(Default)]
pub struct ToolArgsPreviews {
    calls: HashMap<String, (PartialArgs, String)>,
}

impl ToolArgsPreviews {
    /// Add `fragment` to the call stored under `slot` and return a preview event when its view
    /// changed. A call with no id or name yet has nothing a screen could attach the view to.
    pub fn push(
        &mut self,
        slot: &str,
        id: &str,
        name: &str,
        fragment: &str,
    ) -> Option<ProviderStreamEvent> {
        let (args, last) = self.calls.entry(slot.to_string()).or_default();
        args.push(fragment);
        if id.is_empty() || name.is_empty() {
            return None;
        }
        let view = args.view_json();
        if view == "{}" || view == *last {
            return None;
        }
        last.clone_from(&view);
        Some(ProviderStreamEvent::ToolArgumentsPartial {
            id: id.to_string(),
            name: name.to_string(),
            arguments_json: view,
        })
    }
}
