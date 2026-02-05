pub mod miden_ecdsa;
pub mod miden_falcon_rpo;

use crate::delta_object::DeltaObject;
use crate::error::Result;
use private_state_manager_shared::SignatureScheme;
use std::path::PathBuf;

pub use miden_ecdsa::MidenEcdsaSigner;
pub use miden_falcon_rpo::MidenFalconRpoSigner;

/// Registry holding both Falcon and ECDSA signers.
///
/// Services pick the correct signer based on the account's auth scheme.
#[derive(Clone)]
pub struct AckRegistry {
    falcon: MidenFalconRpoSigner,
    ecdsa: MidenEcdsaSigner,
}

impl AckRegistry {
    pub fn new(keystore_path: PathBuf) -> Result<Self> {
        let falcon = MidenFalconRpoSigner::new(keystore_path.clone())?;
        let ecdsa = MidenEcdsaSigner::new(keystore_path)?;
        Ok(Self { falcon, ecdsa })
    }

    pub fn pubkey(&self, scheme: &SignatureScheme) -> String {
        match scheme {
            SignatureScheme::Falcon => self.falcon.pubkey_hex(),
            SignatureScheme::Ecdsa => self.ecdsa.pubkey_hex(),
        }
    }

    pub fn commitment(&self, scheme: &SignatureScheme) -> String {
        match scheme {
            SignatureScheme::Falcon => self.falcon.commitment_hex(),
            SignatureScheme::Ecdsa => self.ecdsa.commitment_hex(),
        }
    }

    pub fn ack_delta(&self, delta: DeltaObject, scheme: &SignatureScheme) -> Result<DeltaObject> {
        match scheme {
            SignatureScheme::Falcon => Ok(self.falcon.ack_delta(delta)?),
            SignatureScheme::Ecdsa => Ok(self.ecdsa.ack_delta(delta)?),
        }
    }
}
