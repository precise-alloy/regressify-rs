use serde::Deserialize;
use serde_json::Value;

/// A JSON-compatible value that rejects objects with repeated keys instead of keeping the last.
///
/// Works with any serde format, so YAML and JSON documents that are parsed generically
/// before strict deserialization keep the duplicate-property errors typed parsing gives.
pub(crate) struct UniqueJsonValue(pub Value);

impl<'de> Deserialize<'de> for UniqueJsonValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Value;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a value with unique object properties")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
                Ok(Value::Null)
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Value, E> {
                Ok(Value::Bool(value))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Value, E> {
                Ok(value.into())
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Value, E> {
                Ok(value.into())
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Value, E> {
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .ok_or_else(|| E::custom("Invalid JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Value, E> {
                Ok(Value::String(value.into()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Value, E> {
                Ok(Value::String(value))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<UniqueJsonValue>()? {
                    values.push(value.0);
                }
                Ok(Value::Array(values))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        // Reject before Value can collapse a property that Fields would erase.
                        return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
                    }
                    values.insert(key, map.next_value::<UniqueJsonValue>()?.0);
                }
                Ok(Value::Object(values))
            }
        }
        deserializer.deserialize_any(Visitor).map(Self)
    }
}
