use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt;

// v1 hashed struct field order; preserve the order written by that version before applying defaults.
enum OrderedJson {
    Scalar(Value),
    Array(Vec<OrderedJson>),
    Object(Vec<(String, OrderedJson)>),
}

impl Serialize for OrderedJson {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Scalar(value) => value.serialize(serializer),
            Self::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items { seq.serialize_element(item)?; }
                seq.end()
            }
            Self::Object(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries { map.serialize_entry(key, value)?; }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for OrderedJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = OrderedJson;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("JSON value") }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> { Ok(OrderedJson::Scalar(Value::Null)) }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> { Ok(OrderedJson::Scalar(value.into())) }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> { Ok(OrderedJson::Scalar(value.into())) }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> { Ok(OrderedJson::Scalar(value.into())) }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(value).map(|n| OrderedJson::Scalar(Value::Number(n)))
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> { Ok(OrderedJson::Scalar(value.into())) }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? { items.push(item); }
                Ok(OrderedJson::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                let mut keys = std::collections::HashSet::new();
                while let Some((key, value)) = map.next_entry::<String, OrderedJson>()? {
                    if !keys.insert(key.clone()) { return Err(serde::de::Error::custom("duplicate JSON key")); }
                    entries.push((key, value));
                }
                Ok(OrderedJson::Object(entries))
            }
        }
        deserializer.deserialize_any(JsonVisitor)
    }
}

impl OrderedJson {
    fn sort_keys(&mut self) {
        match self {
            Self::Object(entries) => {
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                for (_, value) in entries { value.sort_keys(); }
            }
            Self::Array(items) => { for item in items { item.sort_keys(); } }
            Self::Scalar(_) => {}
        }
    }
    fn digest(&self) -> Result<String, serde_json::Error> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}

pub(super) fn canonical(payload: &impl Serialize) -> Result<String, serde_json::Error> {
    let mut value: OrderedJson = serde_json::from_slice(&serde_json::to_vec(payload)?)?;
    value.sort_keys();
    value.digest()
}

pub(super) fn verify(raw: &[u8]) -> crate::error::AppResult<()> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Envelope {
        format_version: u32,
        payload_sha256: String,
        payload: OrderedJson,
    }
    let mut envelope: Envelope = serde_json::from_slice(raw)
        .map_err(|e| crate::error::AppError::Invalid(format!("不是有效的 SynaRoute 导出文件: {e}")))?;
    super::check_format_version(envelope.format_version)?;
    if envelope.format_version >= 2 { envelope.payload.sort_keys(); }
    if envelope.payload.digest()? != envelope.payload_sha256 {
        return Err(crate::error::AppError::Invalid(
            "导出文件校验和不匹配：文件已损坏或被修改，已拒绝导入（未改动任何本机配置）。".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_digest_is_recursive_and_order_independent() {
        let a: OrderedJson = serde_json::from_str(r#"{"z":[{"b":1,"a":2}],"a":false}"#).unwrap();
        let b: OrderedJson = serde_json::from_str(r#"{"a":false,"z":[{"a":2,"b":1}]}"#).unwrap();
        assert_eq!(canonical(&a).unwrap(), canonical(&b).unwrap());
        assert_ne!(a.digest().unwrap(), b.digest().unwrap());
        assert!(serde_json::from_str::<OrderedJson>(r#"{"a":1,"a":2}"#).is_err());
    }
}
