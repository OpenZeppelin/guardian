use miden_objects::account::AccountId;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey, Signature};
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::Serializable;
use miden_objects::{Felt, FieldElement, Word};

pub struct FalconRpoSigner {
    secret_key: SecretKey,
    public_key: PublicKey,
}

impl FalconRpoSigner {
    pub fn new(secret_key: SecretKey) -> Self {
        let public_key = secret_key.public_key();
        Self {
            secret_key,
            public_key,
        }
    }

    pub fn public_key_hex(&self) -> String {
        let pubkey_word: Word = self.public_key.into();
        format!("0x{}", hex::encode(pubkey_word.to_bytes()))
    }

    pub fn sign_account_id(&self, account_id: &AccountId) -> String {
        let message = account_id_to_digest(account_id);
        let signature = self.secret_key.sign(message);
        signature_to_hex(&signature)
    }
}

fn account_id_to_digest(account_id: &AccountId) -> Word {
    let account_id_felts: [Felt; 2] = (*account_id).into();

    let message_elements = vec![
        account_id_felts[0],
        account_id_felts[1],
        Felt::ZERO,
        Felt::ZERO,
    ];

    Rpo256::hash_elements(&message_elements)
}

fn signature_to_hex(signature: &Signature) -> String {
    use miden_objects::utils::Serializable;
    let signature_bytes = signature.to_bytes();
    format!("0x{}", hex::encode(&signature_bytes))
}

pub fn verify_commitment_signature(
    commitment_hex: &str,
    server_pubkey_hex: &str,
    signature_hex: &str,
) -> Result<bool, String> {
    let message = commitment_to_digest(commitment_hex)?;
    let pubkey = parse_public_key(server_pubkey_hex)?;
    let signature = parse_signature(signature_hex)?;

    Ok(pubkey.verify(message, &signature))
}

fn commitment_to_digest(commitment_hex: &str) -> Result<Word, String> {
    let commitment_hex = commitment_hex.strip_prefix("0x").unwrap_or(commitment_hex);

    let bytes = hex::decode(commitment_hex).map_err(|e| format!("Invalid commitment hex: {e}"))?;

    if bytes.len() != 32 {
        return Err(format!("Commitment must be 32 bytes, got {}", bytes.len()));
    }

    let mut felts = Vec::new();
    for chunk in bytes.chunks(8) {
        let mut arr = [0u8; 8];
        arr[..chunk.len()].copy_from_slice(chunk);
        let value = u64::from_le_bytes(arr);
        felts.push(Felt::try_from(value).map_err(|e| format!("Invalid field element: {e}"))?);
    }

    let message_elements = vec![felts[0], felts[1], felts[2], felts[3]];
    let digest = Rpo256::hash_elements(&message_elements);
    Ok(digest)
}

fn parse_public_key(hex_str: &str) -> Result<PublicKey, String> {
    let word = Word::try_from(hex_str).map_err(|e| format!("Invalid public key hex: {e}"))?;
    Ok(PublicKey::new(word))
}

fn parse_signature(hex_str: &str) -> Result<Signature, String> {
    use miden_objects::utils::Deserializable;

    let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
    let bytes = hex::decode(hex_str).map_err(|e| format!("Invalid signature hex: {e}"))?;

    const EXPECTED_SIG_LEN: usize = 1563;
    if bytes.len() != EXPECTED_SIG_LEN {
        return Err(format!(
            "Signature must be exactly {EXPECTED_SIG_LEN} bytes, got {} bytes",
            bytes.len()
        ));
    }

    Signature::read_from_bytes(&bytes).map_err(|e| format!("Failed to deserialize signature: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_falcon_signer_creates_valid_signature() {
        let secret_key = SecretKey::new();
        let signer = FalconRpoSigner::new(secret_key);

        let account_id = AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").unwrap();
        let signature_hex = signer.sign_account_id(&account_id);

        assert!(signature_hex.starts_with("0x"));
        assert_eq!(signature_hex.len(), 2 + (1563 * 2));
    }

    #[test]
    fn test_pubkey_hex_format() {
        let secret_key = SecretKey::new();
        let signer = FalconRpoSigner::new(secret_key);
        let pubkey_hex = signer.public_key_hex();

        assert!(pubkey_hex.starts_with("0x"));
        assert_eq!(pubkey_hex.len(), 2 + (32 * 2));
    }
}
