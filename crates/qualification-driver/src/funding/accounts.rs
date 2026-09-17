use miden_protocol::crypto::dsa::ecdsa_k256_keccak::SigningKey as EcdsaSecretKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey as FalconSecretKey;

use crate::manifest::Scheme;

/// A signer belonging to one run, discarded with it.
///
/// Keys are generated per run rather than derived from a fixed seed. Reusing a
/// signer across runs would let one run's leftover on-chain state satisfy
/// another run's assertions, which is the failure a qualification suite can
/// least afford: it reports coverage it did not earn.
pub enum RunSigner {
    Falcon(FalconSecretKey),
    Ecdsa(EcdsaSecretKey),
}

impl RunSigner {
    pub fn generate(scheme: Scheme) -> Option<Self> {
        match scheme {
            Scheme::Falcon => Some(Self::Falcon(FalconSecretKey::new())),
            Scheme::Ecdsa => Some(Self::Ecdsa(EcdsaSecretKey::new())),
            Scheme::Mixed | Scheme::NotApplicable => None,
        }
    }

    /// The key in the SDK's own wire form, whose leading byte names the scheme.
    /// Used to hand a cosigner to the other SDK.
    pub fn to_auth_secret_key(&self) -> miden_protocol::account::auth::AuthSecretKey {
        use miden_protocol::account::auth::AuthSecretKey;
        match self {
            Self::Falcon(key) => AuthSecretKey::Falcon512Poseidon2(key.clone()),
            Self::Ecdsa(key) => AuthSecretKey::EcdsaK256Keccak(key.clone()),
        }
    }

    pub fn scheme(&self) -> Scheme {
        match self {
            Self::Falcon(_) => Scheme::Falcon,
            Self::Ecdsa(_) => Scheme::Ecdsa,
        }
    }
}

/// The signer set a multisig shape needs, all freshly generated.
pub struct RunSigners {
    pub signers: Vec<RunSigner>,
    pub threshold: u32,
}

impl RunSigners {
    pub fn for_shape(threshold: u32, total: u32, scheme: Scheme) -> Option<Self> {
        if threshold == 0 || threshold > total {
            return None;
        }
        let signers: Vec<RunSigner> = (0..total)
            .map(|_| RunSigner::generate(scheme))
            .collect::<Option<Vec<_>>>()?;
        Some(Self { signers, threshold })
    }

    pub fn total(&self) -> u32 {
        self.signers.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Shape;

    #[test]
    fn a_shape_yields_that_many_signers() {
        for shape in [Shape::OneOfOne, Shape::TwoOfThree, Shape::ThreeOfThree] {
            let (threshold, total) = shape.threshold_and_total().expect("a real shape");
            let signers =
                RunSigners::for_shape(threshold, total, Scheme::Falcon).expect("generates");
            assert_eq!(signers.total(), total);
            assert_eq!(signers.threshold, threshold);
        }
    }

    #[test]
    fn a_mixed_scheme_cannot_be_generated() {
        assert!(RunSigner::generate(Scheme::Mixed).is_none());
        assert!(RunSigners::for_shape(2, 3, Scheme::Mixed).is_none());
    }

    #[test]
    fn a_threshold_above_the_signer_count_is_refused() {
        assert!(RunSigners::for_shape(4, 3, Scheme::Falcon).is_none());
        assert!(RunSigners::for_shape(0, 3, Scheme::Falcon).is_none());
    }

    #[test]
    fn signers_carry_the_requested_scheme() {
        let signers = RunSigners::for_shape(2, 3, Scheme::Ecdsa).expect("generates");
        assert!(signers.signers.iter().all(|s| s.scheme() == Scheme::Ecdsa));
    }
}
