use miden_client::Serializable;
use miden_objects::account::Account;
use miden_objects::Word;

pub struct AccountInspector<'a> {
    account: &'a Account,
}

impl<'a> AccountInspector<'a> {
    pub fn new(account: &'a Account) -> Self {
        Self { account }
    }

    /// Extract commitments from account storage slot 1 (multisig mapping)
    /// Returns empty vector if slot 1 is empty or has no entries
    pub fn extract_cosigner_commitments(&self) -> Vec<String> {
        let mut commitments = Vec::new();

        let key_zero = Word::from([0u32, 0, 0, 0]);
        let first_entry = self.account.storage().get_map_item(1, key_zero);

        if first_entry.is_err() || first_entry.as_ref().unwrap() == &Word::default() {
            return commitments;
        }

        let mut index = 0u32;
        loop {
            let key = Word::from([index, 0, 0, 0]);
            match self.account.storage().get_map_item(1, key) {
                Ok(value) if value != Word::default() => {
                    let commitment_hex = format!("0x{}", hex::encode(value.to_bytes()));
                    commitments.push(commitment_hex);
                    index += 1;
                }
                _ => break,
            }
        }

        commitments
    }
}
