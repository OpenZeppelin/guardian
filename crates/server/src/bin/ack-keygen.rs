use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey as FalconSecretKey;
use miden_protocol::utils::Serializable;
use serde::Serialize;

#[derive(Serialize)]
struct AckKeys {
    falcon_secret_key: String,
    ecdsa_secret_key: String,
}

fn main() {
    let falcon_secret = FalconSecretKey::new();
    let ecdsa_secret = EcdsaSecretKey::new();

    let keys = AckKeys {
        falcon_secret_key: hex::encode(falcon_secret.to_bytes()),
        ecdsa_secret_key: hex::encode(ecdsa_secret.to_bytes()),
    };

    println!(
        "{}",
        serde_json::to_string(&keys).expect("Failed to serialize ack keys")
    );
}
