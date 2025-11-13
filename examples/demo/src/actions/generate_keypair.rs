use crate::display::{print_keypair_generated, print_success, print_waiting};
use crate::falcon::generate_falcon_keypair;
use crate::state::SessionState;

pub async fn action_generate_keypair(state: &mut SessionState) -> Result<(), String> {
    print_waiting("Generating Falcon keypair");

    let keystore = state.get_keystore();
    let (commitment_hex, secret_key) = generate_falcon_keypair(keystore)?;

    state.set_keypair(commitment_hex.clone(), secret_key);

    print_keypair_generated(&commitment_hex);
    print_success("Keypair generated and added to keystore");

    Ok(())
}
