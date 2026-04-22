use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::{Felt, Word};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRequestPayload {
    digest: Word,
}

impl AuthRequestPayload {
    pub fn empty() -> Self {
        Self {
            digest: Word::from([Felt::ZERO; 4]),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::empty();
        }

        let mut payload_elements = Vec::with_capacity(bytes.len().div_ceil(8));
        for chunk in bytes.chunks(8) {
            let mut chunk_bytes = [0u8; 8];
            chunk_bytes[..chunk.len()].copy_from_slice(chunk);
            payload_elements.push(Felt::new(u64::from_le_bytes(chunk_bytes)));
        }

        Self {
            digest: Rpo256::hash_elements(&payload_elements),
        }
    }

    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, String> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("Invalid JSON payload: {e}"))?;
        Self::from_json_value(&value)
    }

    pub fn from_json_value(value: &Value) -> Result<Self, String> {
        let canonical = canonicalize_json(value);
        let bytes =
            serde_json::to_vec(&canonical).map_err(|e| format!("Failed to serialize JSON: {e}"))?;
        Ok(Self::from_bytes(&bytes))
    }

    pub fn from_json_serializable<T: Serialize>(value: &T) -> Result<Self, String> {
        let json = serde_json::to_value(value)
            .map_err(|e| format!("Failed to convert payload to JSON value: {e}"))?;
        Self::from_json_value(&json)
    }

    pub fn from_protobuf_message<T: prost::Message>(value: &T) -> Self {
        Self::from_bytes(&value.encode_to_vec())
    }

    pub fn as_elements(&self) -> [Felt; 4] {
        let elements = self.digest.as_elements();
        [elements[0], elements[1], elements[2], elements[3]]
    }

    pub fn to_word(&self) -> Word {
        self.digest
    }
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().cloned().collect::<Vec<_>>();
            keys.sort();

            let mut sorted = serde_json::Map::with_capacity(map.len());
            for key in keys {
                let item = map
                    .get(&key)
                    .expect("key collected from map must always exist");
                sorted.insert(key, canonicalize_json(item));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize_json).collect()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::AuthRequestPayload;

    #[test]
    fn json_payload_hash_is_order_insensitive() {
        let left = br#"{"b":2,"a":1}"#;
        let right = br#"{"a":1,"b":2}"#;

        let left_payload = AuthRequestPayload::from_json_bytes(left).expect("left json");
        let right_payload = AuthRequestPayload::from_json_bytes(right).expect("right json");

        assert_eq!(left_payload, right_payload);
    }

    #[test]
    fn empty_payload_hash_is_zero_word() {
        let payload = AuthRequestPayload::from_bytes(b"");
        assert_eq!(payload, AuthRequestPayload::empty());
    }
}
