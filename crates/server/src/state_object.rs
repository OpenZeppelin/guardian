use serde::{Deserialize, Serialize};

/// Account state object
#[derive(Serialize, Deserialize, Clone, Debug, Default, utoipa::ToSchema)]
pub struct StateObject {
    pub account_id: String,
    /// Opaque, schema-free JSON blob describing the account state.
    #[schema(value_type = Object)]
    pub state_json: serde_json::Value,
    pub commitment: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub auth_scheme: String,
}
