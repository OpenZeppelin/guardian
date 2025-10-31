use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use miden_objects::Word;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTransaction {
    pub tx_summary_json: String,
    pub tx_summary_commitment_hex: String,
    pub new_threshold: u64,
    pub signer_commitments_hex: Vec<String>, // ALL commitments for final tx (including new cosigner)
    pub signer_pubkeys_hex: Vec<String>,      // ALL public keys (matching signer_commitments_hex order)
    pub signers_required_hex: Vec<String>,   // Only EXISTING commitments that need to sign
    pub salt_hex: String,
    pub psm_commitment_hex: String,
    pub current_nonce: u64,
    pub prev_commitment: String,
    pub collected_signatures: HashMap<String, String>, // commitment_hex -> signature_hex
}

impl PendingTransaction {
    pub fn tx_summary_commitment(&self) -> Word {
        hex_to_word(&self.tx_summary_commitment_hex)
    }

    pub fn signer_commitments(&self) -> Vec<Word> {
        self.signer_commitments_hex.iter().map(|h| hex_to_word(h)).collect()
    }

    pub fn salt(&self) -> Word {
        hex_to_word(&self.salt_hex)
    }

    pub fn psm_commitment(&self) -> Word {
        hex_to_word(&self.psm_commitment_hex)
    }
}

fn hex_to_word(hex: &str) -> Word {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    let bytes = hex::decode(hex).expect("Invalid hex");
    let mut word = [0u64; 4];
    for (i, chunk) in bytes.chunks(8).enumerate() {
        let mut arr = [0u8; 8];
        arr[..chunk.len()].copy_from_slice(chunk);
        word[i] = u64::from_le_bytes(arr);
    }
    Word::from(word.map(miden_objects::Felt::new))
}

pub struct PendingTxStore {
    storage_path: PathBuf,
}

impl PendingTxStore {
    pub fn new(storage_path: PathBuf) -> Self {
        Self { storage_path }
    }

    pub fn save(&self, pending_tx: &PendingTransaction) -> Result<(), String> {
        fs::create_dir_all(&self.storage_path)
            .map_err(|e| format!("Failed to create storage directory: {}", e))?;

        let file_path = self.storage_path.join("pending_tx.json");
        let json = serde_json::to_string_pretty(pending_tx)
            .map_err(|e| format!("Failed to serialize pending transaction: {}", e))?;

        fs::write(&file_path, json)
            .map_err(|e| format!("Failed to write pending transaction: {}", e))?;

        Ok(())
    }

    pub fn load(&self) -> Result<PendingTransaction, String> {
        let file_path = self.storage_path.join("pending_tx.json");

        if !file_path.exists() {
            return Err("No pending transaction found".to_string());
        }

        let json = fs::read_to_string(&file_path)
            .map_err(|e| format!("Failed to read pending transaction: {}", e))?;

        let pending_tx = serde_json::from_str(&json)
            .map_err(|e| format!("Failed to deserialize pending transaction: {}", e))?;

        Ok(pending_tx)
    }

    pub fn add_signature(&self, commitment_hex: String, signature_hex: String) -> Result<(), String> {
        let mut pending_tx = self.load()?;
        pending_tx.collected_signatures.insert(commitment_hex, signature_hex);
        self.save(&pending_tx)
    }

    pub fn clear(&self) -> Result<(), String> {
        let file_path = self.storage_path.join("pending_tx.json");
        if file_path.exists() {
            fs::remove_file(&file_path)
                .map_err(|e| format!("Failed to remove pending transaction: {}", e))?;
        }
        Ok(())
    }

    pub fn has_pending(&self) -> bool {
        self.storage_path.join("pending_tx.json").exists()
    }
}
