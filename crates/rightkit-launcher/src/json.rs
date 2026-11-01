//! Bounded JSON reading for the usage readers, the clipboard store and the launcher: `serde_json`
//! does the parsing, into a `Value` that keeps object members in file order (the clipboard
//! store writes unknown members back as it found them). Input size, nesting depth and element
//! counts are capped; malformed input returns `None`, never panics.

const MAX_DEPTH: usize = 12;
const MAX_ITEMS: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Walks nested object members; `None` as soon as one step is missing.
    pub fn path(&self, keys: &[&str]) -> Option<&Value> {
        let mut current = self;
        for key in keys {
            current = current.get(key)?;
        }
        Some(current)
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// Parses one JSON document of at most `max_bytes` bytes (a UTF-8 BOM is tolerated).
pub fn parse(bytes: &[u8], max_bytes: usize) -> Option<Value> {
    parse_with_items(bytes, max_bytes, MAX_ITEMS)
}

/// As `parse`, for a document that holds more than `MAX_ITEMS` elements in one array or
/// object (the clipboard's index of up to 5000 items); the caller names the bound.
pub fn parse_with_items(bytes: &[u8], max_bytes: usize, max_items: usize) -> Option<Value> {
    if bytes.len() > max_bytes {
        return None;
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let value: Value = serde_json::from_slice(bytes).ok()?;
    within_bounds(&value, 0, max_items).then_some(value)
}

fn within_bounds(value: &Value, depth: usize, max_items: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match value {
        Value::Array(items) => {
            items.len() <= max_items
                && items
                    .iter()
                    .all(|item| within_bounds(item, depth + 1, max_items))
        }
        Value::Object(members) => {
            members.len() <= max_items
                && members
                    .iter()
                    .all(|(_, member)| within_bounds(member, depth + 1, max_items))
        }
        _ => true,
    }
}

impl<'de> serde::Deserialize<'de> for Value {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Reader;

        impl<'de> serde::de::Visitor<'de> for Reader {
            type Value = Value;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON value")
            }

            fn visit_unit<E>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }

            fn visit_bool<E>(self, flag: bool) -> Result<Value, E> {
                Ok(Value::Bool(flag))
            }

            fn visit_i64<E>(self, number: i64) -> Result<Value, E> {
                Ok(Value::Number(number as f64))
            }

            fn visit_u64<E>(self, number: u64) -> Result<Value, E> {
                Ok(Value::Number(number as f64))
            }

            fn visit_f64<E>(self, number: f64) -> Result<Value, E> {
                Ok(Value::Number(number))
            }

            fn visit_str<E>(self, text: &str) -> Result<Value, E> {
                Ok(Value::Text(text.to_string()))
            }

            fn visit_string<E>(self, text: String) -> Result<Value, E> {
                Ok(Value::Text(text))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Value::Array(items))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Value, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry::<String, Value>()? {
                    members.push(member);
                }
                Ok(Value::Object(members))
            }
        }

        deserializer.deserialize_any(Reader)
    }
}
