use crate::delta_object::ProposalSignature;
use crate::error::{GuardianError, Result};
use crate::metadata::NetworkConfig;
use crate::metadata::auth::Credentials;
use crate::metadata::network::{evm_account_id, normalize_evm_address};
use alloy::primitives::{Address, B256, Bytes, Signature, U256, keccak256};
use alloy::providers::ProviderBuilder;
use alloy::sol;
use alloy::sol_types::{SolStruct, SolValue, eip712_domain};
use std::str::FromStr;

sol! {
    #[sol(rpc)]
    interface ERC7579Multisig {
        function getSigners(address account, uint256 start, uint256 end) external view returns (bytes[]);
        function getSignerCount(address account) external view returns (uint256);
        function isSigner(address account, bytes signer) external view returns (bool);
        function threshold(address account) external view returns (uint64);
    }

    struct GuardianRequest {
        string account_id;
        uint64 timestamp;
        bytes32 request_hash;
    }

    struct GuardianProposal {
        bytes32 mode;
        bytes32 execution_calldata_hash;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedEvmProposal {
    pub payload: serde_json::Value,
    pub mode: B256,
    pub execution_calldata: Vec<u8>,
    pub execution_calldata_hash: B256,
}

pub async fn configure_evm_account(
    network_config: &NetworkConfig,
    auth: &crate::metadata::Auth,
    account_id: &str,
    credentials: &Credentials,
) -> Result<Vec<String>> {
    let config = expect_evm_config(network_config)?;
    ensure_allowed_chain(config.chain_id)?;
    verify_request_auth(network_config, account_id, credentials)?;

    let reader = Erc7579MultisigReader::new(config)?;
    let signers = reader.signers().await?;
    if signers.is_empty() {
        return Err(GuardianError::RpcValidationFailed(
            "multisig module returned no EOA signers".to_string(),
        ));
    }

    let threshold = reader.threshold().await?;
    if threshold == 0 || threshold as usize > signers.len() {
        return Err(GuardianError::RpcValidationFailed(format!(
            "unreachable threshold {threshold} for {} signer(s)",
            signers.len()
        )));
    }

    let signer = credentials_signer_address(credentials)?;
    if !signers.iter().any(|candidate| candidate == &signer) {
        return Err(GuardianError::SignerNotAuthorized(signer));
    }

    if let crate::metadata::Auth::EvmEcdsa {
        signers: configured,
    } = auth
    {
        let normalized = configured
            .iter()
            .map(|signer| {
                normalize_evm_address(signer).map_err(GuardianError::InvalidNetworkConfig)
            })
            .collect::<Result<Vec<_>>>()?;
        for signer in &normalized {
            if !signers.iter().any(|candidate| candidate == signer) {
                return Err(GuardianError::SignerNotAuthorized(signer.clone()));
            }
        }
    }

    Ok(signers)
}

pub async fn authorize_evm_request(
    network_config: &NetworkConfig,
    account_id: &str,
    credentials: &Credentials,
) -> Result<String> {
    let config = expect_evm_config(network_config)?;
    ensure_allowed_chain(config.chain_id)?;
    verify_request_auth(network_config, account_id, credentials)?;
    let signer = credentials_signer_address(credentials)?;
    let reader = Erc7579MultisigReader::new(config)?;
    reader.ensure_signer(&signer).await?;
    Ok(signer)
}

pub fn normalize_evm_proposal_payload(payload: serde_json::Value) -> Result<NormalizedEvmProposal> {
    normalize_evm_proposal_payload_with_policy(payload, true)
}

pub fn normalize_stored_evm_proposal_payload(
    payload: serde_json::Value,
) -> Result<NormalizedEvmProposal> {
    normalize_evm_proposal_payload_with_policy(payload, false)
}

fn normalize_evm_proposal_payload_with_policy(
    payload: serde_json::Value,
    require_empty_signatures: bool,
) -> Result<NormalizedEvmProposal> {
    let obj = payload.as_object().ok_or_else(|| {
        GuardianError::InvalidEvmProposal("delta_payload must be an object".to_string())
    })?;

    let kind = obj
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| GuardianError::InvalidEvmProposal("kind is required".to_string()))?;
    if kind != "evm" {
        return Err(GuardianError::InvalidEvmProposal(
            "kind must be 'evm'".to_string(),
        ));
    }

    let mode_hex = obj
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| GuardianError::InvalidEvmProposal("mode is required".to_string()))?;
    let mode_bytes = parse_fixed_hex(mode_hex, 32, "mode")?;
    validate_supported_mode(&mode_bytes)?;
    let mode = B256::from_slice(&mode_bytes);

    let execution_calldata_hex = obj
        .get("execution_calldata")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            GuardianError::InvalidEvmProposal("execution_calldata is required".to_string())
        })?;
    let execution_calldata = parse_hex(execution_calldata_hex, "execution_calldata")?;
    let execution_calldata_hash = keccak256(&execution_calldata);

    let signatures = obj
        .get("signatures")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            GuardianError::InvalidEvmProposal("signatures must be an array".to_string())
        })?;
    if require_empty_signatures && !signatures.is_empty() {
        return Err(GuardianError::InvalidEvmProposal(
            "signatures must be empty on create".to_string(),
        ));
    }

    Ok(NormalizedEvmProposal {
        payload: serde_json::json!({
            "kind": "evm",
            "mode": format_fixed_hex(&mode_bytes),
            "execution_calldata": format!("0x{}", hex::encode(&execution_calldata)),
            "signatures": signatures.clone()
        }),
        mode,
        execution_calldata,
        execution_calldata_hash,
    })
}

pub fn compute_proposal_id(
    network_config: &NetworkConfig,
    proposal: &NormalizedEvmProposal,
) -> Result<String> {
    let config = expect_evm_config(network_config)?;
    let account = parse_address(&config.account_address)?;
    let encoded = (
        U256::from(config.chain_id),
        account,
        proposal.mode,
        proposal.execution_calldata_hash,
    )
        .abi_encode();
    Ok(format!("0x{}", hex::encode(keccak256(encoded))))
}

pub fn verify_proposal_signature(
    network_config: &NetworkConfig,
    proposal: &NormalizedEvmProposal,
    signature: &ProposalSignature,
) -> Result<String> {
    let signature_hex = match signature {
        ProposalSignature::Ecdsa { signature, .. } => signature,
        ProposalSignature::Falcon { .. } => {
            return Err(GuardianError::InvalidProposalSignature(
                "EVM proposals require ECDSA signatures".to_string(),
            ));
        }
    };

    let hash = proposal_signing_hash(network_config, proposal)?;
    recover_signature_address(signature_hex, &hash)
        .map_err(|e| GuardianError::InvalidProposalSignature(format!("recover failed: {e}")))
}

pub fn is_evm_payload(payload: &serde_json::Value) -> bool {
    payload
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == "evm")
}

fn verify_request_auth(
    network_config: &NetworkConfig,
    account_id: &str,
    credentials: &Credentials,
) -> Result<()> {
    let signer = credentials_signer_address(credentials)?;
    let (_, signature, timestamp) = credentials.as_signature().ok_or_else(|| {
        GuardianError::AuthenticationFailed(
            "EVM requests require signature credentials".to_string(),
        )
    })?;

    let hash = request_signing_hash(network_config, account_id, timestamp as u64, credentials)?;
    let recovered = recover_signature_address(signature, &hash)
        .map_err(|e| GuardianError::AuthenticationFailed(format!("recover failed: {e}")))?;
    if recovered != signer {
        return Err(GuardianError::AuthenticationFailed(
            "EVM request signature does not match x-pubkey signer address".to_string(),
        ));
    }
    Ok(())
}

fn request_signing_hash(
    network_config: &NetworkConfig,
    account_id: &str,
    timestamp: u64,
    credentials: &Credentials,
) -> Result<B256> {
    let config = expect_evm_config(network_config)?;
    let account = parse_address(&config.account_address)?;
    let request_hash = keccak256(credentials.request_payload_bytes());
    let domain = eip712_domain! {
        name: "Guardian EVM Request",
        version: "1",
        chain_id: config.chain_id,
        verifying_contract: account,
    };
    let message = GuardianRequest {
        account_id: account_id.to_string(),
        timestamp,
        request_hash,
    };
    Ok(message.eip712_signing_hash(&domain))
}

fn proposal_signing_hash(
    network_config: &NetworkConfig,
    proposal: &NormalizedEvmProposal,
) -> Result<B256> {
    let config = expect_evm_config(network_config)?;
    let account = parse_address(&config.account_address)?;
    let domain = eip712_domain! {
        name: "Guardian EVM Proposal",
        version: "1",
        chain_id: config.chain_id,
        verifying_contract: account,
    };
    let message = GuardianProposal {
        mode: proposal.mode,
        execution_calldata_hash: proposal.execution_calldata_hash,
    };
    Ok(message.eip712_signing_hash(&domain))
}

fn recover_signature_address(
    signature_hex: &str,
    hash: &B256,
) -> std::result::Result<String, String> {
    let signature = Signature::from_str(signature_hex).map_err(|e| e.to_string())?;
    let address = signature
        .recover_address_from_prehash(hash)
        .map_err(|e| e.to_string())?;
    Ok(format!("{address:?}"))
}

fn credentials_signer_address(credentials: &Credentials) -> Result<String> {
    let (signer, _, _) = credentials.as_signature().ok_or_else(|| {
        GuardianError::AuthenticationFailed(
            "EVM requests require signature credentials".to_string(),
        )
    })?;
    normalize_evm_address(signer).map_err(GuardianError::AuthenticationFailed)
}

fn ensure_allowed_chain(chain_id: u64) -> Result<()> {
    let Ok(allowed) = std::env::var("GUARDIAN_EVM_ALLOWED_CHAIN_IDS") else {
        return Ok(());
    };
    let allowed = allowed
        .split(',')
        .filter_map(|value| value.trim().parse::<u64>().ok())
        .collect::<Vec<_>>();
    if allowed.is_empty() || allowed.contains(&chain_id) {
        return Ok(());
    }
    Err(GuardianError::InvalidNetworkConfig(format!(
        "chain_id {chain_id} is not allowed"
    )))
}

struct EvmConfig<'a> {
    chain_id: u64,
    account_address: &'a str,
    multisig_module_address: &'a str,
    rpc_endpoint: &'a str,
}

fn expect_evm_config(config: &NetworkConfig) -> Result<EvmConfig<'_>> {
    match config {
        NetworkConfig::Evm {
            chain_id,
            account_address,
            multisig_module_address,
            rpc_endpoint,
        } => Ok(EvmConfig {
            chain_id: *chain_id,
            account_address,
            multisig_module_address,
            rpc_endpoint,
        }),
        NetworkConfig::Miden { .. } => Err(GuardianError::UnsupportedForNetwork {
            network: "miden".to_string(),
            operation: "evm".to_string(),
        }),
    }
}

struct Erc7579MultisigReader<'a> {
    config: EvmConfig<'a>,
}

impl<'a> Erc7579MultisigReader<'a> {
    fn new(config: EvmConfig<'a>) -> Result<Self> {
        parse_address(config.account_address)?;
        parse_address(config.multisig_module_address)?;
        Ok(Self { config })
    }

    async fn signers(&self) -> Result<Vec<String>> {
        let url = self.config.rpc_endpoint.parse().map_err(|e| {
            GuardianError::InvalidNetworkConfig(format!("invalid rpc_endpoint: {e}"))
        })?;
        let provider = ProviderBuilder::new().connect_http(url);
        let module = parse_address(self.config.multisig_module_address)?;
        let contract = ERC7579Multisig::new(module, provider);
        let account = parse_address(self.config.account_address)?;
        let signer_count = contract
            .getSignerCount(account)
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))?;
        if signer_count > U256::from(1024u64) {
            return Err(GuardianError::RpcValidationFailed(format!(
                "signer count {signer_count} exceeds safety limit"
            )));
        }
        let raw_signers = contract
            .getSigners(account, U256::ZERO, signer_count)
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))?;

        raw_signers
            .into_iter()
            .map(|signer| signer_bytes_to_address(&signer))
            .collect()
    }

    async fn threshold(&self) -> Result<u64> {
        let url = self.config.rpc_endpoint.parse().map_err(|e| {
            GuardianError::InvalidNetworkConfig(format!("invalid rpc_endpoint: {e}"))
        })?;
        let provider = ProviderBuilder::new().connect_http(url);
        let module = parse_address(self.config.multisig_module_address)?;
        let contract = ERC7579Multisig::new(module, provider);
        let account = parse_address(self.config.account_address)?;
        contract
            .threshold(account)
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))
    }

    async fn ensure_signer(&self, signer: &str) -> Result<()> {
        let url = self.config.rpc_endpoint.parse().map_err(|e| {
            GuardianError::InvalidNetworkConfig(format!("invalid rpc_endpoint: {e}"))
        })?;
        let provider = ProviderBuilder::new().connect_http(url);
        let module = parse_address(self.config.multisig_module_address)?;
        let contract = ERC7579Multisig::new(module, provider);
        let account = parse_address(self.config.account_address)?;
        let signer_bytes = Bytes::from(parse_address(signer)?.to_vec());
        let authorized = contract
            .isSigner(account, signer_bytes)
            .call()
            .await
            .map_err(|e| GuardianError::RpcUnavailable(e.to_string()))?;
        if authorized {
            Ok(())
        } else {
            Err(GuardianError::SignerNotAuthorized(signer.to_string()))
        }
    }
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

fn parse_address(value: &str) -> Result<Address> {
    let normalized = normalize_evm_address(value).map_err(GuardianError::InvalidNetworkConfig)?;
    let bytes = parse_fixed_hex(&normalized, 20, "address")?;
    Ok(Address::from_slice(&bytes))
}

fn parse_fixed_hex(value: &str, expected_len: usize, field: &str) -> Result<Vec<u8>> {
    let bytes = parse_hex(value, field)?;
    if bytes.len() != expected_len {
        return Err(GuardianError::InvalidEvmProposal(format!(
            "{field} must be {expected_len} bytes"
        )));
    }
    Ok(bytes)
}

fn parse_hex(value: &str, field: &str) -> Result<Vec<u8>> {
    let clean = value
        .strip_prefix("0x")
        .ok_or_else(|| GuardianError::InvalidEvmProposal(format!("{field} must start with 0x")))?;
    if clean.len() % 2 != 0 {
        return Err(GuardianError::InvalidEvmProposal(format!(
            "{field} must contain whole bytes"
        )));
    }
    hex::decode(clean)
        .map_err(|e| GuardianError::InvalidEvmProposal(format!("{field} is invalid hex: {e}")))
}

fn format_fixed_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn validate_supported_mode(mode: &[u8]) -> Result<()> {
    let call_type = mode[0];
    let exec_type = mode[1];
    let rest_is_zero = mode[2..].iter().all(|byte| *byte == 0);
    if matches!(call_type, 0x00 | 0x01) && exec_type == 0x00 && rest_is_zero {
        Ok(())
    } else {
        Err(GuardianError::InvalidEvmProposal(
            "mode must be single-call or batch-call with default exec type and zero selector/payload"
                .to_string(),
        ))
    }
}

pub fn canonical_account_id(network_config: &NetworkConfig) -> Result<String> {
    let config = expect_evm_config(network_config)?;
    let address = normalize_evm_address(config.account_address)
        .map_err(GuardianError::InvalidNetworkConfig)?;
    Ok(evm_account_id(config.chain_id, &address))
}
