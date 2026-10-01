use serde::{Deserialize, Serialize};

/// Account state object
#[derive(Serialize, Deserialize, Clone, Debug, Default, utoipa::ToSchema)]
pub struct StateObject {
    pub account_id: String,
    /// Opaque, schema-free JSON blob describing the account state.
    #[schema(value_type = Object)]
    pub state_json: serde_json::Value,
    pub commitment: String,
    /// Account nonce of `state_json`, stored next to `commitment` so the
    /// canonical-nonce pre-check (issue #191) reads it without loading or
    /// decoding the blob. Writers take it from the same decode that yields
    /// `commitment` (`NetworkClient::get_state_head` / `apply_delta`).
    ///
    /// `None` means the stored row does not know it: the row was written
    /// before the server stored nonces, or rewritten by a server version
    /// that predates them (Postgres clears the nonce when an update moves
    /// the commitment without setting a new nonce). Readers that need it
    /// decode `state_json` instead. Only Miden states are stored, and a
    /// Miden account always has a nonce; a future writer for a network
    /// without one leaves this `None` rather than inventing a value.
    ///
    /// Server-internal: not part of the `GET /state` response.
    #[serde(skip)]
    pub nonce: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub auth_scheme: String,
}

/// Commitment and nonce of one account state, without its blob: what the
/// network client reads off a decoded account and what
/// [`crate::storage::StorageBackend::pull_state_head`] reads from storage.
/// `nonce` follows [`StateObject::nonce`]: `None` when it is not known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateHead {
    pub commitment: String,
    pub nonce: Option<u64>,
}
