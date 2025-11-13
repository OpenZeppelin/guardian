use miden_client::Serializable;
use miden_objects::crypto::dsa::rpo_falcon512::PublicKey;
use private_state_manager_shared::hex::FromHex;

use crate::display::{print_keypair_generated, print_success, print_waiting};
use crate::falcon::generate_falcon_keypair;
use crate::state::SessionState;

pub fn pubkey_commitment_hex(pubkey_hex: &str) -> Option<String> {
    PublicKey::from_hex(pubkey_hex).ok().map(|pk| {
        let commitment = pk.to_commitment();
        format!("0x{}", hex::encode(commitment.to_bytes()))
    })
}

pub async fn action_generate_keypair(state: &mut SessionState) -> Result<(), String> {
    print_waiting("Generating Falcon keypair");

    let keystore = state.get_keystore();
    let (commitment_hex, secret_key) = generate_falcon_keypair(keystore)?;

    state.set_keypair(commitment_hex.clone(), secret_key);

    print_keypair_generated(&commitment_hex);
    print_success("Keypair generated and added to keystore");

    Ok(())
}
