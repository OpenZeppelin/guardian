//! Configuration types for the multisig client.

use miden_objects::Word;

/// Configuration for connecting to a PSM server.
#[derive(Debug, Clone)]
pub struct PsmConfig {
    /// The gRPC endpoint URL (e.g., "http://localhost:50051").
    pub endpoint: String,
}

impl PsmConfig {
    /// Creates a new PSM configuration with the given endpoint.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}

/// Configuration for creating a new multisig account.
#[derive(Debug, Clone)]
pub struct MultisigConfig {
    /// Minimum number of signatures required to authorize a transaction.
    pub threshold: u32,
    /// Public key commitments of all signers.
    pub signer_commitments: Vec<Word>,
    /// PSM server configuration.
    pub psm_config: PsmConfig,
}

impl MultisigConfig {
    /// Creates a new multisig configuration.
    ///
    /// # Arguments
    /// * `threshold` - Minimum number of signatures required
    /// * `signer_commitments` - Public key commitments of all signers
    /// * `psm_config` - PSM server configuration
    pub fn new(threshold: u32, signer_commitments: Vec<Word>, psm_config: PsmConfig) -> Self {
        Self {
            threshold,
            signer_commitments,
            psm_config,
        }
    }

    /// Validates the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.threshold == 0 {
            return Err("threshold must be greater than 0".to_string());
        }
        if self.signer_commitments.is_empty() {
            return Err("at least one signer commitment is required".to_string());
        }
        if self.threshold > self.signer_commitments.len() as u32 {
            return Err(format!(
                "threshold ({}) cannot exceed number of signers ({})",
                self.threshold,
                self.signer_commitments.len()
            ));
        }
        if self.psm_config.endpoint.is_empty() {
            return Err("PSM endpoint is required".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_objects::Word;

    fn dummy_word() -> Word {
        Word::default()
    }

    #[test]
    fn psm_config_new_creates_with_endpoint() {
        let config = PsmConfig::new("http://localhost:50051");
        assert_eq!(config.endpoint, "http://localhost:50051");
    }

    #[test]
    fn psm_config_new_accepts_string() {
        let endpoint = String::from("http://localhost:50051");
        let config = PsmConfig::new(endpoint);
        assert_eq!(config.endpoint, "http://localhost:50051");
    }

    #[test]
    fn multisig_config_new_sets_all_fields() {
        let signers = vec![dummy_word(), dummy_word()];
        let psm = PsmConfig::new("http://psm:50051");
        let config = MultisigConfig::new(2, signers.clone(), psm);

        assert_eq!(config.threshold, 2);
        assert_eq!(config.signer_commitments.len(), 2);
        assert_eq!(config.psm_config.endpoint, "http://psm:50051");
    }

    #[test]
    fn validate_threshold_zero_returns_error() {
        let config = MultisigConfig::new(0, vec![dummy_word()], PsmConfig::new("http://psm:50051"));
        let err = config.validate().unwrap_err();
        assert!(err.contains("threshold"));
        assert!(err.contains("greater than 0"));
    }

    #[test]
    fn validate_empty_commitments_returns_error() {
        let config = MultisigConfig::new(1, vec![], PsmConfig::new("http://psm:50051"));
        let err = config.validate().unwrap_err();
        assert!(err.contains("signer"));
    }

    #[test]
    fn validate_threshold_exceeds_signers_returns_error() {
        let config = MultisigConfig::new(
            3,
            vec![dummy_word(), dummy_word()],
            PsmConfig::new("http://psm:50051"),
        );
        let err = config.validate().unwrap_err();
        assert!(err.contains("exceed"));
        assert!(err.contains("3"));
        assert!(err.contains("2"));
    }

    #[test]
    fn validate_empty_psm_endpoint_returns_error() {
        let config = MultisigConfig::new(1, vec![dummy_word()], PsmConfig::new(""));
        let err = config.validate().unwrap_err();
        assert!(err.contains("PSM endpoint"));
    }

    #[test]
    fn validate_valid_1_of_1_config() {
        let config = MultisigConfig::new(1, vec![dummy_word()], PsmConfig::new("http://psm:50051"));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_valid_2_of_3_config() {
        let config = MultisigConfig::new(
            2,
            vec![dummy_word(), dummy_word(), dummy_word()],
            PsmConfig::new("http://psm:50051"),
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_valid_3_of_3_config() {
        let config = MultisigConfig::new(
            3,
            vec![dummy_word(), dummy_word(), dummy_word()],
            PsmConfig::new("http://psm:50051"),
        );
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_threshold_equals_signers_is_valid() {
        let config = MultisigConfig::new(
            2,
            vec![dummy_word(), dummy_word()],
            PsmConfig::new("http://psm:50051"),
        );
        assert!(config.validate().is_ok());
    }
}
