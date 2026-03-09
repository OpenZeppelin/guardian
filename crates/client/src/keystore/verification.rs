use miden_protocol::Word;
use miden_protocol::crypto::dsa::falcon512_rpo::Signature;
use miden_protocol::utils::Deserializable;
use miden_protocol::utils::Serializable;
use private_state_manager_shared::hex::FromHex;

/// Verifies a signature using commitment-based authentication.
///
/// This function verifies that a signature was created by a key whose
/// commitment matches the expected server commitment.
pub fn verify_commitment_signature(
    commitment_hex: &str,
    server_commitment_hex: &str,
    signature_hex: &str,
) -> Result<bool, String> {
    let commitment_bytes = hex::decode(commitment_hex.strip_prefix("0x").unwrap_or(commitment_hex))
        .map_err(|e| format!("Invalid commitment hex: {e}"))?;

    if commitment_bytes.len() != 32 {
        return Err(format!(
            "Commitment must be 32 bytes, got {}",
            commitment_bytes.len()
        ));
    }

    let message = Word::read_from_bytes(&commitment_bytes)
        .map_err(|e| format!("Failed to deserialize Word from bytes: {e}"))?;
    let signature = Signature::from_hex(signature_hex)?;

    let pubkey = signature.public_key();
    let sig_pubkey_commitment = pubkey.to_commitment();
    let sig_commitment_hex = format!("0x{}", hex::encode(sig_pubkey_commitment.to_bytes()));

    if sig_commitment_hex != server_commitment_hex {
        return Ok(false);
    }

    Ok(pubkey.verify(message, &signature))
}
