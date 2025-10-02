use private_state_manager_client::{FromJson, ToJson};
use private_state_manager_client::miden_lib::account::{auth::AuthRpoFalcon512, wallets::BasicWallet};
use private_state_manager_client::miden_objects::{account::AccountBuilder, crypto::dsa::rpo_falcon512::PublicKey};

fn main() {
    let public_key = PublicKey::new([true; 4].into());

    let (account, _) = AccountBuilder::new([0xff; 32])
        .with_auth_component(AuthRpoFalcon512::new(public_key))
        .with_component(BasicWallet)
        .build()
        .unwrap();

    let json = account.to_json();
    println!("{:?}", json);
    let account = private_state_manager_client::miden_objects::account::Account::from_json(&json).unwrap();

    let json = account.to_json();
    println!("{:?}", json);
}
