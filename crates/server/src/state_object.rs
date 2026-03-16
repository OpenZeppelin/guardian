use serde::{Deserialize, Serialize};

/// Account state object
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StateObject {
    pub account_id: String,
    pub state_json: serde_json::Value,
    pub commitment: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub auth_scheme: String,
}
