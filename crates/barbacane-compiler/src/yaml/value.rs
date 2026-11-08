//! A YAML document as a tree that keeps the order of mapping keys, for edits
//! written back as YAML.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

/// A YAML node.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    String(String),
    Sequence(Vec<Value>),
    Mapping(Mapping),
}

/// A mapping, in the order its keys were read or inserted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mapping(Vec<(Value, Value)>);

impl Value {
    /// The mapping this node is, if it is one.
    pub fn as_mapping(&self) -> Option<&Mapping> {
        match self {
            Value::Mapping(mapping) => Some(mapping),
            _ => None,
        }
    }

    /// The mapping this node is, writable, if it is one.
    pub fn as_mapping_mut(&mut self) -> Option<&mut Mapping> {
        match self {
            Value::Mapping(mapping) => Some(mapping),
            _ => None,
        }
    }
}

impl Mapping {
    pub fn new() -> Self {
        Self::default()
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.0
            .iter()
            .position(|(k, _)| matches!(k, Value::String(k) if k == key))
    }

    /// The value under the string key `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.position(key).map(|i| &self.0[i].1)
    }

    /// The value under the string key `key`, writable.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.position(key).map(|i| &mut self.0[i].1)
    }

    /// Set `key` to `value`, in place when the key exists and last otherwise,
    /// and return the value it replaces.
    pub fn insert(&mut self, key: &str, value: Value) -> Option<Value> {
        match self.position(key) {
            Some(i) => Some(std::mem::replace(&mut self.0[i].1, value)),
            None => {
                self.0.push((Value::String(key.to_string()), value));
                None
            }
        }
    }

    /// Remove `key` and return its value.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.position(key).map(|i| self.0.remove(i).1)
    }

    /// The entries, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&Value, &Value)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(b) => serializer.serialize_bool(*b),
            Value::Int(n) => serializer.serialize_i64(*n),
            Value::UInt(n) => serializer.serialize_u64(*n),
            Value::Float(n) => serializer.serialize_f64(*n),
            Value::String(s) => serializer.serialize_str(s),
            Value::Sequence(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Value::Mapping(mapping) => mapping.serialize(serializer),
        }
    }
}

impl Serialize for Mapping {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Int(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::UInt(v))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(Value::Float(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_string()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        Value::deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Sequence(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut entries: Vec<(Value, Value)> = Vec::new();
        while let Some((key, value)) = map.next_entry::<Value, Value>()? {
            // A duplicated key keeps its first position and its last value.
            match entries.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key, value)),
            }
        }
        Ok(Value::Mapping(Mapping(entries)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_keep_their_order_through_a_read_an_edit_and_a_write() {
        let mut doc: Value = crate::yaml::from_str("b: 1\na: 2\nc: [x, y]\n").unwrap();
        let root = doc.as_mapping_mut().unwrap();
        root.insert("a", Value::String("two".into()));
        root.insert("d", Value::Bool(true));
        assert_eq!(root.remove("b"), Some(Value::UInt(1)));
        let written = crate::yaml::to_string(&doc).unwrap();
        let keys: Vec<_> = crate::yaml::from_str::<Value>(&written)
            .unwrap()
            .as_mapping()
            .unwrap()
            .iter()
            .map(|(k, _)| k.clone())
            .collect();
        assert_eq!(
            keys,
            ["a", "c", "d"].map(|k| Value::String(k.into())).to_vec()
        );
    }

    #[test]
    fn to_value_keeps_struct_fields_in_declaration_order() {
        #[derive(serde::Serialize)]
        struct Binding {
            name: &'static str,
            config: serde_json::Value,
        }
        let value = crate::yaml::to_value(&Binding {
            name: "auth",
            config: serde_json::json!({"z": 1, "a": 2}),
        })
        .unwrap();
        let keys: Vec<_> = value.as_mapping().unwrap().iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            vec![
                &Value::String("name".into()),
                &Value::String("config".into())
            ]
        );
    }
}
