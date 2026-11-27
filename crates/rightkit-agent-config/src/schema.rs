//! JSON Schema for `config.toml`, derived from the types themselves.
//!
//! There is no second list of keys to keep in step: the schema is read off the serde
//! `Deserialize` implementation of [`HostConfig`] by feeding it a probing deserializer. Each
//! struct reports its field names (already renamed to the kebab-case a file uses), each field
//! reports the kind of value it asks for, and the probe answers with a default so the walk goes
//! on to the next field. A host that checks the schema in keeps it under [`config_schema_file`];
//! a host that retired keys with [`crate::set_retired_config_keys`] gets a schema without them.

use std::cell::RefCell;
use std::fmt;

use serde::de::{
    self, DeserializeSeed, Deserializer, IntoDeserializer, MapAccess, SeqAccess, Visitor,
};
use serde_json::{json, Map, Value};

use crate::HostConfig;

/// The file name of the host's checked-in schema, relative to the repository's `schemas/` folder:
/// `<host slug>-config.v1.schema.json` (`myapp-config.v1.schema.json` for a host whose env
/// prefix is `MYAPP`; see [`crate::host_slug`]).
pub fn config_schema_file() -> String {
    format!("{}-config.v1.schema.json", crate::host_slug())
}

const MAX_DEPTH: usize = 12;

#[derive(Debug)]
struct ProbeError(String);

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProbeError {}

impl de::Error for ProbeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        ProbeError(msg.to_string())
    }
}

/// A deserializer that records what it was asked for in `out` and answers with a default value.
#[derive(Clone, Copy)]
struct Probe<'a> {
    out: &'a RefCell<Value>,
    depth: usize,
}

impl<'a> Probe<'a> {
    fn set(&self, schema: Value) {
        *self.out.borrow_mut() = schema;
    }

    fn child(&self, slot: &'a RefCell<Value>) -> Probe<'a> {
        Probe {
            out: slot,
            depth: self.depth + 1,
        }
    }
}

macro_rules! probe_scalar {
    ($($method:ident => $schema:expr, $visit:ident($default:expr);)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
                self.set($schema);
                visitor.$visit($default)
            }
        )*
    };
}

impl<'de, 'a> Deserializer<'de> for Probe<'a> {
    type Error = ProbeError;

    fn is_human_readable(&self) -> bool {
        true
    }

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        self.set(json!({}));
        visitor.visit_unit()
    }

    probe_scalar! {
        deserialize_bool => json!({"type": "boolean"}), visit_bool(false);
        deserialize_i8 => json!({"type": "integer"}), visit_i64(0);
        deserialize_i16 => json!({"type": "integer"}), visit_i64(0);
        deserialize_i32 => json!({"type": "integer"}), visit_i64(0);
        deserialize_i64 => json!({"type": "integer"}), visit_i64(0);
        deserialize_i128 => json!({"type": "integer"}), visit_i64(0);
        deserialize_u8 => json!({"type": "integer", "minimum": 0}), visit_u64(0);
        deserialize_u16 => json!({"type": "integer", "minimum": 0}), visit_u64(0);
        deserialize_u32 => json!({"type": "integer", "minimum": 0}), visit_u64(0);
        deserialize_u64 => json!({"type": "integer", "minimum": 0}), visit_u64(0);
        deserialize_u128 => json!({"type": "integer", "minimum": 0}), visit_u64(0);
        deserialize_f32 => json!({"type": "number"}), visit_f64(0.0);
        deserialize_f64 => json!({"type": "number"}), visit_f64(0.0);
        deserialize_char => json!({"type": "string"}), visit_char('a');
        deserialize_str => json!({"type": "string"}), visit_str("");
        deserialize_string => json!({"type": "string"}), visit_str("");
        deserialize_identifier => json!({"type": "string"}), visit_str("");
        deserialize_bytes => json!({"type": "array", "items": {"type": "integer"}}), visit_bytes(&[]);
        deserialize_byte_buf => json!({"type": "array", "items": {"type": "integer"}}), visit_bytes(&[]);
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        self.set(json!({"type": "null"}));
        visitor.visit_unit()
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        self.set(json!({}));
        visitor.visit_unit()
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        // An optional key is the same key with the inner type; the probe is the "some" case.
        visitor.visit_some(self)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        if self.depth > MAX_DEPTH {
            self.set(json!({"type": "array"}));
            return visitor.visit_seq(OneElement::empty());
        }
        let item = RefCell::new(json!({}));
        let result = visitor.visit_seq(OneElement::new(self.child(&item)));
        self.set(json!({"type": "array", "items": item.into_inner()}));
        result
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, ProbeError> {
        if self.depth > MAX_DEPTH {
            self.set(json!({"type": "object"}));
            return visitor.visit_map(OneEntry::empty());
        }
        let value = RefCell::new(json!({}));
        let result = visitor.visit_map(OneEntry::new(self.child(&value)));
        // A table whose keys are chosen by the file (`[provider-routes.<name>]`): every key is
        // allowed and each value has the shape the map's value type asks for.
        self.set(json!({"type": "object", "additionalProperties": value.into_inner()}));
        result
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        if self.depth > MAX_DEPTH {
            self.set(json!({"type": "object"}));
            return visitor.visit_map(OneEntry::empty());
        }
        let properties = RefCell::new(Map::new());
        let result = visitor.visit_map(Fields {
            probe: self,
            fields,
            index: 0,
            properties: &properties,
        });
        self.set(json!({
            "type": "object",
            "properties": properties.into_inner(),
            "additionalProperties": false,
        }));
        result
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, ProbeError> {
        self.set(json!({"type": "string", "enum": variants}));
        match variants.first() {
            Some(first) => {
                visitor.visit_enum(IntoDeserializer::<ProbeError>::into_deserializer(*first))
            }
            None => Err(de::Error::custom("an enum with no variant")),
        }
    }
}

/// The fields of one struct, in declaration order; each value is probed and recorded.
struct Fields<'a> {
    probe: Probe<'a>,
    fields: &'static [&'static str],
    index: usize,
    properties: &'a RefCell<Map<String, Value>>,
}

impl<'de, 'a> MapAccess<'de> for Fields<'a> {
    type Error = ProbeError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, ProbeError> {
        let Some(name) = self.fields.get(self.index) else {
            return Ok(None);
        };
        seed.deserialize(IntoDeserializer::<ProbeError>::into_deserializer(*name))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, ProbeError> {
        let name = self.fields[self.index];
        self.index += 1;
        let slot = RefCell::new(json!({}));
        let result = seed.deserialize(self.probe.child(&slot));
        self.properties
            .borrow_mut()
            .insert(name.to_string(), slot.into_inner());
        result
    }
}

/// One entry of a map: the key `"key"` and a probed value.
struct OneEntry<'a> {
    probe: Option<Probe<'a>>,
    key_given: bool,
}

impl<'a> OneEntry<'a> {
    fn new(probe: Probe<'a>) -> Self {
        OneEntry {
            probe: Some(probe),
            key_given: false,
        }
    }

    fn empty() -> Self {
        OneEntry {
            probe: None,
            key_given: false,
        }
    }
}

impl<'de, 'a> MapAccess<'de> for OneEntry<'a> {
    type Error = ProbeError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, ProbeError> {
        if self.key_given || self.probe.is_none() {
            return Ok(None);
        }
        self.key_given = true;
        seed.deserialize(IntoDeserializer::<ProbeError>::into_deserializer("key"))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, ProbeError> {
        match self.probe {
            Some(probe) => seed.deserialize(probe),
            None => Err(de::Error::custom("no value")),
        }
    }
}

/// One element of a sequence, probed.
struct OneElement<'a> {
    probe: Option<Probe<'a>>,
}

impl<'a> OneElement<'a> {
    fn new(probe: Probe<'a>) -> Self {
        OneElement { probe: Some(probe) }
    }

    fn empty() -> Self {
        OneElement { probe: None }
    }
}

impl<'de, 'a> SeqAccess<'de> for OneElement<'a> {
    type Error = ProbeError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, ProbeError> {
        match self.probe.take() {
            Some(probe) => seed.deserialize(probe).map(Some),
            None => Ok(None),
        }
    }
}

/// Remove the property at the dotted path `segments` (each segment a key under `properties`); a
/// path that is not in the schema is left alone.
fn remove_property(node: &mut Value, segments: &[&str]) {
    let Some(properties) = node.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    match segments {
        // `retain` keeps the order of the other properties whether or not serde_json keeps
        // insertion order (`remove` would reorder them under `preserve_order`).
        [last] => properties.retain(|name, _| name != last),
        [first, rest @ ..] => {
            if let Some(child) = properties.get_mut(*first) {
                remove_property(child, rest);
            }
        }
        [] => {}
    }
}

/// The JSON Schema (draft 2020-12) for `config.toml` and `<state-dir>/config.toml`. `Err` names
/// the type that the probe could not walk. A key the host retired
/// ([`crate::set_retired_config_keys`]) is not in the schema.
pub fn config_json_schema() -> Result<Value, String> {
    let out = RefCell::new(json!({}));
    <HostConfig as serde::Deserialize>::deserialize(Probe {
        out: &out,
        depth: 0,
    })
    .map_err(|e| format!("config schema: {e}"))?;
    let mut schema = out.into_inner();
    for key in crate::retired_config_keys() {
        let segments: Vec<&str> = key.split('.').collect();
        remove_property(&mut schema, &segments);
    }
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "$schema".into(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        object.insert(
            "$id".into(),
            json!(format!(
                "https://{}.invalid/schemas/{}",
                crate::host_slug(),
                config_schema_file()
            )),
        );
        object.insert(
            "title".into(),
            json!(format!("{} config.toml", crate::app_name())),
        );
        object.insert(
            "description".into(),
            json!(format!(
                "Settings file read from <state root>/config.toml and <workspace>/{}/config.toml. A key no setting reads is reported by `{} config` and refused by `--strict-config`.",
                crate::state_dir_name(),
                crate::host_slug()
            )),
        );
        if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
            // Written by older installs and read by nothing.
            properties.insert("schema-version".into(), json!({"type": "integer"}));
        }
    }
    Ok(schema)
}

/// [`config_json_schema`] as the text of the checked-in file (pretty, with a final newline).
pub fn config_json_schema_text() -> Result<String, String> {
    let schema = config_json_schema()?;
    let mut text = serde_json::to_string_pretty(&schema).map_err(|e| e.to_string())?;
    text.push('\n');
    Ok(text)
}
