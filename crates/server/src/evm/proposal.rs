use serde::{Deserialize, Serialize};

use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus, ProposalSignature};
use crate::error::{GuardianError, Result};
use crate::metadata::network::normalize_evm_address;

pub const EVM_PROPOSAL_KIND: &str = "evm";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvmProposalSignature {
    pub signer: String,
    pub signature: String,
    pub signed_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvmProposal {
    pub proposal_id: String,
    pub account_id: String,
    pub chain_id: u64,
    pub smart_account_address: String,
    pub validator_address: String,
    pub user_op_hash: String,
    pub payload: String,
    pub nonce: String,
    pub nonce_key: String,
    pub proposer: String,
    pub signer_snapshot: Vec<String>,
    pub threshold: usize,
    pub signatures: Vec<EvmProposalSignature>,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvmProposalFilter {
    pub chain_id: Option<u64>,
    pub smart_account_address: Option<String>,
    pub validator_address: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutableEvmProposal {
    pub hash: String,
    pub payload: String,
    pub signatures: Vec<String>,
    pub signers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedNonce {
    pub decimal: String,
    pub nonce_key: String,
    pub bytes: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedEvmProposalInput {
    pub chain_id: u64,
    pub smart_account_address: String,
    pub validator_address: String,
    pub hash: String,
    pub hash_bytes: [u8; 32],
    pub payload: String,
    pub nonce: NormalizedNonce,
    pub proposer: String,
    pub signature: EvmProposalSignature,
    pub ttl_seconds: u64,
}

impl EvmProposal {
    pub fn from_stored_delta(delta: &DeltaObject) -> Result<Self> {
        let proposal_id = proposal_id_from_delta(delta)?;
        Self::from_delta(&proposal_id, delta)
    }

    pub fn from_delta(commitment: &str, delta: &DeltaObject) -> Result<Self> {
        let payload: EvmProposalPayload = serde_json::from_value(delta.delta_payload.clone())
            .map_err(|e| GuardianError::InvalidEvmProposal(format!("invalid EVM payload: {e}")))?;
        if payload.kind != EVM_PROPOSAL_KIND {
            return Err(GuardianError::InvalidEvmProposal(
                "delta_payload.kind must be evm".to_string(),
            ));
        }
        if payload.proposal_id != commitment {
            return Err(GuardianError::InvalidEvmProposal(
                "proposal commitment does not match payload proposal_id".to_string(),
            ));
        }
        let signatures = match &delta.status {
            DeltaStatus::Pending { cosigner_sigs, .. } => cosigner_sigs
                .iter()
                .map(EvmProposalSignature::try_from)
                .collect::<Result<Vec<_>>>()?,
            _ => {
                return Err(GuardianError::ProposalNotFound {
                    account_id: delta.account_id.clone(),
                    commitment: commitment.to_string(),
                });
            }
        };

        Ok(Self {
            proposal_id: payload.proposal_id,
            account_id: delta.account_id.clone(),
            chain_id: payload.chain_id,
            smart_account_address: payload.smart_account_address,
            validator_address: payload.validator_address,
            user_op_hash: payload.user_op_hash,
            payload: payload.payload,
            nonce: payload.nonce,
            nonce_key: payload.nonce_key,
            proposer: payload.proposer,
            signer_snapshot: payload.signer_snapshot,
            threshold: payload.threshold,
            signatures,
            created_at: payload.created_at,
            expires_at: payload.expires_at,
        })
    }

    pub fn into_delta(self) -> DeltaObject {
        let signatures = self
            .signatures
            .iter()
            .map(CosignerSignature::from)
            .collect::<Vec<_>>();
        let status = DeltaStatus::Pending {
            timestamp: self.created_at.to_string(),
            proposer_id: self.proposer.clone(),
            cosigner_sigs: signatures,
        };
        let payload = EvmProposalPayload {
            kind: EVM_PROPOSAL_KIND.to_string(),
            proposal_id: self.proposal_id,
            chain_id: self.chain_id,
            smart_account_address: self.smart_account_address,
            validator_address: self.validator_address,
            user_op_hash: self.user_op_hash,
            payload: self.payload,
            nonce: self.nonce,
            nonce_key: self.nonce_key,
            proposer: self.proposer,
            signer_snapshot: self.signer_snapshot,
            threshold: self.threshold,
            created_at: self.created_at,
            expires_at: self.expires_at,
        };

        DeltaObject {
            account_id: crate::metadata::network::evm_account_id(
                payload.chain_id,
                &payload.smart_account_address,
            ),
            nonce: payload.low_u64_nonce(),
            prev_commitment: String::new(),
            new_commitment: None,
            delta_payload: serde_json::to_value(payload)
                .expect("EVM proposal payload should always serialize"),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status,
        }
    }

    pub fn has_signer(&self, signer: &str) -> bool {
        self.signer_snapshot
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(signer))
    }

    pub fn has_signature_from(&self, signer: &str) -> bool {
        self.signatures
            .iter()
            .any(|signature| signature.signer.eq_ignore_ascii_case(signer))
    }

    pub fn signature_count(&self) -> usize {
        self.signatures.len()
    }

    pub fn is_executable(&self) -> bool {
        self.signature_count() >= self.threshold
    }

    pub fn executable(&self) -> ExecutableEvmProposal {
        ExecutableEvmProposal {
            hash: self.user_op_hash.clone(),
            payload: self.payload.clone(),
            signatures: self
                .signatures
                .iter()
                .map(|signature| signature.signature.clone())
                .collect(),
            signers: self
                .signatures
                .iter()
                .map(|signature| signature.signer.clone())
                .collect(),
        }
    }
}

pub fn proposal_id_from_delta(delta: &DeltaObject) -> Result<String> {
    delta
        .delta_payload
        .get("proposal_id")
        .and_then(serde_json::Value::as_str)
        .map(normalize_proposal_id)
        .transpose()?
        .ok_or_else(|| {
            GuardianError::InvalidEvmProposal(
                "EVM delta proposal payload is missing proposal_id".to_string(),
            )
        })
}

impl EvmProposalFilter {
    pub fn normalize(self) -> Result<Self> {
        Ok(Self {
            chain_id: self.chain_id,
            smart_account_address: self
                .smart_account_address
                .as_deref()
                .map(normalize_evm_address)
                .transpose()
                .map_err(GuardianError::InvalidInput)?,
            validator_address: self
                .validator_address
                .as_deref()
                .map(normalize_evm_address)
                .transpose()
                .map_err(GuardianError::InvalidInput)?,
        })
    }

    pub fn matches(&self, proposal: &EvmProposal) -> bool {
        if let Some(chain_id) = self.chain_id
            && proposal.chain_id != chain_id
        {
            return false;
        }
        if let Some(account) = &self.smart_account_address
            && !proposal.smart_account_address.eq_ignore_ascii_case(account)
        {
            return false;
        }
        if let Some(validator) = &self.validator_address
            && !proposal.validator_address.eq_ignore_ascii_case(validator)
        {
            return false;
        }
        true
    }
}

impl NormalizedEvmProposalInput {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        chain_id: u64,
        smart_account_address: &str,
        validator_address: &str,
        hash: &str,
        payload: String,
        nonce: &str,
        proposer: &str,
        signature: EvmProposalSignature,
        ttl_seconds: u64,
    ) -> Result<Self> {
        if chain_id == 0 {
            return Err(GuardianError::InvalidEvmProposal(
                "chain_id must be greater than zero".to_string(),
            ));
        }
        if ttl_seconds == 0 {
            return Err(GuardianError::InvalidEvmProposal(
                "ttl_seconds must be greater than zero".to_string(),
            ));
        }

        let hash_bytes = parse_fixed_hex(hash, 32, "hash")?;
        let hash = format_fixed_hex(&hash_bytes);
        let proposer = normalize_evm_address(proposer).map_err(GuardianError::InvalidInput)?;
        let signature = EvmProposalSignature {
            signer: normalize_evm_address(&signature.signer)
                .map_err(GuardianError::InvalidInput)?,
            signature: normalize_signature(&signature.signature)?,
            signed_at: signature.signed_at,
        };
        if signature.signer != proposer {
            return Err(GuardianError::SignerNotAuthorized(signature.signer));
        }

        Ok(Self {
            chain_id,
            smart_account_address: normalize_evm_address(smart_account_address)
                .map_err(GuardianError::InvalidInput)?,
            validator_address: normalize_evm_address(validator_address)
                .map_err(GuardianError::InvalidInput)?,
            hash,
            hash_bytes: hash_bytes.try_into().expect("hash length checked"),
            payload,
            nonce: normalize_nonce(nonce)?,
            proposer,
            signature,
            ttl_seconds,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvmProposalPayload {
    pub kind: String,
    pub proposal_id: String,
    pub chain_id: u64,
    pub smart_account_address: String,
    pub validator_address: String,
    pub user_op_hash: String,
    pub payload: String,
    pub nonce: String,
    pub nonce_key: String,
    pub proposer: String,
    pub signer_snapshot: Vec<String>,
    pub threshold: usize,
    pub created_at: i64,
    pub expires_at: i64,
}

impl EvmProposalPayload {
    fn low_u64_nonce(&self) -> u64 {
        normalize_nonce(&self.nonce)
            .map(|nonce| nonce.low_u64())
            .unwrap_or(0)
    }
}

impl TryFrom<&CosignerSignature> for EvmProposalSignature {
    type Error = GuardianError;

    fn try_from(value: &CosignerSignature) -> Result<Self> {
        let signature = match &value.signature {
            ProposalSignature::Ecdsa { signature, .. } => normalize_signature(signature)?,
            ProposalSignature::Falcon { .. } => {
                return Err(GuardianError::InvalidProposalSignature(
                    "EVM proposals require ECDSA signatures".to_string(),
                ));
            }
        };
        Ok(Self {
            signer: normalize_evm_address(&value.signer_id).map_err(GuardianError::InvalidInput)?,
            signature,
            signed_at: value.timestamp.parse::<i64>().unwrap_or(0),
        })
    }
}

impl From<&EvmProposalSignature> for CosignerSignature {
    fn from(value: &EvmProposalSignature) -> Self {
        Self {
            signer_id: value.signer.clone(),
            signature: ProposalSignature::Ecdsa {
                signature: value.signature.clone(),
                public_key: None,
            },
            timestamp: value.signed_at.to_string(),
        }
    }
}

pub fn normalize_signature(signature: &str) -> Result<String> {
    let bytes = parse_fixed_hex(signature, 65, "signature")?;
    Ok(format_fixed_hex(&bytes))
}

pub fn normalize_proposal_id(proposal_id: &str) -> Result<String> {
    let bytes = parse_fixed_hex(proposal_id, 32, "proposal_id")?;
    Ok(format_fixed_hex(&bytes))
}

pub fn normalize_hash(hash: &str) -> Result<(String, [u8; 32])> {
    let bytes = parse_fixed_hex(hash, 32, "hash")?;
    Ok((
        format_fixed_hex(&bytes),
        bytes.try_into().expect("hash length checked"),
    ))
}

pub fn normalize_nonce(value: &str) -> Result<NormalizedNonce> {
    let bytes = parse_u256(value)?;
    Ok(NormalizedNonce {
        decimal: u256_to_decimal(bytes),
        nonce_key: u256_to_decimal(shift_right_64(bytes)),
        bytes,
    })
}

pub fn compare_u256_decimal(left: &str, right: &str) -> Result<std::cmp::Ordering> {
    let left = parse_u256(left)?;
    let right = parse_u256(right)?;
    Ok(left.cmp(&right))
}

impl NormalizedNonce {
    pub fn low_u64(&self) -> u64 {
        u64::from_be_bytes(
            self.bytes[24..32]
                .try_into()
                .expect("u64 slice length is fixed"),
        )
    }
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

fn parse_u256(value: &str) -> Result<[u8; 32]> {
    if let Some(hex_value) = value.strip_prefix("0x") {
        if hex_value.len() > 64 {
            return Err(GuardianError::InvalidEvmProposal(
                "nonce exceeds uint256".to_string(),
            ));
        }
        if !hex_value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(GuardianError::InvalidEvmProposal(
                "nonce hex value is invalid".to_string(),
            ));
        }
        let padded = format!("{hex_value:0>64}");
        let bytes = hex::decode(padded)
            .map_err(|e| GuardianError::InvalidEvmProposal(format!("nonce is invalid: {e}")))?;
        return Ok(bytes.try_into().expect("nonce length checked"));
    }

    let mut bytes = [0u8; 32];
    let value = value.trim();
    if value.is_empty() || !value.as_bytes().iter().all(|byte| byte.is_ascii_digit()) {
        return Err(GuardianError::InvalidEvmProposal(
            "nonce must be a decimal string or 0x-prefixed hex".to_string(),
        ));
    }
    for digit in value.bytes() {
        multiply_u256_small(&mut bytes, 10)?;
        add_u256_small(&mut bytes, digit - b'0')?;
    }
    Ok(bytes)
}

fn multiply_u256_small(bytes: &mut [u8; 32], factor: u8) -> Result<()> {
    let mut carry = 0u16;
    for byte in bytes.iter_mut().rev() {
        let next = (*byte as u16) * (factor as u16) + carry;
        *byte = (next & 0xff) as u8;
        carry = next >> 8;
    }
    if carry > 0 {
        return Err(GuardianError::InvalidEvmProposal(
            "nonce exceeds uint256".to_string(),
        ));
    }
    Ok(())
}

fn add_u256_small(bytes: &mut [u8; 32], value: u8) -> Result<()> {
    let mut carry = value as u16;
    for byte in bytes.iter_mut().rev() {
        let next = (*byte as u16) + carry;
        *byte = (next & 0xff) as u8;
        carry = next >> 8;
        if carry == 0 {
            return Ok(());
        }
    }
    Err(GuardianError::InvalidEvmProposal(
        "nonce exceeds uint256".to_string(),
    ))
}

fn shift_right_64(mut bytes: [u8; 32]) -> [u8; 32] {
    for index in (0..32).rev() {
        bytes[index] = if index >= 8 { bytes[index - 8] } else { 0 };
    }
    bytes
}

fn u256_to_decimal(bytes: [u8; 32]) -> String {
    if bytes.iter().all(|byte| *byte == 0) {
        return "0".to_string();
    }
    let mut work = bytes;
    let mut digits = Vec::new();
    while work.iter().any(|byte| *byte != 0) {
        let remainder = div_mod_u256_small(&mut work, 10);
        digits.push((b'0' + remainder) as char);
    }
    digits.iter().rev().collect()
}

fn div_mod_u256_small(bytes: &mut [u8; 32], divisor: u8) -> u8 {
    let mut remainder = 0u16;
    for byte in bytes.iter_mut() {
        let value = (remainder << 8) + (*byte as u16);
        *byte = (value / divisor as u16) as u8;
        remainder = value % divisor as u16;
    }
    remainder as u8
}

fn format_fixed_hex(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_decimal_nonce_and_key() {
        let nonce = normalize_nonce("18446744073709551616").expect("valid nonce");

        assert_eq!(nonce.decimal, "18446744073709551616");
        assert_eq!(nonce.nonce_key, "1");
    }

    #[test]
    fn normalizes_hex_nonce() {
        let nonce = normalize_nonce("0x0100").expect("valid nonce");

        assert_eq!(nonce.decimal, "256");
    }

    #[test]
    fn rejects_invalid_signature_length() {
        let err = normalize_signature("0x1234").unwrap_err();

        assert_eq!(err.code(), "invalid_evm_proposal");
    }

    #[test]
    fn executable_threshold_uses_stored_signatures() {
        let mut proposal = EvmProposal {
            proposal_id: format!("0x{}", "11".repeat(32)),
            account_id: "evm:31337:0x1111111111111111111111111111111111111111".to_string(),
            chain_id: 31337,
            smart_account_address: "0x1111111111111111111111111111111111111111".to_string(),
            validator_address: "0x2222222222222222222222222222222222222222".to_string(),
            user_op_hash: format!("0x{}", "33".repeat(32)),
            payload: "0x".to_string(),
            nonce: "0".to_string(),
            nonce_key: "0".to_string(),
            proposer: "0x1111111111111111111111111111111111111111".to_string(),
            signer_snapshot: vec![
                "0x1111111111111111111111111111111111111111".to_string(),
                "0x2222222222222222222222222222222222222222".to_string(),
            ],
            threshold: 2,
            signatures: vec![EvmProposalSignature {
                signer: "0x1111111111111111111111111111111111111111".to_string(),
                signature: format!("0x{}", "44".repeat(65)),
                signed_at: 1,
            }],
            created_at: 1,
            expires_at: 2,
        };

        assert!(!proposal.is_executable());
        proposal.signatures.push(EvmProposalSignature {
            signer: "0x2222222222222222222222222222222222222222".to_string(),
            signature: format!("0x{}", "55".repeat(65)),
            signed_at: 2,
        });
        assert!(proposal.is_executable());
    }
}
