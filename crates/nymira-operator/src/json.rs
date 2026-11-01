//! Strict JSON: no duplicate keys at any depth, and no unknown keys in the lease request.

use std::collections::HashSet;
use std::fmt;

use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

struct NoDup;

impl<'de> DeserializeSeed<'de> for NoDup {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(NoDupVisitor)
    }
}

struct NoDupVisitor;

impl<'de> Visitor<'de> for NoDupVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(v.into())
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(v.into())
    }

    fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
        Ok(serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.into()))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(NoDup)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut seen = HashSet::new();
        let mut out = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!("duplicate key {key}")));
            }
            out.insert(key, map.next_value_seed(NoDup)?);
        }
        Ok(Value::Object(out))
    }
}

/// Parses `bytes` as one JSON value with no duplicate keys.
pub fn parse_strict(bytes: &[u8]) -> Result<Value, String> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = NoDup.deserialize(&mut de).map_err(|e| e.to_string())?;
    de.end().map_err(|e| e.to_string())?;
    Ok(value)
}

/// `object` has exactly the keys in `allowed` or fewer, nothing else.
pub fn only_keys(object: &Value, allowed: &[&str]) -> bool {
    object
        .as_object()
        .is_some_and(|o| o.keys().all(|k| allowed.contains(&k.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_are_refused_at_any_depth() {
        assert!(parse_strict(br#"{"a":1,"b":2}"#).is_ok());
        assert!(parse_strict(br#"{"a":1,"a":2}"#).is_err());
        assert!(parse_strict(br#"{"a":{"b":1,"b":2}}"#).is_err());
        assert!(parse_strict(br#"[{"a":1,"a":1}]"#).is_err());
        assert!(parse_strict(br#"{"a":1} trailing"#).is_err());
    }

    #[test]
    fn only_keys_refuses_an_extra_key() {
        let v: Value = serde_json::json!({"a":1,"z":2});
        assert!(only_keys(&v, &["a", "z"]));
        assert!(!only_keys(&v, &["a"]));
    }
}
