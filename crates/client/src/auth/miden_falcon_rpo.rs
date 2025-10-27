use miden_objects::account::AccountId;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey, Signature};
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::Deserializable;
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
        let message = account_id.into_word();
        let signature = self.secret_key.sign(message);
        signature.into_hex()
    }
}

pub trait IntoWord {
    fn into_word(&self) -> Word;
}

impl IntoWord for AccountId {
    fn into_word(&self) -> Word {
        let account_id_felts: [Felt; 2] = (*self).into();

        let message_elements = vec![
            account_id_felts[0],
            account_id_felts[1],
            Felt::ZERO,
            Felt::ZERO,
        ];

        Rpo256::hash_elements(&message_elements)
    }
}

pub trait IntoHex {
    fn into_hex(&self) -> String;
}

impl IntoHex for Signature {
    fn into_hex(&self) -> String {
        use miden_objects::utils::Serializable;
        let signature_bytes = self.to_bytes();
        format!("0x{}", hex::encode(&signature_bytes))
    }
}

pub fn verify_commitment_signature(
    commitment_hex: &str,
    server_pubkey_hex: &str,
    signature_hex: &str,
) -> Result<bool, String> {
    let message = commitment_hex.hex_into_word()?;
    let pubkey = server_pubkey_hex.hex_into_public_key()?;
    let signature = signature_hex.hex_into_signature()?;

    Ok(pubkey.verify(message, &signature))
}

pub trait HexIntoWord {
    fn hex_into_word(&self) -> Result<Word, String>;
}

impl HexIntoWord for &str {
    fn hex_into_word(&self) -> Result<Word, String> {
        let commitment_hex = self.strip_prefix("0x").unwrap_or(&self);

        let bytes =
            hex::decode(commitment_hex).map_err(|e| format!("Invalid commitment hex: {e}"))?;

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
}

pub trait HexIntoPublicKey {
    fn hex_into_public_key(&self) -> Result<PublicKey, String>;
}

impl HexIntoPublicKey for &str {
    fn hex_into_public_key(&self) -> Result<PublicKey, String> {
        let word = Word::try_from(*self).map_err(|e| format!("Invalid public key hex: {e}"))?;
        Ok(PublicKey::new(word))
    }
}

pub trait HexIntoSignature {
    fn hex_into_signature(&self) -> Result<Signature, String>;
}

impl HexIntoSignature for &str {
    fn hex_into_signature(&self) -> Result<Signature, String> {
        let hex_str = self.strip_prefix("0x").unwrap_or(&self);
        let bytes = hex::decode(hex_str).map_err(|e| format!("Invalid signature hex: {e}"))?;

        const EXPECTED_SIG_LEN: usize = 1563;
        if bytes.len() != EXPECTED_SIG_LEN {
            return Err(format!(
                "Signature must be exactly {EXPECTED_SIG_LEN} bytes, got {} bytes",
                bytes.len()
            ));
        }

        Signature::read_from_bytes(&bytes)
            .map_err(|e| format!("Failed to deserialize signature: {e}"))
    }
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
