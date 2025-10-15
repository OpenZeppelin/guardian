mod miden_falcon_rpo;

/// Authentication type enum - defines which signature scheme to use
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum AuthType {
    /// Miden Falcon RPO signature scheme
    MidenFalconRpo,
}

impl AuthType {
    /// Verify a signature for a request using the appropriate auth scheme
    ///
    /// # Arguments
    /// * `account_id` - The account ID
    /// * `pubkey` - The public key (hex-encoded or scheme-specific format)
    /// * `signature` - The signature to verify (hex-encoded or scheme-specific format)
    ///
    /// # Returns
    /// * `Ok(())` if signature is valid
    /// * `Err(String)` with error message otherwise
    pub fn verify_signature(
        &self,
        account_id: &str,
        pubkey: &str,
        signature: &str,
    ) -> Result<(), String> {
        match self {
            AuthType::MidenFalconRpo => {
                miden_falcon_rpo::verify_request_signature(
                    account_id,
                    pubkey,
                    signature,
                )
            }
        }
    }
}
