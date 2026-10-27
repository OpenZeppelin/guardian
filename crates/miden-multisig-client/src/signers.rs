//! Approvers and the signature scheme the account verifies each one under.

use guardian_client::{
    AuthConfig, MidenAuth, MidenCosigner, MidenEcdsaAuth, MidenFalconRpoAuth, auth_config::AuthType,
};
use guardian_shared::SignatureScheme;
use miden_protocol::Word;

use crate::error::{MultisigError, Result};
use crate::transaction::word_to_hex;

/// An approver with the signature scheme the account verifies its signatures
/// under. The TypeScript SDK's `SignerSpec` carries the same pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignerSpec {
    pub commitment: Word,
    pub scheme: SignatureScheme,
}

impl SignerSpec {
    pub fn new(commitment: Word, scheme: SignatureScheme) -> Self {
        Self { commitment, scheme }
    }

    /// The commitments of `signers`, in order.
    pub fn commitments(signers: &[SignerSpec]) -> Vec<Word> {
        signers.iter().map(|signer| signer.commitment).collect()
    }

    /// The wire names of `schemes` (`"falcon"` / `"ecdsa"`).
    pub(crate) fn names(schemes: &[SignatureScheme]) -> Vec<String> {
        schemes
            .iter()
            .map(|scheme| scheme.as_str().to_string())
            .collect()
    }

    /// The `signer_schemes` an update-signers proposal carries for `targets`
    /// on an account whose approvers are `current`: none when every target
    /// signer uses the proposer's scheme, as a proposal from before per-signer
    /// schemes reads, and [`Self::resolve_targets`] reads the same targets
    /// back without names; one per target signer otherwise. Single-scheme
    /// proposals keep their wire shape.
    pub(crate) fn wire_schemes(
        targets: &[SignerSpec],
        proposer_scheme: SignatureScheme,
        current: &[SignerSpec],
    ) -> Vec<SignatureScheme> {
        let unnamed_is_exact = targets
            .iter()
            .all(|target| target.scheme == proposer_scheme)
            && Self::resolve_targets(&Self::commitments(targets), &[], current)
                .is_ok_and(|read| read == targets);
        if unnamed_is_exact {
            Vec::new()
        } else {
            targets.iter().map(|target| target.scheme).collect()
        }
    }

    /// The approver set an update-signers proposal writes, given its target
    /// `commitments`, the `schemes` it names for them and the account's
    /// `current` approvers.
    ///
    /// A proposal that names no schemes gives every target signer the scheme
    /// the account stores for the approvers it keeps (for all of them when it
    /// keeps none), which is what its proposer wrote whenever it rewrote no
    /// scheme. Either way a target signer the account already stores under
    /// another scheme is refused with [`MultisigError::SignerSchemeMismatch`].
    pub(crate) fn resolve_targets(
        commitments: &[Word],
        schemes: &[SignatureScheme],
        current: &[SignerSpec],
    ) -> Result<Vec<SignerSpec>> {
        let schemes = if schemes.is_empty() {
            vec![Self::kept_scheme(commitments, current)?; commitments.len()]
        } else if schemes.len() == commitments.len() {
            schemes.to_vec()
        } else {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal metadata names {} signer_schemes for {} signer_commitments",
                schemes.len(),
                commitments.len()
            )));
        };
        let targets: Vec<SignerSpec> = commitments
            .iter()
            .zip(schemes)
            .map(|(commitment, scheme)| SignerSpec::new(*commitment, scheme))
            .collect();
        Self::ensure_kept_schemes(&targets, current)?;
        Ok(targets)
    }

    fn kept_scheme(commitments: &[Word], current: &[SignerSpec]) -> Result<SignatureScheme> {
        current
            .iter()
            .find(|approver| commitments.contains(&approver.commitment))
            .or_else(|| current.first())
            .map(|approver| approver.scheme)
            .ok_or_else(|| {
                MultisigError::AccountStorage("the account has no approvers".to_string())
            })
    }

    fn ensure_kept_schemes(targets: &[SignerSpec], current: &[SignerSpec]) -> Result<()> {
        let changes: Vec<SignerSchemeChange> = targets
            .iter()
            .filter_map(|target| {
                current
                    .iter()
                    .find(|approver| approver.commitment == target.commitment)
                    .filter(|approver| approver.scheme != target.scheme)
                    .map(|approver| SignerSchemeChange {
                        commitment: target.commitment,
                        stored: approver.scheme,
                        proposed: target.scheme,
                    })
            })
            .collect();
        if changes.is_empty() {
            Ok(())
        } else {
            Err(MultisigError::SignerSchemeMismatch { changes })
        }
    }

    /// The auth GUARDIAN registers an account with these approvers and this
    /// GUARDIAN scheme under. An account whose approvers all use the GUARDIAN's
    /// scheme gets that scheme's single-scheme variant, which servers before
    /// per-signer schemes also accept; any other gets the per-signer `Miden`
    /// variant.
    pub(crate) fn guardian_auth_config(
        cosigners: &[SignerSpec],
        guardian_scheme: SignatureScheme,
    ) -> AuthConfig {
        let auth_type = if cosigners
            .iter()
            .all(|cosigner| cosigner.scheme == guardian_scheme)
        {
            let cosigner_commitments = cosigners
                .iter()
                .map(|cosigner| word_to_hex(&cosigner.commitment))
                .collect();
            match guardian_scheme {
                SignatureScheme::Falcon => AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                    cosigner_commitments,
                }),
                SignatureScheme::Ecdsa => AuthType::MidenEcdsa(MidenEcdsaAuth {
                    cosigner_commitments,
                }),
            }
        } else {
            AuthType::Miden(MidenAuth {
                cosigners: cosigners
                    .iter()
                    .map(|cosigner| MidenCosigner {
                        commitment: word_to_hex(&cosigner.commitment),
                        scheme: cosigner.scheme.as_str().to_string(),
                    })
                    .collect(),
                guardian_scheme: guardian_scheme.as_str().to_string(),
            })
        };
        AuthConfig {
            auth_type: Some(auth_type),
        }
    }
}

/// An approver an update-signers proposal would write under a scheme other
/// than the one the account stores for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignerSchemeChange {
    pub commitment: Word,
    pub stored: SignatureScheme,
    pub proposed: SignatureScheme,
}

impl SignerSchemeChange {
    pub(crate) fn list(changes: &[SignerSchemeChange]) -> String {
        changes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl std::fmt::Display for SignerSchemeChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (stored '{}', proposed '{}')",
            word_to_hex(&self.commitment),
            self.stored,
            self.proposed
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(v: u32) -> Word {
        Word::from([v, 0, 0, 0])
    }

    #[test]
    fn uniform_approvers_register_under_the_single_scheme_variant() {
        let falcon = SignerSpec::guardian_auth_config(
            &[
                SignerSpec::new(word(1), SignatureScheme::Falcon),
                SignerSpec::new(word(2), SignatureScheme::Falcon),
            ],
            SignatureScheme::Falcon,
        );
        assert_eq!(
            falcon.auth_type,
            Some(AuthType::MidenFalconRpo(MidenFalconRpoAuth {
                cosigner_commitments: vec![word_to_hex(&word(1)), word_to_hex(&word(2))],
            }))
        );

        let ecdsa = SignerSpec::guardian_auth_config(
            &[SignerSpec::new(word(3), SignatureScheme::Ecdsa)],
            SignatureScheme::Ecdsa,
        );
        assert_eq!(
            ecdsa.auth_type,
            Some(AuthType::MidenEcdsa(MidenEcdsaAuth {
                cosigner_commitments: vec![word_to_hex(&word(3))],
            }))
        );
    }

    #[test]
    fn mixed_approvers_register_under_the_per_signer_variant() {
        let config = SignerSpec::guardian_auth_config(
            &[
                SignerSpec::new(word(1), SignatureScheme::Falcon),
                SignerSpec::new(word(2), SignatureScheme::Ecdsa),
            ],
            SignatureScheme::Ecdsa,
        );
        assert_eq!(
            config.auth_type,
            Some(AuthType::Miden(MidenAuth {
                cosigners: vec![
                    MidenCosigner {
                        commitment: word_to_hex(&word(1)),
                        scheme: "falcon".to_string(),
                    },
                    MidenCosigner {
                        commitment: word_to_hex(&word(2)),
                        scheme: "ecdsa".to_string(),
                    },
                ],
                guardian_scheme: "ecdsa".to_string(),
            }))
        );
    }

    /// Approvers that share a scheme the GUARDIAN does not use still need the
    /// per-signer variant: the single-scheme variants name one scheme for both.
    #[test]
    fn approvers_of_another_scheme_than_the_guardian_register_per_signer() {
        let config = SignerSpec::guardian_auth_config(
            &[SignerSpec::new(word(1), SignatureScheme::Falcon)],
            SignatureScheme::Ecdsa,
        );
        assert!(matches!(config.auth_type, Some(AuthType::Miden(_))));
    }

    const F: SignatureScheme = SignatureScheme::Falcon;
    const E: SignatureScheme = SignatureScheme::Ecdsa;

    fn specs(entries: &[(u32, SignatureScheme)]) -> Vec<SignerSpec> {
        entries
            .iter()
            .map(|(v, scheme)| SignerSpec::new(word(*v), *scheme))
            .collect()
    }

    fn words(values: &[u32]) -> Vec<Word> {
        values.iter().map(|v| word(*v)).collect()
    }

    #[test]
    fn wire_schemes_are_omitted_only_when_every_target_uses_the_proposer_scheme() {
        let current = specs(&[(1, F), (2, F)]);
        assert!(
            SignerSpec::wire_schemes(&specs(&[(1, F), (2, F), (3, F)]), F, &current).is_empty()
        );
        assert_eq!(
            SignerSpec::wire_schemes(&specs(&[(1, F), (2, F), (3, E)]), F, &current),
            vec![F, F, E]
        );
        // A uniform target the proposer does not share still names its scheme:
        // without it a reader would assume the proposer's.
        let mixed = specs(&[(1, F), (2, E)]);
        assert_eq!(
            SignerSpec::wire_schemes(&specs(&[(1, F)]), E, &mixed),
            vec![F]
        );
    }

    /// A target that keeps no approver reads back without names under the
    /// first approver's scheme, so one of another scheme is named even when
    /// it is the proposer's.
    #[test]
    fn wire_schemes_name_a_replacement_the_reader_would_misread() {
        let current = specs(&[(1, F), (2, E)]);
        assert_eq!(
            SignerSpec::wire_schemes(&specs(&[(3, E), (4, E)]), E, &current),
            vec![E, E]
        );
    }

    /// Whatever the writer names or omits, the reader resolves exactly the
    /// targets written, for every target that keeps the stored schemes of
    /// the approvers it keeps.
    #[test]
    fn every_written_target_reads_back_exactly() {
        let accounts = [
            specs(&[(1, F)]),
            specs(&[(1, E)]),
            specs(&[(1, F), (2, E)]),
            specs(&[(1, E), (2, F)]),
            specs(&[(1, F), (2, E), (3, F)]),
            specs(&[(1, E), (2, E), (3, F)]),
        ];
        for current in &accounts {
            for kept_mask in 0u32..(1 << current.len()) {
                let kept: Vec<SignerSpec> = current
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| kept_mask & (1 << index) != 0)
                    .map(|(_, approver)| *approver)
                    .collect();
                for added in [None, Some(F), Some(E)] {
                    let mut targets = kept.clone();
                    targets.extend(added.map(|scheme| SignerSpec::new(word(9), scheme)));
                    if targets.is_empty() {
                        continue;
                    }
                    for proposer in [F, E] {
                        let names = SignerSpec::wire_schemes(&targets, proposer, current);
                        let read = SignerSpec::resolve_targets(
                            &SignerSpec::commitments(&targets),
                            &names,
                            current,
                        )
                        .unwrap();
                        assert_eq!(
                            read, targets,
                            "current {current:?}, proposer {proposer}, names {names:?}"
                        );
                    }
                }
            }
        }
    }

    /// A proposal from before per-signer schemes on a single-scheme account:
    /// every target signer, the added one too, takes the account's scheme.
    #[test]
    fn unnamed_schemes_take_the_scheme_of_the_kept_approvers() {
        let current = specs(&[(1, E), (2, E)]);
        assert_eq!(
            SignerSpec::resolve_targets(&words(&[1, 2, 3]), &[], &current).unwrap(),
            specs(&[(1, E), (2, E), (3, E)])
        );
    }

    /// Removing the only approver of the other scheme leaves a target that
    /// shares one scheme, which needs no names; every reader, whatever its
    /// own scheme, rebuilds it under the scheme the kept approvers store.
    #[test]
    fn unnamed_schemes_after_removing_the_other_scheme_keep_the_stored_one() {
        let current = specs(&[(1, F), (2, E), (3, F)]);
        assert_eq!(
            SignerSpec::resolve_targets(&words(&[1, 3]), &[], &current).unwrap(),
            specs(&[(1, F), (3, F)])
        );
    }

    /// An older ECDSA client removing the Falcon approver wrote the ECDSA one
    /// back under its own scheme, which is the one stored: no rewrite, so the
    /// proposal is accepted, read from the approver it keeps.
    #[test]
    fn unnamed_schemes_are_read_from_a_kept_approver_not_the_first_one() {
        let current = specs(&[(1, F), (2, E)]);
        assert_eq!(
            SignerSpec::resolve_targets(&words(&[2]), &[], &current).unwrap(),
            specs(&[(2, E)])
        );
    }

    /// Issue #549: an older client stamped its own scheme on every approver,
    /// so its proposal on a mixed account rewrites the others.
    #[test]
    fn unnamed_schemes_on_mixed_kept_approvers_are_refused() {
        let current = specs(&[(1, F), (2, E)]);
        let err = SignerSpec::resolve_targets(&words(&[1, 2, 3]), &[], &current).unwrap_err();
        let MultisigError::SignerSchemeMismatch { changes } = err else {
            panic!("expected a scheme mismatch, got {err}");
        };
        assert_eq!(
            changes,
            vec![SignerSchemeChange {
                commitment: word(2),
                stored: E,
                proposed: F,
            }]
        );
    }

    #[test]
    fn named_schemes_are_taken_as_given() {
        let current = specs(&[(1, F), (2, E)]);
        assert_eq!(
            SignerSpec::resolve_targets(&words(&[1, 2, 3]), &[F, E, E], &current).unwrap(),
            specs(&[(1, F), (2, E), (3, E)])
        );
    }

    #[test]
    fn named_schemes_that_rewrite_a_kept_approver_are_refused() {
        let current = specs(&[(1, F), (2, E)]);
        let err = SignerSpec::resolve_targets(&words(&[1, 2]), &[F, F], &current).unwrap_err();
        assert!(
            matches!(&err, MultisigError::SignerSchemeMismatch { changes } if changes.len() == 1),
            "got {err}"
        );
        assert!(
            err.to_string()
                .contains("stored 'ecdsa', proposed 'falcon'")
        );
    }

    #[test]
    fn named_schemes_must_align_with_the_commitments() {
        let current = specs(&[(1, F)]);
        let err = SignerSpec::resolve_targets(&words(&[1, 2]), &[F], &current).unwrap_err();
        assert!(matches!(err, MultisigError::InvalidConfig(_)), "got {err}");
    }
}
