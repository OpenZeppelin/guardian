use crate::auth_request_payload::AuthRequestPayload;
use miden_protocol::account::AccountId;
use miden_protocol::crypto::hash::rpo::Rpo256;
use miden_protocol::{Felt, Word};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthRequestMessage {
    account_id: AccountId,
    timestamp: i64,
    payload: AuthRequestPayload,
}

impl AuthRequestMessage {
    pub fn new(account_id: AccountId, timestamp: i64, payload: AuthRequestPayload) -> Self {
        Self {
            account_id,
            timestamp,
            payload,
        }
    }

    pub fn from_account_id_hex(
        account_id_hex: &str,
        timestamp: i64,
        payload: AuthRequestPayload,
    ) -> Result<Self, String> {
        let account_id = AccountId::from_hex(account_id_hex)
            .map_err(|e| format!("Invalid account ID hex: {e}"))?;
        Ok(Self::new(account_id, timestamp, payload))
    }

    pub fn from_protobuf_message<T: prost::Message>(
        account_id: AccountId,
        timestamp: i64,
        request: &T,
    ) -> Self {
        Self::new(
            account_id,
            timestamp,
            AuthRequestPayload::from_protobuf_message(request),
        )
    }

    pub fn from_json_serializable<T: Serialize>(
        account_id: AccountId,
        timestamp: i64,
        request: &T,
    ) -> Result<Self, String> {
        let payload = AuthRequestPayload::from_json_serializable(request)?;
        Ok(Self::new(account_id, timestamp, payload))
    }

    pub fn to_word(&self) -> Word {
        let account_id_felts: [Felt; 2] = self.account_id.into();
        let timestamp_felt = Felt::new(self.timestamp as u64);
        let payload_elements = self.payload.as_elements();
        let message_elements = vec![
            account_id_felts[0],
            account_id_felts[1],
            timestamp_felt,
            payload_elements[0],
            payload_elements[1],
            payload_elements[2],
            payload_elements[3],
        ];
        Rpo256::hash_elements(&message_elements)
    }
}

#[cfg(test)]
mod tests {
    use super::AuthRequestMessage;
    use crate::auth_request_payload::AuthRequestPayload;
    use miden_protocol::account::AccountId;

    #[test]
    fn request_message_digest_changes_with_payload() {
        let account_id =
            AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").expect("account id");
        let timestamp = 1_700_000_000i64;
        let left_payload =
            AuthRequestPayload::from_json_bytes(br#"{"op":"get_state"}"#).expect("left payload");
        let right_payload =
            AuthRequestPayload::from_json_bytes(br#"{"op":"push_delta"}"#).expect("right payload");

        let left = AuthRequestMessage::new(account_id, timestamp, left_payload).to_word();
        let right = AuthRequestMessage::new(account_id, timestamp, right_payload).to_word();

        assert_ne!(left, right);
    }
}
