//! Serde adapters for TDLib's JSON encoding, used by the generated code.
//!
//! TDLib sends `int64` values as strings (JSON numbers cannot hold them
//! exactly in every language) and `bytes` values as base64 strings.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::de::{self, DeserializeOwned, Deserializer, Visitor};
use serde::ser::{SerializeSeq, Serializer};
use serde::Deserialize;
use std::fmt;

/// Reads the `@type` field of a JSON object.
pub fn type_name<E: de::Error>(value: &serde_json::Value) -> Result<String, E> {
    value
        .get("@type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| E::custom("TDLib object has no `@type` field"))
}

/// Converts a JSON value into `T`, reporting failures as `E`.
pub fn from_value<T: DeserializeOwned, E: de::Error>(value: serde_json::Value) -> Result<T, E> {
    serde_json::from_value(value).map_err(E::custom)
}

/// `int64` fields: serialized as a string, deserialized from a string or a number.
pub mod int64 {
    use super::*;

    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        deserializer.deserialize_any(Int64Visitor)
    }

    struct Int64Visitor;

    impl Visitor<'_> for Int64Visitor {
        type Value = i64;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a 64-bit integer or a string containing one")
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<i64, E> {
            Ok(value)
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<i64, E> {
            i64::try_from(value).map_err(E::custom)
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<i64, E> {
            value.parse().map_err(E::custom)
        }
    }
}

/// `vector<int64>` fields.
pub mod vec_int64 {
    use super::*;

    #[derive(Deserialize)]
    struct Item(#[serde(with = "super::int64")] i64);

    pub fn serialize<S: Serializer>(values: &[i64], serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for value in values {
            seq.serialize_element(&value.to_string())?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<i64>, D::Error> {
        Ok(Vec::<Item>::deserialize(deserializer)?.into_iter().map(|Item(value)| value).collect())
    }
}

/// `bytes` fields: base64 strings.
pub mod bytes {
    use super::*;

    pub fn serialize<S: Serializer>(value: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(de::Error::custom)
    }
}

/// `vector<bytes>` fields.
pub mod vec_bytes {
    use super::*;

    #[derive(Deserialize)]
    struct Item(#[serde(with = "super::bytes")] Vec<u8>);

    pub fn serialize<S: Serializer>(values: &[Vec<u8>], serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for value in values {
            seq.serialize_element(&STANDARD.encode(value))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Vec<u8>>, D::Error> {
        Ok(Vec::<Item>::deserialize(deserializer)?.into_iter().map(|Item(value)| value).collect())
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        #[serde(with = "super::int64")]
        id: i64,
        #[serde(with = "super::vec_int64")]
        ids: Vec<i64>,
        #[serde(with = "super::bytes")]
        data: Vec<u8>,
        #[serde(with = "super::vec_bytes")]
        chunks: Vec<Vec<u8>>,
    }

    #[test]
    fn round_trips_tdlib_encodings() {
        let sample =
            Sample { id: -9_007_199_254_740_993, ids: vec![1, 2], data: vec![1, 2, 3], chunks: vec![vec![255]] };
        let value = serde_json::to_value(&sample).unwrap();
        assert_eq!(value, json!({"id": "-9007199254740993", "ids": ["1", "2"], "data": "AQID", "chunks": ["/w=="]}));
        assert_eq!(serde_json::from_value::<Sample>(value).unwrap(), sample);
    }

    #[test]
    fn int64_accepts_numbers() {
        let value = json!({"id": 42, "ids": [7], "data": "", "chunks": []});
        let sample: Sample = serde_json::from_value(value).unwrap();
        assert_eq!((sample.id, sample.ids), (42, vec![7]));
    }
}
