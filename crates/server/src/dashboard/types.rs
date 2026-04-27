use chrono::{DateTime, Utc};
use guardian_shared::auth_request_payload::AuthRequestPayload;
use miden_protocol::Word;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedOperator {
    pub operator_id: String,
    pub commitment: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorChallengePayload {
    pub domain: String,
    pub commitment: String,
    pub nonce: String,
    pub expires_at: String,
}

impl OperatorChallengePayload {
    pub fn signing_digest(&self) -> std::result::Result<Word, String> {
        AuthRequestPayload::from_json_serializable(self).map(|payload| payload.to_word())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorChallenge {
    pub payload: OperatorChallengePayload,
    pub signing_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedOperatorSession {
    pub operator: AuthenticatedOperator,
    pub expires_at: String,
    pub cookie_header: String,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingChallenge {
    pub(crate) signing_digest: Word,
    pub(crate) issued_at: DateTime<Utc>,
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub(crate) struct OperatorSessionRecord {
    pub(crate) operator: AuthenticatedOperator,
    pub(crate) issued_at: DateTime<Utc>,
    pub(crate) expires_at: DateTime<Utc>,
}
