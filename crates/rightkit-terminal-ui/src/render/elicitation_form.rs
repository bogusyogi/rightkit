//! MCP elicitation form: field-by-field entry for a server's requested schema.
//!
//! An MCP server's `elicitation/create` form request carries a flat JSON
//! Schema (`type: object`, primitive `properties`, optional `required`).
//! [`ElicitationForm`] turns that into typed fields, validates the answer
//! locally, and yields the JSON object the host returns to the server.
//!
//! Pure state machine: it reads keys and renders text, and performs no I/O.
//! The host maps [`FormOutcome::Submit`] onto `resolve_mcp_elicitation(Accept)`
//! and [`FormOutcome::Decline`] onto `Decline`. Every server-supplied string
//! is sanitized at construction (it is untrusted text bound for a terminal).
//! A schema this form cannot represent (nested objects, arrays, unknown
//! types) is refused by [`ElicitationForm::from_schema`] so the host falls
//! back to its raw-JSON entry rather than showing a lossy form.

use crossterm::event::KeyCode;
use serde_json::{Map, Number, Value};

use super::sanitize::sanitize;

const MAX_FIELDS: usize = 24;
const MAX_TEXT: usize = 2048;

#[derive(Debug, Clone, PartialEq)]
enum FieldKind {
    Text {
        min_len: Option<usize>,
        max_len: Option<usize>,
        format: Option<String>,
    },
    Number {
        integer: bool,
        min: Option<f64>,
        max: Option<f64>,
    },
    Boolean,
    Choice(Vec<Choice>),
}

#[derive(Debug, Clone, PartialEq)]
struct Choice {
    value: String,
    title: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Field {
    key: String,
    title: String,
    description: Option<String>,
    required: bool,
    kind: FieldKind,
}

#[derive(Debug, Clone, Default)]
struct FieldState {
    text: String,
    flag: bool,
    choice: Option<usize>,
}

/// What a key did to the form.
#[derive(Debug, Clone, PartialEq)]
pub enum FormOutcome {
    /// Still editing.
    Continue,
    /// Validated answer, ready to accept.
    Submit(Value),
    /// The user declined.
    Decline,
}

/// A typed, validating form built from an elicitation schema.
#[derive(Debug, Clone)]
pub struct ElicitationForm {
    fields: Vec<Field>,
    state: Vec<FieldState>,
    current: usize,
    error: Option<String>,
}

fn clean(text: &str) -> String {
    sanitize(text).replace(['\n', '\r'], " ")
}

impl ElicitationForm {
    /// Build a form from `requestedSchema`. `Err` names why the schema is not
    /// representable; the caller then offers raw JSON entry instead.
    pub fn from_schema(schema: &Value) -> Result<Self, String> {
        let object = schema.as_object().ok_or("schema is not an object")?;
        if let Some(kind) = object.get("type").and_then(Value::as_str) {
            if kind != "object" {
                return Err(format!("schema type `{kind}` is not an object"));
            }
        }
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .ok_or("schema has no properties")?;
        if properties.is_empty() {
            return Err("schema has no properties".into());
        }
        if properties.len() > MAX_FIELDS {
            return Err(format!("schema has more than {MAX_FIELDS} fields"));
        }
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut fields = Vec::new();
        let mut state = Vec::new();
        for (key, property) in properties {
            let (field, initial) = parse_field(key, property, required.contains(&key.as_str()))?;
            fields.push(field);
            state.push(initial);
        }
        Ok(Self {
            fields,
            state,
            current: 0,
            error: None,
        })
    }

    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    /// Index of the field being edited.
    pub fn current(&self) -> usize {
        self.current
    }

    /// The last validation message, if the previous submit failed.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Apply one key. Never panics on any input.
    pub fn handle_key(&mut self, code: KeyCode) -> FormOutcome {
        self.error = None;
        match code {
            KeyCode::Esc => return FormOutcome::Decline,
            KeyCode::Tab | KeyCode::Down => self.move_to(self.current + 1),
            KeyCode::BackTab | KeyCode::Up => self.move_to(self.current.saturating_sub(1)),
            KeyCode::Enter => {
                if self.current + 1 < self.fields.len() {
                    self.current += 1;
                } else {
                    match self.build() {
                        Ok(value) => return FormOutcome::Submit(value),
                        Err(message) => self.error = Some(message),
                    }
                }
            }
            KeyCode::Backspace => {
                if matches!(
                    self.fields[self.current].kind,
                    FieldKind::Text { .. } | FieldKind::Number { .. }
                ) {
                    self.state[self.current].text.pop();
                }
            }
            KeyCode::Left => self.step_choice(false),
            KeyCode::Right => self.step_choice(true),
            KeyCode::Char(c) => self.type_char(c),
            _ => {}
        }
        FormOutcome::Continue
    }

    /// Paste into the current text/number field.
    pub fn handle_paste(&mut self, text: &str) {
        for c in text.chars().filter(|c| !c.is_control()) {
            self.type_char(c);
        }
    }

    fn move_to(&mut self, index: usize) {
        self.current = index.min(self.fields.len().saturating_sub(1));
    }

    fn step_choice(&mut self, forward: bool) {
        let index = self.current;
        match &self.fields[index].kind {
            FieldKind::Boolean => self.state[index].flag = !self.state[index].flag,
            FieldKind::Choice(options) if !options.is_empty() => {
                let len = options.len();
                let next = match (self.state[index].choice, forward) {
                    (None, true) => 0,
                    (None, false) => len - 1,
                    (Some(i), true) => (i + 1) % len,
                    (Some(i), false) => (i + len - 1) % len,
                };
                self.state[index].choice = Some(next);
            }
            _ => {}
        }
    }

    fn type_char(&mut self, c: char) {
        let index = self.current;
        match &self.fields[index].kind {
            FieldKind::Text { .. } => {
                if !c.is_control() && self.state[index].text.chars().count() < MAX_TEXT {
                    self.state[index].text.push(c);
                }
            }
            FieldKind::Number { .. } => {
                if (c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
                    && self.state[index].text.len() < 64
                {
                    self.state[index].text.push(c);
                }
            }
            FieldKind::Boolean => match c {
                ' ' => self.state[index].flag = !self.state[index].flag,
                'y' | 'Y' => self.state[index].flag = true,
                'n' | 'N' => self.state[index].flag = false,
                _ => {}
            },
            FieldKind::Choice(_) => {
                if c == ' ' {
                    self.step_choice(true);
                }
            }
        }
    }

    /// Validate every field and build the answer object.
    pub fn build(&self) -> Result<Value, String> {
        let mut out = Map::new();
        for (field, state) in self.fields.iter().zip(&self.state) {
            let label = &field.title;
            match &field.kind {
                FieldKind::Text {
                    min_len,
                    max_len,
                    format,
                } => {
                    let text = state.text.trim_end_matches(['\n', '\r']);
                    if text.is_empty() {
                        if field.required {
                            return Err(format!("{label} is required"));
                        }
                        continue;
                    }
                    let len = text.chars().count();
                    if min_len.is_some_and(|min| len < min) {
                        return Err(format!(
                            "{label} must be at least {} characters",
                            min_len.unwrap_or(0)
                        ));
                    }
                    if max_len.is_some_and(|max| len > max) {
                        return Err(format!(
                            "{label} must be at most {} characters",
                            max_len.unwrap_or(0)
                        ));
                    }
                    if let Some(format) = format {
                        check_format(label, format, text)?;
                    }
                    out.insert(field.key.clone(), Value::String(text.to_string()));
                }
                FieldKind::Number { integer, min, max } => {
                    let text = state.text.trim();
                    if text.is_empty() {
                        if field.required {
                            return Err(format!("{label} is required"));
                        }
                        continue;
                    }
                    let parsed = text
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite())
                        .ok_or_else(|| format!("{label} must be a number"))?;
                    if *integer && parsed.fract() != 0.0 {
                        return Err(format!("{label} must be a whole number"));
                    }
                    if min.is_some_and(|m| parsed < m) {
                        return Err(format!("{label} must be at least {}", min.unwrap_or(0.0)));
                    }
                    if max.is_some_and(|m| parsed > m) {
                        return Err(format!("{label} must be at most {}", max.unwrap_or(0.0)));
                    }
                    let number = if *integer {
                        Number::from(parsed as i64)
                    } else {
                        Number::from_f64(parsed)
                            .ok_or_else(|| format!("{label} must be a number"))?
                    };
                    out.insert(field.key.clone(), Value::Number(number));
                }
                FieldKind::Boolean => {
                    out.insert(field.key.clone(), Value::Bool(state.flag));
                }
                FieldKind::Choice(options) => match state.choice.and_then(|i| options.get(i)) {
                    Some(choice) => {
                        out.insert(field.key.clone(), Value::String(choice.value.clone()));
                    }
                    None if field.required => return Err(format!("{label} is required")),
                    None => {}
                },
            }
        }
        Ok(Value::Object(out))
    }

    /// The card body: one row per field, the current one marked, with the
    /// last validation error and the key legend.
    pub fn render_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let total = self.fields.len();
        for (index, (field, state)) in self.fields.iter().zip(&self.state).enumerate() {
            let marker = if index == self.current { "› " } else { "  " };
            let star = if field.required { " *" } else { "" };
            let caret = if index == self.current { "▏" } else { "" };
            let value = match &field.kind {
                FieldKind::Text { .. } | FieldKind::Number { .. } => {
                    format!("{}{caret}", state.text)
                }
                FieldKind::Boolean => if state.flag { "[x] yes" } else { "[ ] no" }.to_string(),
                FieldKind::Choice(options) => {
                    let names = options
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            if state.choice == Some(i) {
                                format!("({})", c.title)
                            } else {
                                c.title.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" | ");
                    if state.choice.is_none() {
                        format!("<choose> {names}")
                    } else {
                        names
                    }
                }
            };
            lines.push(format!(
                "{marker}{}/{total} {}{star}: {value}",
                index + 1,
                field.title
            ));
            if let Some(description) = &field.description {
                lines.push(format!("      {description}"));
            }
        }
        if let Some(error) = &self.error {
            lines.push(format!("! {error}"));
        }
        lines.push(
            "Tab/↑↓ move · Enter next, submit on last · Space/←→ toggle or choose · Esc decline"
                .to_string(),
        );
        lines
    }
}

fn check_format(label: &str, format: &str, text: &str) -> Result<(), String> {
    let ok = match format {
        "email" => {
            let mut parts = text.splitn(2, '@');
            matches!((parts.next(), parts.next()), (Some(l), Some(d)) if !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.'))
                && !text.contains(char::is_whitespace)
        }
        "uri" => {
            matches!(text.split_once(':'), Some((scheme, rest)) if !scheme.is_empty()
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                && !rest.is_empty())
                && !text.contains(char::is_whitespace)
        }
        "date" => {
            let b: Vec<&str> = text.split('-').collect();
            b.len() == 3
                && b[0].len() == 4
                && b[1].len() == 2
                && b[2].len() == 2
                && b.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
                && matches!(b[1].parse::<u32>(), Ok(1..=12))
                && matches!(b[2].parse::<u32>(), Ok(1..=31))
        }
        "date-time" => text.contains('T') && text.len() >= 16,
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!("{label} must be a valid {format}"))
    }
}

fn parse_field(key: &str, property: &Value, required: bool) -> Result<(Field, FieldState), String> {
    let object = property
        .as_object()
        .ok_or_else(|| format!("field `{key}` is not an object"))?;
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .map(clean)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| clean(key));
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .map(clean)
        .filter(|d| !d.trim().is_empty());
    let kind_name = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string");
    let mut state = FieldState::default();
    let kind = match kind_name {
        "string" => {
            if let Some(options) = choices(object) {
                let options = options?;
                if let Some(default) = object.get("default").and_then(Value::as_str) {
                    state.choice = options.iter().position(|c| c.value == default);
                }
                FieldKind::Choice(options)
            } else {
                if let Some(default) = object.get("default").and_then(Value::as_str) {
                    state.text = clean(default);
                }
                FieldKind::Text {
                    min_len: object
                        .get("minLength")
                        .and_then(Value::as_u64)
                        .map(|n| n as usize),
                    max_len: object
                        .get("maxLength")
                        .and_then(Value::as_u64)
                        .map(|n| n as usize),
                    format: object
                        .get("format")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                }
            }
        }
        "number" | "integer" => {
            if let Some(default) = object.get("default").and_then(Value::as_f64) {
                state.text = if kind_name == "integer" {
                    format!("{}", default as i64)
                } else {
                    format!("{default}")
                };
            }
            FieldKind::Number {
                integer: kind_name == "integer",
                min: object.get("minimum").and_then(Value::as_f64),
                max: object.get("maximum").and_then(Value::as_f64),
            }
        }
        "boolean" => {
            state.flag = object
                .get("default")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            FieldKind::Boolean
        }
        other => return Err(format!("field `{key}` has unsupported type `{other}`")),
    };
    Ok((
        Field {
            key: key.to_string(),
            title,
            description,
            required,
            kind,
        },
        state,
    ))
}

/// Enumerated string choices: `enum` (+ optional `enumNames`) or `oneOf` of
/// `{const, title}`. `None` when the property is free text.
fn choices(object: &Map<String, Value>) -> Option<Result<Vec<Choice>, String>> {
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        let names = object.get("enumNames").and_then(Value::as_array);
        let mut out = Vec::new();
        for (i, value) in values.iter().enumerate() {
            let Some(value) = value.as_str() else {
                return Some(Err("enum values must be strings".into()));
            };
            let title = names
                .and_then(|n| n.get(i))
                .and_then(Value::as_str)
                .map(clean)
                .unwrap_or_else(|| clean(value));
            out.push(Choice {
                value: value.to_string(),
                title,
            });
        }
        return Some(Ok(out)).filter(|_| !values.is_empty());
    }
    if let Some(items) = object.get("oneOf").and_then(Value::as_array) {
        let mut out = Vec::new();
        for item in items {
            let Some(value) = item.get("const").and_then(Value::as_str) else {
                return Some(Err("oneOf entries need a string const".into()));
            };
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .map(clean)
                .unwrap_or_else(|| clean(value));
            out.push(Choice {
                value: value.to_string(),
                title,
            });
        }
        return Some(Ok(out)).filter(|_| !items.is_empty());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "title": "Name", "description": "Who", "minLength": 2},
                "age": {"type": "integer", "title": "Age", "minimum": 0, "maximum": 150},
                "ok": {"type": "boolean", "default": true},
                "mode": {"type": "string", "enum": ["fast", "slow"], "enumNames": ["Fast", "Slow"]},
            },
            "required": ["name", "mode"]
        })
    }

    fn type_text(form: &mut ElicitationForm, text: &str) {
        for c in text.chars() {
            form.handle_key(KeyCode::Char(c));
        }
    }

    #[test]
    fn a_filled_form_submits_the_typed_object() {
        let mut form = ElicitationForm::from_schema(&schema()).expect("form");
        assert_eq!(form.field_count(), 4);
        type_text(&mut form, "Ada");
        form.handle_key(KeyCode::Enter);
        type_text(&mut form, "36");
        form.handle_key(KeyCode::Enter);
        form.handle_key(KeyCode::Char(' '));
        form.handle_key(KeyCode::Enter);
        form.handle_key(KeyCode::Right);
        form.handle_key(KeyCode::Right);
        let outcome = form.handle_key(KeyCode::Enter);
        assert_eq!(
            outcome,
            FormOutcome::Submit(json!({"name": "Ada", "age": 36, "ok": false, "mode": "slow"}))
        );
    }

    #[test]
    fn a_missing_required_field_blocks_submit_with_a_message() {
        let mut form = ElicitationForm::from_schema(&schema()).expect("form");
        for _ in 0..3 {
            form.handle_key(KeyCode::Tab);
        }
        assert_eq!(form.handle_key(KeyCode::Enter), FormOutcome::Continue);
        assert_eq!(form.error(), Some("Name is required"));
    }

    #[test]
    fn range_length_and_format_are_validated() {
        let mut form = ElicitationForm::from_schema(&schema()).expect("form");
        type_text(&mut form, "A");
        assert_eq!(
            form.build().unwrap_err(),
            "Name must be at least 2 characters"
        );
        type_text(&mut form, "da");
        form.handle_key(KeyCode::Tab);
        type_text(&mut form, "200");
        assert_eq!(form.build().unwrap_err(), "Age must be at most 150");

        let email = ElicitationForm::from_schema(&json!({
            "properties": {"e": {"type": "string", "format": "email"}}
        }))
        .expect("form");
        let mut email = email;
        type_text(&mut email, "not-an-email");
        assert_eq!(email.build().unwrap_err(), "e must be a valid email");
    }

    #[test]
    fn defaults_seed_the_form_and_optional_blanks_are_omitted() {
        let form = ElicitationForm::from_schema(&json!({
            "properties": {
                "a": {"type": "string", "default": "x"},
                "b": {"type": "number"},
                "c": {"type": "boolean", "default": true},
            }
        }))
        .expect("form");
        assert_eq!(form.build().unwrap(), json!({"a": "x", "c": true}));
    }

    #[test]
    fn esc_declines() {
        let mut form = ElicitationForm::from_schema(&schema()).expect("form");
        assert_eq!(form.handle_key(KeyCode::Esc), FormOutcome::Decline);
    }

    #[test]
    fn unsupported_schemas_are_refused_so_the_host_can_fall_back() {
        for schema in [
            json!({}),
            json!({"type": "array"}),
            json!({"properties": {}}),
            json!({"properties": {"x": {"type": "object"}}}),
            json!({"properties": {"x": {"type": "array", "items": {"type": "string"}}}}),
            json!({"properties": {"x": {"type": "string", "enum": [1, 2]}}}),
        ] {
            assert!(ElicitationForm::from_schema(&schema).is_err(), "{schema}");
        }
    }

    #[test]
    fn server_text_is_sanitized_and_rendered_in_rows() {
        let form = ElicitationForm::from_schema(&json!({
            "properties": {"k": {"type": "string", "title": "Na\u{1b}[31mme", "description": "line1\nline2"}},
            "required": ["k"]
        }))
        .expect("form");
        let text = form.render_lines().join("\n");
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("1/1"), "{text}");
        assert!(text.contains("line1 line2"), "{text}");
        assert!(text.contains("*"), "{text}");
        assert!(text.contains("Esc decline"), "{text}");
    }

    #[test]
    fn number_fields_ignore_letters_and_integers_reject_fractions() {
        let mut form = ElicitationForm::from_schema(&json!({
            "properties": {"n": {"type": "integer"}}
        }))
        .expect("form");
        type_text(&mut form, "1x.5");
        assert_eq!(form.build().unwrap_err(), "n must be a whole number");
    }

    fn questions_schema() -> Value {
        json!({
            "properties": {
                "db": {"type": "string", "title": "Database",
                       "enum": ["pg", "sqlite"], "enumNames": ["Postgres", "SQLite"]},
                "cache": {"type": "string", "title": "Cache",
                          "enum": ["redis", "none"], "enumNames": ["Redis", "None"]},
            },
            "required": ["db", "cache"]
        })
    }

    #[test]
    fn a_multi_question_prompt_shows_every_option_and_returns_the_selections() {
        let mut form = ElicitationForm::from_schema(&questions_schema()).expect("form");
        let text = form.render_lines().join("\n");
        for needle in [
            "1/2 Database",
            "Postgres | SQLite",
            "2/2 Cache",
            "Redis | None",
        ] {
            assert!(text.contains(needle), "missing {needle:?}:\n{text}");
        }
        // Nothing chosen yet: submitting the last question is blocked.
        form.handle_key(KeyCode::Right);
        form.handle_key(KeyCode::Right);
        form.handle_key(KeyCode::Enter);
        assert_eq!(form.handle_key(KeyCode::Enter), FormOutcome::Continue);
        assert_eq!(form.error(), Some("Cache is required"));
        form.handle_key(KeyCode::Right);
        let selected = form.render_lines().join("\n");
        assert!(selected.contains("(SQLite)"), "{selected}");
        assert_eq!(
            form.handle_key(KeyCode::Enter),
            FormOutcome::Submit(json!({"db": "sqlite", "cache": "redis"}))
        );
    }

    #[test]
    fn esc_cancels_a_half_answered_multi_question_prompt() {
        let mut form = ElicitationForm::from_schema(&questions_schema()).expect("form");
        form.handle_key(KeyCode::Right);
        form.handle_key(KeyCode::Enter);
        assert_eq!(form.handle_key(KeyCode::Esc), FormOutcome::Decline);
    }
}
