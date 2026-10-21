//! A JavaScript value, as the Paseo plugin holds a ledger entry.
//!
//! The plugin reads YAML into JavaScript values and hashes and reads those.
//! JSON cannot hold all of them: a number may be `Infinity` or `NaN`, which
//! `JSON.stringify` prints as `null` but `String(n)` prints as `"Infinity"`.
//! A ledger entry is therefore kept as a `JsValue`, which holds every number as
//! the double JavaScript holds, and is turned into JSON only for output.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum JsValue {
    Null,
    Bool(bool),
    /// Every JavaScript number is a double, `Infinity` and `NaN` included.
    Num(f64),
    Str(String),
    Arr(Vec<JsValue>),
    /// An object's own keys. Their order does not matter: `canonical` sorts them.
    Obj(BTreeMap<String, JsValue>),
}

impl JsValue {
    pub fn get(&self, key: &str) -> Option<&JsValue> {
        match self {
            JsValue::Obj(map) => map.get(key),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, JsValue>> {
        match self {
            JsValue::Obj(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut BTreeMap<String, JsValue>> {
        match self {
            JsValue::Obj(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<JsValue>> {
        match self {
            JsValue::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn is_object(&self) -> bool {
        matches!(self, JsValue::Obj(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self, JsValue::Arr(_))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, JsValue::Null)
    }

    /// The JSON a caller sees: what `JSON.stringify` would write, a non-finite number as `null`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            JsValue::Null => serde_json::Value::Null,
            JsValue::Bool(b) => serde_json::Value::Bool(*b),
            JsValue::Num(x) => json_number(*x),
            JsValue::Str(s) => serde_json::Value::String(s.clone()),
            JsValue::Arr(items) => serde_json::Value::Array(items.iter().map(JsValue::to_json).collect()),
            JsValue::Obj(map) => serde_json::Value::Object(map.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
        }
    }
}

/// A double as JSON: an integer within 2^53 as an integer, a non-finite one as `null`.
fn json_number(x: f64) -> serde_json::Value {
    const SAFE: f64 = 9_007_199_254_740_992.0;
    if !x.is_finite() {
        return serde_json::Value::Null;
    }
    if x.fract() == 0.0 && x.abs() <= SAFE {
        return serde_json::Value::from(x as i64);
    }
    serde_json::Number::from_f64(x).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

impl From<serde_json::Value> for JsValue {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => JsValue::Null,
            serde_json::Value::Bool(b) => JsValue::Bool(b),
            serde_json::Value::Number(n) => JsValue::Num(n.as_f64().unwrap_or(f64::NAN)),
            serde_json::Value::String(s) => JsValue::Str(s),
            serde_json::Value::Array(items) => JsValue::Arr(items.into_iter().map(JsValue::from).collect()),
            serde_json::Value::Object(map) => JsValue::Obj(map.into_iter().map(|(k, v)| (k, v.into())).collect()),
        }
    }
}

impl Serialize for JsValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}
