//! MultisigGuardian Account Builder
//!
//! Builds accounts from the upstream `AuthGuardedMultisig` component and a
//! `BasicWallet`. Guardian rotation requires the procedure's multisig threshold,
//! without a current-guardian signature.

use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use miden_protocol::Word;
use miden_protocol::account::auth::{AuthScheme, PublicKeyCommitment};
use miden_protocol::account::{
    Account, AccountBuilder, AccountComponent, AccountComponentCode, AccountProcedureRoot,
    AccountType,
};
use miden_standards::account::auth::{
    Approver, ApproverSet, AuthGuardedMultisig, AuthGuardedMultisigConfig, GuardianConfig,
};
use miden_standards::account::wallets::BasicWallet;
use miden_standards::code_builder::CodeBuilder;

use guardian_shared::SignatureScheme;

/// The guarded-multisig auth MASM, byte-identical to the upstream component source that
/// [`AuthGuardedMultisig::code()`] is compiled from, and shared verbatim with the TypeScript
/// package that assembles browser accounts from it.
///
/// The source is the same; the *linkage* is not, and that is why this constant exists rather
/// than a plain `AuthGuardedMultisig::code()` call. `auth_tx_guarded_multisig` is the only
/// export that calls `miden::standards::fee`, so it is the only one whose root moves with how
/// the standards package is linked: upstream's component manifest declares
/// `miden-standards = { linkage = "static" }` and inlines the callee's MAST, while
/// [`CodeBuilder`] links it dynamically and leaves an external reference. Building through
/// [`CodeBuilder`] here is what makes a Rust-built account carry the same `auth_tx` root as a
/// browser-built one — the root `miden_multisig_client::ProcedureName::AuthTx` pins, and the
/// key the per-procedure threshold overrides map is keyed by.
/// A crate-local copy of the TypeScript package's MASM. It cannot be an `include_str!` reaching
/// into `packages/`: `cargo publish` packages only files under the crate root, so an escaping
/// path builds in the workspace and then fails to compile from the published tarball.
/// `vendored_masm_matches_the_typescript_package` keeps the two byte-identical.
pub const GUARDED_MULTISIG_AUTH_MASM: &str =
    include_str!("../masm/account_components/auth/guarded_multisig.masm");

/// Assembled once, like upstream's own `AuthGuardedMultisig::code()`: the source is a constant,
/// so the result is too, and account builds are not rare (the load generator makes thousands).
static DYNAMICALLY_LINKED_AUTH_CODE: LazyLock<std::result::Result<AccountComponentCode, String>> =
    LazyLock::new(|| {
        CodeBuilder::new()
            .compile_component_code("guarded_multisig", GUARDED_MULTISIG_AUTH_MASM)
            .map_err(|e| format!("failed to compile the guarded-multisig auth MASM: {e}"))
    });

/// The guarded-multisig auth component code every guardian account carries: the shared MASM
/// compiled with the standards package linked dynamically.
///
/// This is the code both SDKs build accounts from, so it is the code the pinned procedure roots
/// describe. See [`GUARDED_MULTISIG_AUTH_MASM`] for why the linkage is load-bearing.
pub fn dynamically_linked_auth_code() -> Result<AccountComponentCode> {
    DYNAMICALLY_LINKED_AUTH_CODE
        .as_ref()
        .map(Clone::clone)
        .map_err(|e| anyhow!(e.clone()))
}

/// Configuration for creating a MultisigGuardian account.
#[derive(Debug, Clone)]
pub struct MultisigGuardianConfig {
    /// The minimum number of signatures required to authorize a transaction.
    pub threshold: u32,
    /// Public key commitments of all signers (as Words).
    pub signer_commitments: Vec<Word>,
    /// GUARDIAN public key commitment.
    pub guardian_commitment: Word,
    /// Signature scheme for the account (Falcon or ECDSA).
    pub signature_scheme: SignatureScheme,
    /// Account type, which also determines on-chain storage visibility
    /// (`Private` keeps state off-chain; defaults to `Private`).
    pub account_type: AccountType,
    /// Procedure-specific threshold overrides (procedure root -> threshold).
    /// An override for guardian rotation is its sole authorization quorum.
    pub proc_threshold_overrides: Vec<(Word, u32)>,
}

impl MultisigGuardianConfig {
    /// Creates a new MultisigGuardian configuration.
    pub fn new(threshold: u32, signer_commitments: Vec<Word>, guardian_commitment: Word) -> Self {
        Self {
            threshold,
            signer_commitments,
            guardian_commitment,
            signature_scheme: SignatureScheme::Falcon,
            account_type: AccountType::Private,
            proc_threshold_overrides: Vec::new(),
        }
    }

    /// Sets the signature scheme for the account.
    pub fn with_signature_scheme(mut self, signature_scheme: SignatureScheme) -> Self {
        self.signature_scheme = signature_scheme;
        self
    }

    /// Sets the account type (also controls on-chain storage visibility).
    pub fn with_account_type(mut self, account_type: AccountType) -> Self {
        self.account_type = account_type;
        self
    }

    /// Adds procedure-specific threshold overrides.
    pub fn with_proc_threshold_overrides(mut self, overrides: Vec<(Word, u32)>) -> Self {
        self.proc_threshold_overrides = overrides;
        self
    }
}

/// Builder for creating MultisigGuardian accounts from the upstream
/// `AuthGuardedMultisig` component plus a `BasicWallet`.
///
/// # Example
/// ```ignore
/// use miden_confidential_contracts::multisig_guardian::{MultisigGuardianConfig, MultisigGuardianBuilder};
///
/// let config = MultisigGuardianConfig::new(2, vec![pk1, pk2], guardian_pk);
/// let account = MultisigGuardianBuilder::new(config).with_seed([0u8; 32]).build()?;
/// ```
pub struct MultisigGuardianBuilder {
    config: MultisigGuardianConfig,
    seed: [u8; 32],
}

impl MultisigGuardianBuilder {
    /// Creates a new MultisigGuardian builder with the given configuration.
    pub fn new(config: MultisigGuardianConfig) -> Self {
        Self {
            config,
            seed: [0u8; 32],
        }
    }

    /// Sets the seed used for account ID derivation.
    pub fn with_seed(mut self, seed: [u8; 32]) -> Self {
        self.seed = seed;
        self
    }

    /// Sets the account type (also controls on-chain storage visibility).
    pub fn with_account_type(mut self, account_type: AccountType) -> Self {
        self.config.account_type = account_type;
        self
    }

    /// Builds the MultisigGuardian account (fresh, undeployed).
    pub fn build(self) -> Result<Account> {
        let (seed, account_type, component) = self.into_parts()?;
        AccountBuilder::new(seed)
            .with_component(component)
            .with_component(BasicWallet)
            .account_type(account_type)
            .build()
            .map_err(|e| anyhow!("failed to build account: {e}"))
    }

    /// Builds the account using `build_existing()` (for testing with pre-set account state).
    #[cfg(feature = "testing")]
    pub fn build_existing(self) -> Result<Account> {
        let (seed, account_type, component) = self.into_parts()?;
        AccountBuilder::new(seed)
            .with_component(component)
            .with_component(BasicWallet)
            .account_type(account_type)
            .build_existing()
            .map_err(|e| anyhow!("failed to build existing account: {e}"))
    }

    /// Validates the config and assembles the upstream guarded-multisig component.
    fn into_parts(self) -> Result<([u8; 32], AccountType, AccountComponent)> {
        self.validate_config()?;
        let component = self.build_guarded_multisig_component()?;
        Ok((self.seed, self.config.account_type, component))
    }

    fn auth_scheme(&self) -> AuthScheme {
        match self.config.signature_scheme {
            SignatureScheme::Falcon => AuthScheme::Falcon512Poseidon2,
            SignatureScheme::Ecdsa => AuthScheme::EcdsaK256Keccak,
        }
    }

    fn build_guarded_multisig_component(&self) -> Result<AccountComponent> {
        let scheme = self.auth_scheme();

        let approvers: Vec<Approver> = self
            .config
            .signer_commitments
            .iter()
            .map(|commitment| Approver::new(PublicKeyCommitment::from(*commitment), scheme))
            .collect();

        let approver_set = ApproverSet::new(approvers, self.config.threshold)
            .map_err(|e| anyhow!("invalid approver set: {e}"))?;

        let guardian = GuardianConfig::new(Approver::new(
            PublicKeyCommitment::from(self.config.guardian_commitment),
            scheme,
        ));

        let mut cfg = AuthGuardedMultisigConfig::new(approver_set, guardian)
            .map_err(|e| anyhow!("invalid guarded-multisig config: {e}"))?;

        if !self.config.proc_threshold_overrides.is_empty() {
            let overrides: Vec<(AccountProcedureRoot, u32)> = self
                .config
                .proc_threshold_overrides
                .iter()
                .map(|(root, threshold)| (AccountProcedureRoot::from_raw(*root), *threshold))
                .collect();
            cfg = cfg
                .with_proc_thresholds(overrides)
                .map_err(|e| anyhow!("invalid procedure thresholds: {e}"))?;
        }

        let component: AccountComponent = AuthGuardedMultisig::new(cfg)
            .map_err(|e| anyhow!("failed to build guarded-multisig component: {e}"))?
            .into();

        // Same storage and metadata as the upstream component; only the code is swapped, for
        // the dynamically linked build of the identical source. See
        // [`GUARDED_MULTISIG_AUTH_MASM`].
        AccountComponent::new(
            dynamically_linked_auth_code()?,
            component.storage_slots().to_vec(),
            component.metadata().clone(),
        )
        .map_err(|e| anyhow!("failed to build guarded-multisig component: {e}"))
    }

    fn validate_config(&self) -> Result<()> {
        if self.config.threshold == 0 {
            return Err(anyhow!("threshold must be greater than 0"));
        }
        if self.config.signer_commitments.is_empty() {
            return Err(anyhow!("at least one signer commitment is required"));
        }

        let mut seen_signers = std::collections::HashSet::new();
        for commitment in &self.config.signer_commitments {
            if !seen_signers.insert(*commitment) {
                return Err(anyhow!(
                    "duplicate signer commitment: {}",
                    guardian_shared::hex::IntoHex::into_hex(*commitment)
                ));
            }
        }

        if self.config.threshold > self.config.signer_commitments.len() as u32 {
            return Err(anyhow!(
                "threshold ({}) cannot exceed number of signers ({})",
                self.config.threshold,
                self.config.signer_commitments.len()
            ));
        }

        if seen_signers.contains(&self.config.guardian_commitment) {
            return Err(anyhow!(
                "GUARDIAN commitment must be different from all signer commitments"
            ));
        }

        let mut seen_procs = std::collections::HashSet::new();
        for (root, threshold) in &self.config.proc_threshold_overrides {
            if *threshold < 1 {
                return Err(anyhow!("procedure threshold must be at least 1"));
            }
            if *threshold > self.config.signer_commitments.len() as u32 {
                return Err(anyhow!(
                    "procedure threshold ({}) cannot exceed number of signers ({})",
                    threshold,
                    self.config.signer_commitments.len()
                ));
            }
            if !seen_procs.insert(*root) {
                return Err(anyhow!("duplicate procedure threshold override"));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guardian_shared::hex::{FromHex, IntoHex};

    fn mock_commitment(seed: u8) -> Word {
        Word::from([
            seed as u32,
            seed as u32 + 1,
            seed as u32 + 2,
            seed as u32 + 3,
        ])
    }

    #[test]
    fn test_config_creation() {
        let config = MultisigGuardianConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2), mock_commitment(3)],
            mock_commitment(10),
        );

        assert_eq!(config.threshold, 2);
        assert_eq!(config.signer_commitments.len(), 3);
        assert!(config.proc_threshold_overrides.is_empty());
    }

    #[test]
    fn test_validation_zero_threshold() {
        let config = MultisigGuardianConfig::new(0, vec![mock_commitment(1)], mock_commitment(10));
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("threshold must be greater than 0")
        );
    }

    #[test]
    fn test_validation_empty_signers() {
        let config = MultisigGuardianConfig::new(1, vec![], mock_commitment(10));
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("at least one signer commitment")
        );
    }

    #[test]
    fn test_validation_threshold_exceeds_signers() {
        let config = MultisigGuardianConfig::new(
            3,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        );
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot exceed"));
    }

    #[test]
    fn test_validation_duplicate_signers() {
        let config = MultisigGuardianConfig::new(
            1,
            vec![mock_commitment(1), mock_commitment(1)],
            mock_commitment(10),
        );
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("duplicate signer commitment")
        );
    }

    #[test]
    fn test_validation_guardian_collision() {
        let config = MultisigGuardianConfig::new(1, vec![mock_commitment(1)], mock_commitment(1));
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("GUARDIAN commitment must be different")
        );
    }

    #[test]
    fn test_validation_invalid_proc_override() {
        let config = MultisigGuardianConfig::new(1, vec![mock_commitment(1)], mock_commitment(10))
            .with_proc_threshold_overrides(vec![(mock_commitment(20), 0)]);
        let result = MultisigGuardianBuilder::new(config).build();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("must be at least 1")
        );
    }

    #[test]
    fn test_build_account() {
        let config = MultisigGuardianConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        );
        let account = MultisigGuardianBuilder::new(config)
            .with_seed([42u8; 32])
            .build();
        assert!(account.is_ok());
    }

    #[test]
    fn test_auth_procedure_is_first_in_account_code() {
        let config = MultisigGuardianConfig::new(
            2,
            vec![mock_commitment(1), mock_commitment(2)],
            mock_commitment(10),
        );

        let component = MultisigGuardianBuilder::new(config.clone())
            .build_guarded_multisig_component()
            .expect("component");
        let auth_procedures = component
            .procedures()
            .filter_map(|(root, is_auth)| is_auth.then_some(root))
            .collect::<Vec<_>>();
        assert_eq!(auth_procedures.len(), 1);
        let auth_root = auth_procedures[0];

        let account = MultisigGuardianBuilder::new(config)
            .build_existing()
            .expect("account");
        assert_eq!(account.code().procedures()[0], auth_root);
    }

    #[test]
    fn test_browser_deterministic_account_matches_rust_builder() {
        let signer_commitment =
            Word::from_hex("0x260a375ca01f1f05cd7bf22298b40c47290fc09f209011d39049b7f2ef61387b")
                .expect("signer commitment");
        let guardian_commitment =
            Word::from_hex("0xc35d79423c41d46b5289aafef48be2364e9ea494c6b14d6aefad10f1a46e6d7c")
                .expect("guardian commitment");

        let config = MultisigGuardianConfig::new(1, vec![signer_commitment], guardian_commitment);
        let account = MultisigGuardianBuilder::new(config)
            .with_seed([9u8; 32])
            .build()
            .expect("account");

        // Cross-SDK parity: the TypeScript builder must derive these same identity
        // values from the same pinned miden-standards version; regenerate both if
        // the pin changes.
        assert_eq!(account.id().to_hex(), "0x912a25b8dccd678172ade0b9ac8f40");
        assert_eq!(
            account.to_commitment().into_hex(),
            "0x2f42130aaa99dbfba7e4786c44a1cd55983fc03b44a47b6e8385dcb36f246289"
        );
    }
}

#[cfg(test)]
mod vendored_masm_tests {
    use super::GUARDED_MULTISIG_AUTH_MASM;

    /// The TypeScript package owns the canonical MASM (`generate-masm.mjs` vendors it into
    /// `auth.ts`); this crate must carry its own copy to stay publishable. Skipped when the
    /// sibling tree is absent, which is the published-tarball case.
    #[test]
    fn vendored_masm_matches_the_typescript_package() {
        let canonical = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/miden-multisig-client/masm/account_components/auth/guarded_multisig.masm");
        if !canonical.exists() {
            return;
        }
        let expected = std::fs::read_to_string(&canonical).expect("read canonical MASM");
        assert_eq!(
            expected, GUARDED_MULTISIG_AUTH_MASM,
            "crates/contracts/masm copy has drifted from {}",
            canonical.display()
        );
    }
}
