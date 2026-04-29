use std::str::FromStr;

use alloy::primitives::{Address, B256, Bytes, Signature, U256, aliases::U192, keccak256};
use alloy::providers::ProviderBuilder;
use alloy::sol;
use alloy::sol_types::{SolStruct, SolValue, eip712_domain};

use crate::error::{GuardianError, Result};
use crate::evm::config::EvmChainConfig;
use crate::evm::proposal::NormalizedEvmProposalInput;
use crate::evm::session::EvmChallenge;
use crate::metadata::network::normalize_evm_address;

sol! {
    #[sol(rpc)]
    interface IERC7579Account {
        function isModuleInstalled(uint256 moduleTypeId, address module, bytes additionalContext) external view returns (bool);
    }

    #[sol(rpc)]
    interface IERC7579MultisigValidator {
        function getSigners(address account, uint256 start, uint256 end) external view returns (bytes[]);
        function getSignerCount(address account) external view returns (uint256);
        function threshold(address account) external view returns (uint64);
    }

    #[sol(rpc)]
    interface IEntryPoint {
        function getNonce(address sender, uint192 key) external view returns (uint256);
    }

    struct GuardianEvmSession {
        address wallet;
        bytes32 nonce;
        uint64 issued_at;
        uint64 expires_at;
    }
}

#[derive(Clone)]
pub struct EvmContractReader {
    config: EvmChainConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmSignerSnapshot {
    pub signers: Vec<String>,
    pub threshold: usize,
}

impl EvmContractReader {
    pub fn new(config: EvmChainConfig) -> Self {
        Self { config }
    }

    pub async fn ensure_validator_installed(
        &self,
        smart_account_address: &str,
        validator_address: &str,
    ) -> Result<()> {
        let provider = self.provider()?;
        let account_address = parse_address(smart_account_address)?;
        let validator_address = parse_address(validator_address)?;
        let account = IERC7579Account::new(account_address, provider);
        let installed = account
            .isModuleInstalled(U256::from(1u64), validator_address, Bytes::new())
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))?;
        if installed {
            Ok(())
        } else {
            Err(GuardianError::RpcValidationFailed(
                "multisig validator is not installed on smart account".to_string(),
            ))
        }
    }

    pub async fn signer_snapshot(
        &self,
        smart_account_address: &str,
        validator_address: &str,
    ) -> Result<EvmSignerSnapshot> {
        let provider = self.provider()?;
        let account = parse_address(smart_account_address)?;
        let validator = IERC7579MultisigValidator::new(parse_address(validator_address)?, provider);
        let signer_count = validator
            .getSignerCount(account)
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))?;
        if signer_count > U256::from(1024u64) {
            return Err(GuardianError::RpcValidationFailed(format!(
                "signer count {signer_count} exceeds safety limit"
            )));
        }
        let raw_signers = validator
            .getSigners(account, U256::ZERO, signer_count)
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))?;
        let signers = raw_signers
            .into_iter()
            .map(|signer| signer_bytes_to_address(&signer))
            .collect::<Result<Vec<_>>>()?;
        if signers.is_empty() {
            return Err(GuardianError::RpcValidationFailed(
                "multisig validator returned no EOA signers".to_string(),
            ));
        }

        let threshold = validator
            .threshold(account)
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))?
            as usize;
        if threshold == 0 || threshold > signers.len() {
            return Err(GuardianError::RpcValidationFailed(format!(
                "unreachable threshold {threshold} for {} signer(s)",
                signers.len()
            )));
        }

        Ok(EvmSignerSnapshot { signers, threshold })
    }

    pub async fn entrypoint_nonce(
        &self,
        smart_account_address: &str,
        nonce_key: &str,
    ) -> Result<String> {
        let provider = self.provider()?;
        let entrypoint =
            IEntryPoint::new(parse_address(&self.config.entrypoint_address)?, provider);
        let key = nonce_key
            .parse::<U192>()
            .map_err(|e| GuardianError::InvalidEvmProposal(format!("invalid nonce key: {e}")))?;
        let nonce = entrypoint
            .getNonce(parse_address(smart_account_address)?, key)
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))?;
        Ok(nonce.to_string())
    }

    fn provider(&self) -> Result<impl alloy::providers::Provider + Clone> {
        let url =
            self.config.rpc_url.parse().map_err(|e| {
                GuardianError::InvalidNetworkConfig(format!("invalid RPC URL: {e}"))
            })?;
        Ok(ProviderBuilder::new().connect_http(url))
    }
}

pub fn verify_proposal_signature(
    input: &NormalizedEvmProposalInput,
    signature: &str,
) -> Result<String> {
    recover_hash_address(&input.hash_bytes, signature)
}

pub fn recover_session_address(challenge: &EvmChallenge, signature: &str) -> Result<String> {
    let wallet = parse_address(&challenge.address)?;
    let nonce = parse_b256(&challenge.nonce, "nonce")?;
    let domain = eip712_domain! {
        name: "Guardian EVM Session",
        version: "1",
    };
    let message = GuardianEvmSession {
        wallet,
        nonce,
        issued_at: challenge.issued_at.timestamp() as u64,
        expires_at: challenge.expires_at.timestamp() as u64,
    };
    let hash = message.eip712_signing_hash(&domain);
    recover_address_from_b256(&hash, signature)
        .map_err(|e| GuardianError::AuthenticationFailed(format!("recover failed: {e}")))
}

pub fn recover_hash_address(hash: &[u8; 32], signature: &str) -> Result<String> {
    let hash = B256::from_slice(hash);
    recover_address_from_b256(&hash, signature)
        .map_err(|e| GuardianError::InvalidProposalSignature(format!("recover failed: {e}")))
}

pub fn compute_proposal_id(input: &NormalizedEvmProposalInput) -> Result<String> {
    let encoded = (
        U256::from(input.chain_id),
        parse_address(&input.smart_account_address)?,
        parse_address(&input.validator_address)?,
        B256::from_slice(&input.hash_bytes),
        parse_u256_decimal(&input.nonce.decimal)?,
    )
        .abi_encode();
    Ok(format!("0x{}", hex::encode(keccak256(encoded))))
}

fn recover_address_from_b256(
    hash: &B256,
    signature_hex: &str,
) -> std::result::Result<String, String> {
    let signature = Signature::from_str(signature_hex).map_err(|e| e.to_string())?;
    let address = signature
        .recover_address_from_prehash(hash)
        .map_err(|e| e.to_string())?;
    Ok(format!("{address:?}"))
}

fn parse_address(value: &str) -> Result<Address> {
    let normalized = normalize_evm_address(value).map_err(GuardianError::InvalidInput)?;
    let bytes = hex::decode(normalized.trim_start_matches("0x"))
        .map_err(|e| GuardianError::InvalidInput(e.to_string()))?;
    Ok(Address::from_slice(&bytes))
}

fn parse_b256(value: &str, field: &str) -> Result<B256> {
    let clean = value
        .strip_prefix("0x")
        .ok_or_else(|| GuardianError::InvalidEvmProposal(format!("{field} must start with 0x")))?;
    if clean.len() != 64 {
        return Err(GuardianError::InvalidEvmProposal(format!(
            "{field} must be 32 bytes"
        )));
    }
    let bytes = hex::decode(clean)
        .map_err(|e| GuardianError::InvalidEvmProposal(format!("{field} is invalid hex: {e}")))?;
    Ok(B256::from_slice(&bytes))
}

fn parse_u256_decimal(value: &str) -> Result<U256> {
    value
        .parse::<U256>()
        .map_err(|e| GuardianError::InvalidEvmProposal(format!("invalid uint256: {e}")))
}

fn signer_bytes_to_address(value: &Bytes) -> Result<String> {
    if value.len() != 20 {
        return Err(GuardianError::RpcValidationFailed(format!(
            "EVM v1 supports 20-byte EOA signers only; got {} bytes",
            value.len()
        )));
    }
    Ok(format!("0x{}", hex::encode(value)))
}

#[cfg(test)]
mod tests {
    use alloy::signers::{SignerSync, local::PrivateKeySigner};
    use chrono::{TimeZone, Utc};

    use super::*;

    fn test_signer() -> PrivateKeySigner {
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
            .parse()
            .expect("private key")
    }

    fn signature_hex(signature: alloy::primitives::Signature) -> String {
        format!("0x{}", hex::encode(signature.as_bytes()))
    }

    #[test]
    fn recovers_proposal_hash_signer() {
        let signer = test_signer();
        let hash_bytes = [7u8; 32];
        let hash = B256::from(hash_bytes);
        let signature = signature_hex(signer.sign_hash_sync(&hash).expect("signature"));

        let recovered = recover_hash_address(&hash_bytes, &signature).expect("recovered");

        assert_eq!(recovered, format!("{:?}", signer.address()));
    }

    #[test]
    fn recovers_session_typed_data_signer() {
        let signer = test_signer();
        let issued_at = Utc.timestamp_opt(1_700_000_000, 0).single().expect("time");
        let expires_at = Utc.timestamp_opt(1_700_000_300, 0).single().expect("time");
        let challenge = EvmChallenge {
            address: format!("{:?}", signer.address()),
            nonce: format!("0x{}", "11".repeat(32)),
            issued_at,
            expires_at,
        };
        let domain = eip712_domain! {
            name: "Guardian EVM Session",
            version: "1",
        };
        let message = GuardianEvmSession {
            wallet: signer.address(),
            nonce: parse_b256(&challenge.nonce, "nonce").expect("nonce"),
            issued_at: issued_at.timestamp() as u64,
            expires_at: expires_at.timestamp() as u64,
        };
        let signature = signature_hex(
            signer
                .sign_hash_sync(&message.eip712_signing_hash(&domain))
                .expect("signature"),
        );

        let recovered = recover_session_address(&challenge, &signature).expect("recovered");

        assert_eq!(recovered, format!("{:?}", signer.address()));
    }
}
