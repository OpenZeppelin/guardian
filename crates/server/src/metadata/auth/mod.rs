use guardian_shared::SignatureScheme;
use guardian_shared::hex::FromHex;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::PublicKey as EcdsaPublicKey;
use miden_protocol::crypto::dsa::falcon512_poseidon2::PublicKey as FalconPublicKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use std::collections::HashSet;

use crate::api::grpc::guardian::auth_config;
use crate::error::GuardianError;
use crate::metadata::MetadataStore;

mod credentials;
pub mod lookup;
mod miden_ecdsa;
mod miden_falcon_rpo;

pub use credentials::{
    AuthHeader, Credentials, ExtractCredentials, MAX_TIMESTAMP_SKEW_MS, MAX_TIMESTAMP_SKEW_SECS,
    RequestAuthFormat,
};

/// Session-signed requests are resolved by `services::resolve_account`, and
/// wallet-only routes reject them earlier with `WalletSignatureRequired`.
/// This guard keeps `Auth::verify` from ever accepting a delegated signer.
pub(crate) const SESSION_NOT_ACCEPTED: &str =
    "Session credentials are not accepted here; sign this request with the wallet key";

pub(crate) use guardian_shared::hex::FALCON_SIGNATURE_LEN;
/// Serialized ECDSA secp256k1 signature length: `r (32) || s (32) || v (1)`.
pub(crate) const ECDSA_SIGNATURE_LEN: usize = 65;
/// Serialized (compressed) ECDSA secp256k1 public key length.
const ECDSA_PUBLIC_KEY_LEN: usize = 33;

/// One cosigner of a per-signer-scheme (`Miden`) auth: its public-key
/// commitment and the scheme the account verifies its signatures under.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct MidenCosigner {
    /// `0x` followed by 64 lowercase hex digits.
    pub commitment: String,
    pub scheme: SignatureScheme,
}

/// A cosigner as an account state stores it: the commitment from the
/// approver key map and the scheme from the approver scheme map, `None`
/// when that entry is missing or names no supported scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainCosigner {
    pub commitment: String,
    pub scheme: Option<SignatureScheme>,
}

/// Authentication and authorization handler
/// Defines which signature scheme to use and handles verification
/// Each variant contains auth-specific authorization data
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub enum Auth {
    /// Miden Falcon RPO signature scheme
    MidenFalconRpo { cosigner_commitments: Vec<String> },
    /// Miden ECDSA secp256k1 signature scheme
    MidenEcdsa { cosigner_commitments: Vec<String> },
    /// Miden cosigners that each carry their own signature scheme, and the
    /// GUARDIAN's scheme, set independently. Clients send it for an account
    /// whose cosigners and GUARDIAN do not all share one scheme; the two
    /// single-scheme variants keep serving the accounts that do.
    Miden {
        cosigners: Vec<MidenCosigner>,
        guardian_scheme: SignatureScheme,
    },
    /// EVM ECDSA account signer snapshot
    EvmEcdsa { signers: Vec<String> },
}

impl Auth {
    /// The scheme of the account's GUARDIAN key: the ACK identity this
    /// server signs the account's deltas with and the key its guardian slot
    /// must hold. EVM accounts have no GUARDIAN key; their signers are ECDSA.
    pub fn guardian_scheme(&self) -> SignatureScheme {
        match self {
            Auth::MidenFalconRpo { .. } => SignatureScheme::Falcon,
            Auth::MidenEcdsa { .. } | Auth::EvmEcdsa { .. } => SignatureScheme::Ecdsa,
            Auth::Miden {
                guardian_scheme, ..
            } => *guardian_scheme,
        }
    }

    /// The scheme a cosigner is registered under, or `None` when
    /// `commitment` is not a Miden cosigner of the account.
    ///
    /// EVM accounts have no Miden cosigners, so this is always `None` for
    /// them. Session verification never sees one, because
    /// `resolve_account_with` refuses an EVM account before it verifies a
    /// session, and the filesystem cosigner lookup relies on this `None` to
    /// leave EVM accounts out.
    pub fn signer_scheme(&self, commitment: &str) -> Option<SignatureScheme> {
        match self {
            Auth::MidenFalconRpo {
                cosigner_commitments,
            }
            | Auth::MidenEcdsa {
                cosigner_commitments,
            } => cosigner_commitments
                .iter()
                .any(|registered| registered == commitment)
                .then(|| self.guardian_scheme()),
            Auth::Miden { cosigners, .. } => cosigners
                .iter()
                .find(|cosigner| cosigner.commitment == commitment)
                .map(|cosigner| cosigner.scheme),
            Auth::EvmEcdsa { .. } => None,
        }
    }

    /// The Miden cosigners with their schemes, in declaration order. Empty
    /// for EVM accounts.
    pub fn cosigners(&self) -> Vec<MidenCosigner> {
        match self {
            Auth::MidenFalconRpo {
                cosigner_commitments,
            }
            | Auth::MidenEcdsa {
                cosigner_commitments,
            } => cosigner_commitments
                .iter()
                .map(|commitment| MidenCosigner {
                    commitment: commitment.clone(),
                    scheme: self.guardian_scheme(),
                })
                .collect(),
            Auth::Miden { cosigners, .. } => cosigners.clone(),
            Auth::EvmEcdsa { .. } => Vec::new(),
        }
    }

    /// Every scheme the account uses, its cosigners' and its GUARDIAN's,
    /// each once, in wire order (`falcon` before `ecdsa`).
    pub fn schemes_in_use(&self) -> Vec<SignatureScheme> {
        let guardian = self.guardian_scheme();
        let cosigners = self.cosigners();
        [SignatureScheme::Falcon, SignatureScheme::Ecdsa]
            .into_iter()
            .filter(|scheme| {
                *scheme == guardian || cosigners.iter().any(|cosigner| cosigner.scheme == *scheme)
            })
            .collect()
    }

    /// The scheme label reported as `auth_scheme` (`GET /state`, the
    /// dashboard): the one scheme the account uses, or `"mixed"` when its
    /// cosigners and GUARDIAN do not all share one.
    pub fn auth_scheme_label(&self) -> &'static str {
        match self.schemes_in_use().as_slice() {
            [scheme] => scheme.as_str(),
            _ => "mixed",
        }
    }

    /// Stable, dashboard-facing identifier for the auth method.
    /// Surfaced on `GET /dashboard/info` under `accounts_by_auth_method`.
    /// New variants MUST add a new label rather than reuse an existing
    /// one so clients can keep historical counts comparable.
    pub fn method_label(&self) -> &'static str {
        match self {
            Auth::MidenFalconRpo { .. } => "miden_falcon",
            Auth::MidenEcdsa { .. } => "miden_ecdsa",
            Auth::Miden { .. } => "miden",
            Auth::EvmEcdsa { .. } => "evm",
        }
    }

    pub fn compute_signer_commitment(&self, pubkey_hex: &str) -> Result<String, String> {
        match self {
            Auth::MidenFalconRpo { .. } => {
                let clean = pubkey_hex.trim_start_matches("0x");
                if clean.len() == 64 && hex::decode(clean).is_ok() {
                    return Ok(format!("0x{}", clean));
                }
                let public_key = FalconPublicKey::from_hex(pubkey_hex)
                    .map_err(|e| format!("invalid Falcon public key: {}", e))?;
                let commitment = public_key.to_commitment();
                Ok(format!("0x{}", hex::encode(commitment.to_bytes())))
            }
            Auth::MidenEcdsa { .. } => {
                let clean = pubkey_hex.trim_start_matches("0x");
                if clean.len() == 64 && hex::decode(clean).is_ok() {
                    return Ok(format!("0x{}", clean));
                }
                let public_key_bytes = hex::decode(clean)
                    .map_err(|e| format!("invalid ECDSA public key hex: {}", e))?;
                let public_key = EcdsaPublicKey::read_from_bytes(&public_key_bytes)
                    .map_err(|e| format!("invalid ECDSA public key: {}", e))?;
                let commitment = public_key.to_commitment();
                Ok(format!("0x{}", hex::encode(commitment.to_bytes())))
            }
            // The key's length names its scheme: a compressed ECDSA key is
            // 33 bytes, a Falcon key far longer. A commitment is accepted by
            // either single-scheme path.
            Auth::Miden { .. } => {
                let is_ecdsa_key = hex::decode(pubkey_hex.trim_start_matches("0x"))
                    .is_ok_and(|bytes| bytes.len() == ECDSA_PUBLIC_KEY_LEN);
                if is_ecdsa_key {
                    Self::single_scheme(SignatureScheme::Ecdsa, Vec::new())
                        .compute_signer_commitment(pubkey_hex)
                } else {
                    Self::single_scheme(SignatureScheme::Falcon, Vec::new())
                        .compute_signer_commitment(pubkey_hex)
                        .map_err(|e| {
                            format!(
                                "public key is neither a {ECDSA_PUBLIC_KEY_LEN}-byte ECDSA key \
                                 nor a Falcon key: {e}"
                            )
                        })
                }
            }
            Auth::EvmEcdsa { .. } => crate::metadata::network::normalize_evm_address(pubkey_hex),
        }
    }

    /// The authorized signer commitments, in declaration order (`signers`
    /// for EVM accounts).
    pub fn cosigner_commitments(&self) -> Vec<String> {
        match self {
            Auth::MidenFalconRpo {
                cosigner_commitments,
            }
            | Auth::MidenEcdsa {
                cosigner_commitments,
            } => cosigner_commitments.clone(),
            Auth::Miden { cosigners, .. } => cosigners
                .iter()
                .map(|cosigner| cosigner.commitment.clone())
                .collect(),
            Auth::EvmEcdsa { signers } => signers.clone(),
        }
    }

    fn signer_commitment_hex_length(&self) -> usize {
        match self {
            Auth::EvmEcdsa { .. } => 40,
            Auth::MidenFalconRpo { .. } | Auth::MidenEcdsa { .. } | Auth::Miden { .. } => 64,
        }
    }

    /// Returns whether a signer commitment uses the scheme's canonical
    /// lowercase, `0x`-prefixed representation.
    pub fn is_canonical_signer_commitment(&self, commitment: &str) -> bool {
        let expected_hex_length = self.signer_commitment_hex_length();
        let hex = commitment.strip_prefix("0x").unwrap_or("");
        hex.len() == expected_hex_length
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    /// Validates the exact-string signer set used by authorization and account
    /// state comparison. Non-canonical spellings cannot match reliably, an
    /// empty set cannot authorize a request, and duplicates make the declared
    /// signer set ambiguous.
    pub fn validate_canonical_signer_set(&self) -> Result<(), String> {
        let commitments = self.cosigner_commitments();
        if commitments.is_empty() {
            return Err("authorized signer set must not be empty".to_string());
        }

        let mut seen = HashSet::with_capacity(commitments.len());
        for commitment in &commitments {
            if !self.is_canonical_signer_commitment(commitment) {
                let expected_hex_length = self.signer_commitment_hex_length();
                return Err(format!(
                    "authorized signer entry '{commitment}' is not canonical \
                     (expected 0x followed by {expected_hex_length} lowercase hex digits)"
                ));
            }
            if !seen.insert(commitment) {
                return Err(format!(
                    "authorized signer set contains duplicate entry '{commitment}'"
                ));
            }
        }
        Ok(())
    }

    /// The auth that authorizes the cosigners an account state stores,
    /// keeping this auth's GUARDIAN scheme: the state's GUARDIAN binding is
    /// checked once, at `/configure`, and a scheme change re-onboards.
    ///
    /// A single-scheme auth stays single-scheme while every cosigner uses
    /// its scheme and becomes [`Auth::Miden`] once one does not, so the
    /// stored form of an account that never mixes schemes does not change.
    /// An [`Auth::Miden`] stays one. A cosigner whose scheme the state does
    /// not carry keeps the scheme this auth registers it under, or else the
    /// GUARDIAN's, which is the single-scheme behaviour; a wrong guess can
    /// only lock that cosigner out, since no key of another scheme has its
    /// commitment.
    pub fn with_chain_cosigners(&self, chain: Vec<ChainCosigner>) -> Self {
        if let Auth::EvmEcdsa { .. } = self {
            return Auth::EvmEcdsa {
                signers: chain
                    .into_iter()
                    .map(|cosigner| cosigner.commitment)
                    .collect(),
            };
        }
        let guardian_scheme = self.guardian_scheme();
        let cosigners: Vec<MidenCosigner> = chain
            .into_iter()
            .map(|cosigner| MidenCosigner {
                scheme: cosigner
                    .scheme
                    .or_else(|| self.signer_scheme(&cosigner.commitment))
                    .unwrap_or(guardian_scheme),
                commitment: cosigner.commitment,
            })
            .collect();
        let uniform = cosigners
            .iter()
            .all(|cosigner| cosigner.scheme == guardian_scheme);
        match self {
            Auth::MidenFalconRpo { .. } | Auth::MidenEcdsa { .. } if uniform => {
                Self::single_scheme(
                    guardian_scheme,
                    cosigners
                        .into_iter()
                        .map(|cosigner| cosigner.commitment)
                        .collect(),
                )
            }
            _ => Auth::Miden {
                cosigners,
                guardian_scheme,
            },
        }
    }

    fn single_scheme(scheme: SignatureScheme, cosigner_commitments: Vec<String>) -> Self {
        match scheme {
            SignatureScheme::Falcon => Auth::MidenFalconRpo {
                cosigner_commitments,
            },
            SignatureScheme::Ecdsa => Auth::MidenEcdsa {
                cosigner_commitments,
            },
        }
    }

    /// Verify credentials are authorized for account
    ///
    /// This verifies:
    /// 1. The signature is valid for the account_id + timestamp payload,
    ///    optionally bound to request bytes when provided by the transport layer
    /// 2. The signer's commitment is in the authorized list
    ///
    /// Returns the verified signer's public-key commitment (hex), which
    /// scopes replay-protection state per (account, signer).
    ///
    /// # Arguments
    /// * `account_id` - The account ID
    /// * `credentials` - The credentials to verify (includes timestamp)
    pub fn verify(&self, account_id: &str, credentials: &Credentials) -> Result<String, String> {
        if credentials.auth_format() == RequestAuthFormat::Session {
            return Err(SESSION_NOT_ACCEPTED.to_string());
        }
        match self {
            Auth::MidenFalconRpo {
                cosigner_commitments,
            } => {
                if credentials.auth_format() != RequestAuthFormat::Raw {
                    return Err(
                        "EIP-712 request authentication requires an ECDSA account".to_string()
                    );
                }
                let (_pubkey, signature, timestamp) =
                    credentials.as_signature().ok_or_else(|| {
                        tracing::warn!(
                            account_id = %account_id,
                            "MidenFalconRpo requires signature credentials but got different type"
                        );
                        "MidenFalconRpo requires signature credentials".to_string()
                    })?;

                miden_falcon_rpo::verify_request_signature(
                    account_id,
                    timestamp,
                    cosigner_commitments,
                    signature,
                    credentials.request_payload(),
                )
            }
            Auth::MidenEcdsa {
                cosigner_commitments,
            } => {
                let (pubkey, signature, timestamp) =
                    credentials.as_signature().ok_or_else(|| {
                        tracing::warn!(
                            account_id = %account_id,
                            "MidenEcdsa requires signature credentials but got different type"
                        );
                        "MidenEcdsa requires signature credentials".to_string()
                    })?;

                match credentials.auth_format() {
                    RequestAuthFormat::Raw => miden_ecdsa::verify_request_signature(
                        account_id,
                        timestamp,
                        cosigner_commitments,
                        signature,
                        pubkey,
                        credentials.request_payload(),
                    ),
                    RequestAuthFormat::Eip712 => miden_ecdsa::verify_eip712_request_signature(
                        account_id,
                        timestamp,
                        cosigner_commitments,
                        signature,
                        pubkey,
                        credentials.request_payload(),
                    ),
                    RequestAuthFormat::Session => Err(SESSION_NOT_ACCEPTED.to_string()),
                }
            }
            Auth::Miden { cosigners, .. } => {
                let (_pubkey, signature, _timestamp) =
                    credentials.as_signature().ok_or_else(|| {
                        tracing::warn!(
                            account_id = %account_id,
                            "Miden auth requires signature credentials but got different type"
                        );
                        "Miden auth requires signature credentials".to_string()
                    })?;
                // The signature's format names the signer's scheme, and only
                // the cosigners registered under that scheme are candidates,
                // verified exactly as a single-scheme account of that scheme
                // verifies them: a key authenticates only under the scheme
                // it is registered with.
                let scheme = match credentials.auth_format() {
                    RequestAuthFormat::Raw => raw_request_signature_scheme(signature)?,
                    RequestAuthFormat::Eip712 => SignatureScheme::Ecdsa,
                    RequestAuthFormat::Session => return Err(SESSION_NOT_ACCEPTED.to_string()),
                };
                let authorized = cosigners
                    .iter()
                    .filter(|cosigner| cosigner.scheme == scheme)
                    .map(|cosigner| cosigner.commitment.clone())
                    .collect();
                Self::single_scheme(scheme, authorized).verify(account_id, credentials)
            }
            Auth::EvmEcdsa { .. } => {
                let _ = credentials;
                Err("EVM request auth requires network-aware verification".to_string())
            }
        }
    }
}

/// The scheme of a raw request signature on an [`Auth::Miden`] account, by
/// its length: a Falcon signature (public key embedded) or an ECDSA
/// `r || s || v`. Any other length is rejected rather than guessed.
fn raw_request_signature_scheme(signature_hex: &str) -> Result<SignatureScheme, String> {
    let length = hex::decode(signature_hex.trim_start_matches("0x"))
        .map_err(|e| format!("Invalid signature hex: {e}"))?
        .len();
    match length {
        FALCON_SIGNATURE_LEN => Ok(SignatureScheme::Falcon),
        ECDSA_SIGNATURE_LEN => Ok(SignatureScheme::Ecdsa),
        other => Err(format!(
            "Request signature must be a {FALCON_SIGNATURE_LEN}-byte Falcon or \
             {ECDSA_SIGNATURE_LEN}-byte ECDSA signature, got {other} bytes"
        )),
    }
}

impl TryFrom<crate::api::grpc::guardian::AuthConfig> for Auth {
    type Error = String;

    fn try_from(auth_config: crate::api::grpc::guardian::AuthConfig) -> Result<Self, Self::Error> {
        match auth_config.auth_type {
            Some(auth_config::AuthType::MidenFalconRpo(miden_auth)) => Ok(Auth::MidenFalconRpo {
                cosigner_commitments: miden_auth.cosigner_commitments,
            }),
            Some(auth_config::AuthType::MidenEcdsa(miden_auth)) => Ok(Auth::MidenEcdsa {
                cosigner_commitments: miden_auth.cosigner_commitments,
            }),
            Some(auth_config::AuthType::Miden(miden_auth)) => Ok(Auth::Miden {
                cosigners: miden_auth
                    .cosigners
                    .into_iter()
                    .map(|cosigner| {
                        Ok(MidenCosigner {
                            scheme: SignatureScheme::from(&cosigner.scheme)?,
                            commitment: cosigner.commitment,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?,
                guardian_scheme: SignatureScheme::from(&miden_auth.guardian_scheme)?,
            }),
            Some(auth_config::AuthType::EvmEcdsa(evm_auth)) => Ok(Auth::EvmEcdsa {
                signers: evm_auth
                    .signers
                    .into_iter()
                    .map(|signer| crate::metadata::network::normalize_evm_address(&signer))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            None => {
                tracing::warn!("Auth type not specified in AuthConfig");
                Err("Auth type not specified".to_string())
            }
        }
    }
}

/// Verify the wallet signature on a session grant (`POST /session`) and return
/// the verified signer commitment. Falcon grants are raw-only; ECDSA grants are
/// raw or readable EIP-712 and need the public key.
pub fn verify_session_grant(
    scheme: SignatureScheme,
    format: RequestAuthFormat,
    grant: &guardian_shared::session_grant::SessionGrant,
    signature: &str,
    public_key: Option<&str>,
) -> Result<String, String> {
    match (scheme, format) {
        (SignatureScheme::Falcon, RequestAuthFormat::Raw) => {
            miden_falcon_rpo::verify_session_grant(grant, signature)
        }
        (SignatureScheme::Ecdsa, RequestAuthFormat::Raw | RequestAuthFormat::Eip712) => {
            miden_ecdsa::verify_session_grant(
                grant,
                signature,
                public_key,
                format == RequestAuthFormat::Eip712,
            )
        }
        (SignatureScheme::Falcon, RequestAuthFormat::Eip712) => {
            Err("EIP-712 session grants require an ECDSA wallet".to_string())
        }
        (_, RequestAuthFormat::Session) => {
            Err("A session grant must be signed by the wallet key".to_string())
        }
    }
}

pub async fn update_credentials(
    store: &dyn MetadataStore,
    account_id: &str,
    new_auth: Auth,
    now: &str,
) -> Result<(), GuardianError> {
    let mut metadata = store
        .get(account_id)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to get metadata: {e}")))?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

    if metadata.auth == new_auth {
        return Ok(());
    }

    metadata.auth = new_auth;
    metadata.updated_at = now.to_string();

    store
        .set(metadata)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to update metadata: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::grpc::guardian::{AuthConfig, MidenAuth, MidenCosigner as ProtoCosigner};
    use crate::testing::helpers::{TestEcdsaSigner, TestSigner};
    use guardian_shared::auth_request_eip712::request_digest;
    use guardian_shared::auth_request_message::AuthRequestMessage;
    use guardian_shared::auth_request_payload::AuthRequestPayload;

    const ACCOUNT_ID: &str = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";

    fn commitment(byte: &str) -> String {
        format!("0x{}", byte.repeat(32))
    }

    fn cosigner(commitment: &str, scheme: SignatureScheme) -> MidenCosigner {
        MidenCosigner {
            commitment: commitment.to_string(),
            scheme,
        }
    }

    fn chain(commitment: &str, scheme: Option<SignatureScheme>) -> ChainCosigner {
        ChainCosigner {
            commitment: commitment.to_string(),
            scheme,
        }
    }

    fn mixed(falcon: &str, ecdsa: &str, guardian_scheme: SignatureScheme) -> Auth {
        Auth::Miden {
            cosigners: vec![
                cosigner(falcon, SignatureScheme::Falcon),
                cosigner(ecdsa, SignatureScheme::Ecdsa),
            ],
            guardian_scheme,
        }
    }

    fn raw_falcon_credentials(signer: &TestSigner) -> Credentials {
        let timestamp = chrono::Utc::now().timestamp_millis();
        let (signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, timestamp);
        Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp)
    }

    fn raw_ecdsa_credentials(signer: &TestEcdsaSigner) -> Credentials {
        let timestamp = chrono::Utc::now().timestamp_millis();
        let (signature, _) = signer.sign_with_timestamp(ACCOUNT_ID, timestamp);
        Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp)
    }

    fn eip712_credentials(signer: &TestEcdsaSigner) -> Credentials {
        let timestamp = chrono::Utc::now().timestamp_millis();
        let request_hash = AuthRequestMessage::from_account_id_hex(
            ACCOUNT_ID,
            timestamp,
            AuthRequestPayload::empty(),
        )
        .expect("valid account id")
        .to_word();
        let signature = signer.sign_prehash(request_digest(request_hash));
        Credentials::signature(signer.pubkey_hex.clone(), signature, timestamp)
            .with_auth_format(RequestAuthFormat::Eip712)
    }

    #[test]
    fn single_scheme_variants_keep_their_wire_shape() {
        for (json, scheme) in [
            (
                r#"{"MidenFalconRpo":{"cosigner_commitments":["0xaa"]}}"#,
                SignatureScheme::Falcon,
            ),
            (
                r#"{"MidenEcdsa":{"cosigner_commitments":["0xaa"]}}"#,
                SignatureScheme::Ecdsa,
            ),
        ] {
            let auth: Auth = serde_json::from_str(json).expect("legacy auth parses");
            assert_eq!(serde_json::to_string(&auth).unwrap(), json);
            assert_eq!(auth.guardian_scheme(), scheme);
        }
    }

    #[test]
    fn miden_variant_wire_shape() {
        let auth = mixed(&commitment("aa"), &commitment("bb"), SignatureScheme::Ecdsa);
        let json = serde_json::to_value(&auth).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "Miden": {
                    "cosigners": [
                        { "commitment": commitment("aa"), "scheme": "falcon" },
                        { "commitment": commitment("bb"), "scheme": "ecdsa" }
                    ],
                    "guardian_scheme": "ecdsa"
                }
            })
        );
        assert_eq!(serde_json::from_value::<Auth>(json).unwrap(), auth);

        let unknown_scheme = serde_json::json!({
            "Miden": {
                "cosigners": [{ "commitment": commitment("aa"), "scheme": "rsa" }],
                "guardian_scheme": "falcon"
            }
        });
        assert!(serde_json::from_value::<Auth>(unknown_scheme).is_err());
        let missing_guardian_scheme = serde_json::json!({
            "Miden": { "cosigners": [{ "commitment": commitment("aa"), "scheme": "falcon" }] }
        });
        assert!(serde_json::from_value::<Auth>(missing_guardian_scheme).is_err());
    }

    #[test]
    fn grpc_miden_auth_parses_and_rejects_unknown_schemes() {
        let config = |scheme: &str, guardian_scheme: &str| AuthConfig {
            auth_type: Some(auth_config::AuthType::Miden(MidenAuth {
                cosigners: vec![
                    ProtoCosigner {
                        commitment: commitment("aa"),
                        scheme: "falcon".to_string(),
                    },
                    ProtoCosigner {
                        commitment: commitment("bb"),
                        scheme: scheme.to_string(),
                    },
                ],
                guardian_scheme: guardian_scheme.to_string(),
            })),
        };
        assert_eq!(
            Auth::try_from(config("ECDSA", "ecdsa")).unwrap(),
            mixed(&commitment("aa"), &commitment("bb"), SignatureScheme::Ecdsa)
        );
        assert!(
            Auth::try_from(config("rsa", "ecdsa"))
                .unwrap_err()
                .contains("unsupported signature scheme: rsa")
        );
        assert!(
            Auth::try_from(config("ecdsa", ""))
                .unwrap_err()
                .contains("unsupported signature scheme")
        );
    }

    #[test]
    fn scheme_accessors_per_variant() {
        let falcon = Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment("aa")],
        };
        assert_eq!(
            falcon.signer_scheme(&commitment("aa")),
            Some(SignatureScheme::Falcon)
        );
        assert_eq!(falcon.signer_scheme(&commitment("bb")), None);
        assert_eq!(falcon.schemes_in_use(), vec![SignatureScheme::Falcon]);
        assert_eq!(falcon.auth_scheme_label(), "falcon");
        assert_eq!(falcon.method_label(), "miden_falcon");

        let ecdsa = Auth::MidenEcdsa {
            cosigner_commitments: vec![commitment("aa")],
        };
        assert_eq!(ecdsa.auth_scheme_label(), "ecdsa");
        assert_eq!(
            ecdsa.cosigners(),
            vec![cosigner(&commitment("aa"), SignatureScheme::Ecdsa)]
        );

        let auth = mixed(&commitment("aa"), &commitment("bb"), SignatureScheme::Ecdsa);
        assert_eq!(auth.guardian_scheme(), SignatureScheme::Ecdsa);
        assert_eq!(
            auth.signer_scheme(&commitment("aa")),
            Some(SignatureScheme::Falcon)
        );
        assert_eq!(
            auth.signer_scheme(&commitment("bb")),
            Some(SignatureScheme::Ecdsa)
        );
        assert_eq!(auth.signer_scheme(&commitment("cc")), None);
        assert_eq!(
            auth.schemes_in_use(),
            vec![SignatureScheme::Falcon, SignatureScheme::Ecdsa]
        );
        assert_eq!(auth.auth_scheme_label(), "mixed");
        assert_eq!(auth.method_label(), "miden");
        assert_eq!(
            auth.cosigner_commitments(),
            vec![commitment("aa"), commitment("bb")]
        );

        // Falcon cosigners with an ECDSA GUARDIAN still mix schemes.
        let falcon_cosigners_ecdsa_guardian = Auth::Miden {
            cosigners: vec![cosigner(&commitment("aa"), SignatureScheme::Falcon)],
            guardian_scheme: SignatureScheme::Ecdsa,
        };
        assert_eq!(falcon_cosigners_ecdsa_guardian.auth_scheme_label(), "mixed");

        // A uniform set declared per signer reports its one scheme.
        let uniform = Auth::Miden {
            cosigners: vec![cosigner(&commitment("aa"), SignatureScheme::Ecdsa)],
            guardian_scheme: SignatureScheme::Ecdsa,
        };
        assert_eq!(uniform.auth_scheme_label(), "ecdsa");
        assert_eq!(uniform.schemes_in_use(), vec![SignatureScheme::Ecdsa]);

        let evm = Auth::EvmEcdsa {
            signers: vec!["0x1111111111111111111111111111111111111111".to_string()],
        };
        assert!(evm.cosigners().is_empty());
        assert_eq!(
            evm.signer_scheme("0x1111111111111111111111111111111111111111"),
            None
        );
    }

    #[test]
    fn miden_signer_set_validation() {
        assert!(
            Auth::Miden {
                cosigners: vec![],
                guardian_scheme: SignatureScheme::Falcon
            }
            .validate_canonical_signer_set()
            .unwrap_err()
            .contains("must not be empty")
        );
        assert!(
            Auth::Miden {
                cosigners: vec![cosigner("0xAA", SignatureScheme::Falcon)],
                guardian_scheme: SignatureScheme::Falcon
            }
            .validate_canonical_signer_set()
            .unwrap_err()
            .contains("is not canonical")
        );
        // One key cannot be registered under two schemes.
        assert!(
            Auth::Miden {
                cosigners: vec![
                    cosigner(&commitment("aa"), SignatureScheme::Falcon),
                    cosigner(&commitment("aa"), SignatureScheme::Ecdsa),
                ],
                guardian_scheme: SignatureScheme::Falcon
            }
            .validate_canonical_signer_set()
            .unwrap_err()
            .contains("duplicate entry")
        );
        assert!(
            mixed(
                &commitment("aa"),
                &commitment("bb"),
                SignatureScheme::Falcon
            )
            .validate_canonical_signer_set()
            .is_ok()
        );
    }

    #[test]
    fn miden_signer_commitment_from_either_key_or_a_commitment() {
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        let auth = mixed(
            &falcon.commitment_hex,
            &ecdsa.commitment_hex,
            SignatureScheme::Falcon,
        );
        assert_eq!(
            auth.compute_signer_commitment(&falcon.pubkey_hex).unwrap(),
            falcon.commitment_hex
        );
        assert_eq!(
            auth.compute_signer_commitment(&ecdsa.pubkey_hex).unwrap(),
            ecdsa.commitment_hex
        );
        assert_eq!(
            auth.compute_signer_commitment(&ecdsa.commitment_hex)
                .unwrap(),
            ecdsa.commitment_hex
        );
        assert!(auth.compute_signer_commitment("0xzz").is_err());
        assert!(auth.compute_signer_commitment("0x0102").is_err());
    }

    #[test]
    fn miden_verifies_each_signer_under_its_own_scheme() {
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        let auth = mixed(
            &falcon.commitment_hex,
            &ecdsa.commitment_hex,
            SignatureScheme::Falcon,
        );

        assert_eq!(
            auth.verify(ACCOUNT_ID, &raw_falcon_credentials(&falcon))
                .unwrap(),
            falcon.commitment_hex
        );
        assert_eq!(
            auth.verify(ACCOUNT_ID, &raw_ecdsa_credentials(&ecdsa))
                .unwrap(),
            ecdsa.commitment_hex
        );
        assert_eq!(
            auth.verify(ACCOUNT_ID, &eip712_credentials(&ecdsa))
                .unwrap(),
            ecdsa.commitment_hex
        );
    }

    #[test]
    fn miden_rejects_a_key_presented_under_another_scheme() {
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        // Each key registered under the scheme it does not use.
        let swapped = Auth::Miden {
            cosigners: vec![
                cosigner(&falcon.commitment_hex, SignatureScheme::Ecdsa),
                cosigner(&ecdsa.commitment_hex, SignatureScheme::Falcon),
            ],
            guardian_scheme: SignatureScheme::Falcon,
        };
        for credentials in [
            raw_falcon_credentials(&falcon),
            raw_ecdsa_credentials(&ecdsa),
            eip712_credentials(&ecdsa),
        ] {
            let error = swapped
                .verify(ACCOUNT_ID, &credentials)
                .expect_err("a key must authenticate only under its registered scheme");
            assert!(error.contains("not authorized"), "{error}");
        }

        // An unregistered key of either scheme is refused too.
        let auth = mixed(
            &falcon.commitment_hex,
            &ecdsa.commitment_hex,
            SignatureScheme::Falcon,
        );
        assert!(
            auth.verify(ACCOUNT_ID, &raw_falcon_credentials(&TestSigner::new()))
                .is_err()
        );
        assert!(
            auth.verify(ACCOUNT_ID, &raw_ecdsa_credentials(&TestEcdsaSigner::new()))
                .is_err()
        );
    }

    #[test]
    fn miden_rejects_malformed_and_session_credentials() {
        let falcon = TestSigner::new();
        let ecdsa = TestEcdsaSigner::new();
        let auth = mixed(
            &falcon.commitment_hex,
            &ecdsa.commitment_hex,
            SignatureScheme::Falcon,
        );
        let timestamp = chrono::Utc::now().timestamp_millis();

        let error = auth
            .verify(
                ACCOUNT_ID,
                &Credentials::signature(
                    ecdsa.pubkey_hex.clone(),
                    format!("0x{}", "11".repeat(64)),
                    timestamp,
                ),
            )
            .unwrap_err();
        assert!(error.contains("got 64 bytes"), "{error}");
        assert!(
            auth.verify(
                ACCOUNT_ID,
                &Credentials::signature(
                    ecdsa.pubkey_hex.clone(),
                    "0xnothex".to_string(),
                    timestamp
                )
            )
            .unwrap_err()
            .contains("Invalid signature hex")
        );

        // A Falcon signature sent as EIP-712 takes the ECDSA path and fails.
        let falcon_as_eip712 =
            raw_falcon_credentials(&falcon).with_auth_format(RequestAuthFormat::Eip712);
        assert!(auth.verify(ACCOUNT_ID, &falcon_as_eip712).is_err());

        let session = raw_ecdsa_credentials(&ecdsa).with_auth_format(RequestAuthFormat::Session);
        assert_eq!(
            auth.verify(ACCOUNT_ID, &session).unwrap_err(),
            SESSION_NOT_ACCEPTED
        );
    }

    #[test]
    fn single_scheme_auth_stays_single_scheme_while_its_cosigners_share_the_scheme() {
        let falcon = Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment("aa")],
        };
        assert_eq!(
            falcon.with_chain_cosigners(vec![
                chain(&commitment("aa"), Some(SignatureScheme::Falcon)),
                chain(&commitment("bb"), Some(SignatureScheme::Falcon)),
            ]),
            Auth::MidenFalconRpo {
                cosigner_commitments: vec![commitment("aa"), commitment("bb")],
            }
        );
        // A state that carries no schemes keeps the single-scheme behaviour.
        let ecdsa = Auth::MidenEcdsa {
            cosigner_commitments: vec![commitment("aa")],
        };
        assert_eq!(
            ecdsa.with_chain_cosigners(vec![
                chain(&commitment("aa"), None),
                chain(&commitment("bb"), None),
            ]),
            Auth::MidenEcdsa {
                cosigner_commitments: vec![commitment("aa"), commitment("bb")],
            }
        );
    }

    #[test]
    fn single_scheme_auth_is_promoted_once_a_cosigner_uses_another_scheme() {
        let ecdsa = Auth::MidenEcdsa {
            cosigner_commitments: vec![commitment("aa")],
        };
        assert_eq!(
            ecdsa.with_chain_cosigners(vec![
                chain(&commitment("aa"), Some(SignatureScheme::Ecdsa)),
                chain(&commitment("bb"), Some(SignatureScheme::Falcon)),
            ]),
            Auth::Miden {
                cosigners: vec![
                    cosigner(&commitment("aa"), SignatureScheme::Ecdsa),
                    cosigner(&commitment("bb"), SignatureScheme::Falcon),
                ],
                guardian_scheme: SignatureScheme::Ecdsa,
            }
        );
        // Every cosigner re-registered under the other scheme: the GUARDIAN
        // keeps the account's scheme, so the set no longer matches it.
        let falcon = Auth::MidenFalconRpo {
            cosigner_commitments: vec![commitment("aa")],
        };
        assert_eq!(
            falcon
                .with_chain_cosigners(vec![chain(&commitment("aa"), Some(SignatureScheme::Ecdsa))]),
            Auth::Miden {
                cosigners: vec![cosigner(&commitment("aa"), SignatureScheme::Ecdsa)],
                guardian_scheme: SignatureScheme::Falcon,
            }
        );
    }

    #[test]
    fn miden_auth_stays_per_signer_and_keeps_its_guardian_scheme() {
        let auth = mixed(&commitment("aa"), &commitment("bb"), SignatureScheme::Ecdsa);
        // Uniform on chain, still per signer.
        assert_eq!(
            auth.with_chain_cosigners(vec![chain(&commitment("bb"), Some(SignatureScheme::Ecdsa))]),
            Auth::Miden {
                cosigners: vec![cosigner(&commitment("bb"), SignatureScheme::Ecdsa)],
                guardian_scheme: SignatureScheme::Ecdsa,
            }
        );
        // A missing scheme keeps the registered one, else the GUARDIAN's.
        assert_eq!(
            auth.with_chain_cosigners(vec![
                chain(&commitment("aa"), None),
                chain(&commitment("cc"), None),
            ]),
            Auth::Miden {
                cosigners: vec![
                    cosigner(&commitment("aa"), SignatureScheme::Falcon),
                    cosigner(&commitment("cc"), SignatureScheme::Ecdsa),
                ],
                guardian_scheme: SignatureScheme::Ecdsa,
            }
        );
    }

    #[test]
    fn falcon_rejects_eip712_request_auth() {
        let auth = Auth::MidenFalconRpo {
            cosigner_commitments: vec![],
        };
        let credentials = Credentials::signature(String::new(), String::new(), 0)
            .with_auth_format(RequestAuthFormat::Eip712);

        assert!(auth.verify("", &credentials).is_err());
    }
}
