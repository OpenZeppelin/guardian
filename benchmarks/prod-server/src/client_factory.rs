use crate::model::AuthScheme;
use anyhow::Result;
use guardian_client::{Auth, EcdsaSigner, FalconRpoSigner, GuardianClient};
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SecretKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;

pub fn build_auth(scheme: AuthScheme) -> Auth {
    match scheme {
        AuthScheme::Falcon => Auth::FalconRpoSigner(FalconRpoSigner::new(FalconSecretKey::new())),
        AuthScheme::Ecdsa => Auth::EcdsaSigner(EcdsaSigner::new(EcdsaSecretKey::new())),
    }
}

pub async fn connect(endpoint: &str) -> Result<GuardianClient> {
    Ok(GuardianClient::connect(endpoint.to_string()).await?)
}
