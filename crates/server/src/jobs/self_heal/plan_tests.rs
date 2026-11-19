//! Tests of the self-heal planner on real Miden accounts, deltas and
//! signatures.

use guardian_shared::{EcdsaMessageFormat, FromJson, SignatureScheme, ToJson};
use miden_protocol::account::delta::AccountVaultDelta;
use miden_protocol::account::{
    Account, AccountCodePatch, AccountDelta, AccountStoragePatch, StorageMapKey,
};
use miden_protocol::transaction::{
    InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
};
use miden_protocol::{Felt, Word, ZERO};
use miden_standards::account::auth::AuthGuardedMultisig;
use serde_json::{Value, json};

use crate::delta_object::{
    CosignerSignature, DeltaObject, DeltaStatus, ProposalSignature, RetainReason,
};
use crate::metadata::auth::{Auth, MidenCosigner};
use crate::metadata::{AccountMetadata, NetworkConfig};
use crate::network::miden::MidenNetworkClient;
use crate::network::{AppliedState, NetworkClient, NetworkType};
use crate::state_object::StateObject;
use crate::storage::{HealDeltaWrite, HealProposalWrite, ProposalRecord, SupersededDelta};
use crate::testing::helpers::{
    TestEcdsaSigner, TestSigner, build_guarded_multisig_account, word_from_commitment_hex,
};

use super::plan::plan_account;
use super::types::{
    AccountInputs, AccountPlan, ChainRead, HealPlan, RecoveryReason, RowKind, RowVerdict,
    UnrecoveredAccount,
};

const NOW: &str = "2026-11-19T00:00:00Z";
const EARLIER: &str = "2026-01-01T00:00:00Z";

enum Key {
    Falcon(TestSigner),
    Ecdsa(TestEcdsaSigner),
}

impl Key {
    fn new(scheme: SignatureScheme) -> Self {
        match scheme {
            SignatureScheme::Falcon => Self::Falcon(TestSigner::new()),
            SignatureScheme::Ecdsa => Self::Ecdsa(TestEcdsaSigner::new()),
        }
    }

    fn commitment(&self) -> String {
        match self {
            Self::Falcon(signer) => signer.commitment_hex.clone(),
            Self::Ecdsa(signer) => signer.commitment_hex.clone(),
        }
    }

    fn scheme(&self) -> SignatureScheme {
        match self {
            Self::Falcon(_) => SignatureScheme::Falcon,
            Self::Ecdsa(_) => SignatureScheme::Ecdsa,
        }
    }

    fn word(&self) -> Word {
        word_from_commitment_hex(&self.commitment())
    }

    fn sign(&self, payload: &Value) -> CosignerSignature {
        let message = TransactionSummary::from_json(payload)
            .expect("summary")
            .to_commitment();
        let signature = match self {
            Self::Falcon(signer) => ProposalSignature::Falcon {
                signature: signer.sign_word(message),
            },
            Self::Ecdsa(signer) => ProposalSignature::Ecdsa {
                signature: signer.sign_word(message),
                public_key: Some(signer.pubkey_hex.clone()),
                message_format: EcdsaMessageFormat::Raw,
            },
        };
        CosignerSignature {
            signature,
            timestamp: EARLIER.to_string(),
            signer_id: self.commitment(),
        }
    }
}

/// One guarded multisig account and its honest history.
struct World {
    client: MidenNetworkClient,
    account_id: String,
    scheme: SignatureScheme,
    signers: Vec<Key>,
    attacker: Key,
    forged: Account,
    initial_commitment: String,
    creation_payload: Value,
    guardian_commitment: String,
}

impl World {
    fn new(scheme: SignatureScheme, signer_count: usize) -> Self {
        Self::with_guardian(scheme, signer_count, scheme)
    }

    fn with_guardian(
        scheme: SignatureScheme,
        signer_count: usize,
        guardian_scheme: SignatureScheme,
    ) -> Self {
        let signers: Vec<Key> = (0..signer_count).map(|_| Key::new(scheme)).collect();
        let attacker = Key::new(scheme);
        let guardian = Key::new(guardian_scheme);
        let approvers: Vec<(Word, SignatureScheme)> = signers
            .iter()
            .map(|key| (key.word(), key.scheme()))
            .collect();
        let account = build_guarded_multisig_account(
            [7; 32],
            1,
            &approvers,
            (guardian.word(), guardian_scheme),
        );
        let forged = build_guarded_multisig_account(
            [9; 32],
            1,
            &[(attacker.word(), scheme)],
            (guardian.word(), scheme),
        );
        Self {
            client: MidenNetworkClient::lazy_for_test(NetworkType::MidenTestnet),
            account_id: account.id().to_hex(),
            scheme,
            initial_commitment: commitment_of(&account),
            creation_payload: creation_payload(account),
            signers,
            attacker,
            forged,
            guardian_commitment: guardian.commitment(),
        }
    }

    fn id(&self) -> miden_protocol::account::AccountId {
        miden_protocol::account::AccountId::from_hex(&self.account_id).expect("account id")
    }

    fn forged_commitment(&self) -> String {
        commitment_of(&self.forged)
    }

    fn created(&self) -> AppliedState {
        self.client
            .apply_delta(&Value::Null, &self.creation_payload)
            .expect("creation delta builds the account")
    }

    fn apply(&self, state: &AppliedState, payload: &Value) -> AppliedState {
        self.client
            .apply_delta(&state.state_json, payload)
            .expect("delta applies")
    }

    fn plain_payload(&self, block: u32) -> Value {
        summary_json(
            AccountDelta::new(
                self.id(),
                AccountStoragePatch::default(),
                AccountVaultDelta::default(),
                AccountCodePatch::default(),
                Felt::ONE,
            )
            .expect("delta"),
            block,
        )
    }

    fn replace_signer_payload(&self, index: u32, key: &Key, block: u32) -> Value {
        let storage = AccountStoragePatch::builder()
            .update_map(
                AuthGuardedMultisig::approver_public_keys_slot().clone(),
                [(StorageMapKey::new(Word::from([index, 0, 0, 0])), key.word())],
            )
            .build();
        summary_json(
            AccountDelta::new(
                self.id(),
                storage,
                AccountVaultDelta::default(),
                AccountCodePatch::default(),
                Felt::ONE,
            )
            .expect("delta"),
            block,
        )
    }

    fn creation_row(&self, status: DeltaStatus) -> DeltaObject {
        self.row(
            1,
            &self.initial_commitment,
            None,
            self.creation_payload.clone(),
            status,
        )
    }

    fn row(
        &self,
        nonce: u64,
        prev: &str,
        new: Option<&str>,
        payload: Value,
        status: DeltaStatus,
    ) -> DeltaObject {
        DeltaObject {
            account_id: self.account_id.clone(),
            nonce,
            prev_commitment: prev.to_string(),
            new_commitment: new.map(str::to_string),
            delta_payload: payload,
            status,
            ..DeltaObject::default()
        }
    }

    fn proposal(
        &self,
        nonce: u64,
        prev: &str,
        payload: &Value,
        signatures: Vec<CosignerSignature>,
        proposal_type: &str,
    ) -> ProposalRecord {
        let commitment = self
            .client
            .delta_proposal_id(&self.account_id, nonce, payload)
            .expect("proposal id");
        ProposalRecord {
            account_id: self.account_id.clone(),
            commitment,
            proposal: DeltaObject {
                account_id: self.account_id.clone(),
                nonce,
                prev_commitment: prev.to_string(),
                delta_payload: json!({
                    "tx_summary": payload,
                    "signatures": [],
                    "metadata": { "proposal_type": proposal_type },
                }),
                status: DeltaStatus::Pending {
                    timestamp: EARLIER.to_string(),
                    proposer_id: signatures
                        .first()
                        .map_or_else(String::new, |signature| signature.signer_id.clone()),
                    cosigner_sigs: signatures,
                },
                ..DeltaObject::default()
            },
        }
    }

    /// The stored auth the attacker wrote: its own key, under a variant and
    /// guardian scheme the true state does not use.
    fn forged_auth(&self) -> Auth {
        Auth::Miden {
            cosigners: vec![MidenCosigner {
                commitment: self.attacker.commitment(),
                scheme: self.attacker.scheme(),
            }],
            guardian_scheme: other_scheme(self.scheme),
        }
    }

    fn inputs(&self, chain: ChainRead, deltas: Vec<DeltaObject>) -> AccountInputs {
        self.inputs_with(chain, deltas, Vec::new(), Vec::new())
    }

    fn inputs_with(
        &self,
        chain: ChainRead,
        deltas: Vec<DeltaObject>,
        superseded: Vec<DeltaObject>,
        proposals: Vec<ProposalRecord>,
    ) -> AccountInputs {
        AccountInputs {
            metadata: AccountMetadata {
                account_id: self.account_id.clone(),
                auth: self.forged_auth(),
                network_config: NetworkConfig::miden_default(),
                created_at: EARLIER.to_string(),
                updated_at: EARLIER.to_string(),
                has_pending_candidate: false,
                paused_at: None,
                paused_reason: None,
                released_at: None,
            },
            stored: StateObject {
                account_id: self.account_id.clone(),
                state_json: self.forged.to_json(),
                commitment: self.forged_commitment(),
                nonce: Some(0),
                created_at: EARLIER.to_string(),
                updated_at: EARLIER.to_string(),
                auth_scheme: String::new(),
            },
            chain,
            deltas,
            superseded: superseded
                .into_iter()
                .map(|delta| SupersededDelta {
                    delta,
                    superseded_at: chrono::DateTime::UNIX_EPOCH,
                })
                .collect(),
            proposals,
            now: NOW.to_string(),
        }
    }

    fn true_auth(&self, signers: &[&Key]) -> Auth {
        let commitments = signers.iter().map(|key| key.commitment()).collect();
        match self.scheme {
            SignatureScheme::Falcon => Auth::MidenFalconRpo {
                cosigner_commitments: commitments,
            },
            SignatureScheme::Ecdsa => Auth::MidenEcdsa {
                cosigner_commitments: commitments,
            },
        }
    }
}

/// An honest history: creation, then two plain transactions.
struct History {
    states: Vec<AppliedState>,
    payloads: Vec<Value>,
}

impl History {
    fn new(world: &World) -> Self {
        let created = world.created();
        let second_payload = world.plain_payload(2);
        let second = world.apply(&created, &second_payload);
        let third_payload = world.plain_payload(3);
        let third = world.apply(&second, &third_payload);
        Self {
            states: vec![created, second, third],
            payloads: vec![
                world.creation_payload.clone(),
                second_payload,
                third_payload,
            ],
        }
    }

    fn commitment(&self, index: usize) -> &str {
        &self.states[index].commitment
    }

    fn tip(&self) -> &AppliedState {
        self.states.last().expect("tip")
    }

    fn chain(&self) -> ChainRead {
        ChainRead::Mismatch {
            commitment: self.tip().commitment.clone(),
        }
    }

    fn creation(&self, world: &World) -> DeltaObject {
        DeltaObject {
            new_commitment: Some(self.commitment(0).to_string()),
            ..world.creation_row(canonical())
        }
    }

    fn canonical_rows(&self, world: &World) -> Vec<DeltaObject> {
        vec![
            self.creation(world),
            world.row(
                2,
                self.commitment(0),
                Some(self.commitment(1)),
                self.payloads[1].clone(),
                canonical(),
            ),
            world.row(
                3,
                self.commitment(1),
                Some(self.commitment(2)),
                self.payloads[2].clone(),
                canonical(),
            ),
        ]
    }
}

fn canonical() -> DeltaStatus {
    DeltaStatus::canonical(EARLIER.to_string())
}

fn candidate() -> DeltaStatus {
    DeltaStatus::candidate(EARLIER.to_string())
}

fn other_scheme(scheme: SignatureScheme) -> SignatureScheme {
    match scheme {
        SignatureScheme::Falcon => SignatureScheme::Ecdsa,
        SignatureScheme::Ecdsa => SignatureScheme::Falcon,
    }
}

fn commitment_of(account: &Account) -> String {
    format!("0x{}", hex::encode(account.to_commitment().as_bytes()))
}

/// The delta of the transaction that creates `account`: its code and its
/// whole storage, leaving it at nonce 1.
fn creation_payload(account: Account) -> Value {
    let (id, vault, storage, code, _, _) = account.into_parts();
    let deployed = Account::new_existing(id, vault, storage, code, Felt::ONE);
    summary_json(AccountDelta::try_from(deployed).expect("creation delta"), 1)
}

fn summary_json(delta: AccountDelta, block: u32) -> Value {
    TransactionSummary::new(
        delta,
        InputNotes::new(Vec::new()).expect("input notes"),
        RawOutputNotes::new(Vec::new()).expect("output notes"),
        miden_protocol::block::BlockNumber::from(block),
        Word::from([ZERO; 4]),
        0,
        TransactionSummaryUserParams::new([ZERO; 6]),
    )
    .to_json()
}

fn heal(plan: AccountPlan) -> HealPlan {
    match plan {
        AccountPlan::Heal(plan) => *plan,
        AccountPlan::Skip { reason } => panic!("expected a heal, got skip: {reason}"),
        AccountPlan::Unrecovered(entry) => {
            panic!(
                "expected a heal, got {:?}: {:?}",
                entry.reason, entry.detail
            )
        }
    }
}

fn unrecovered(plan: AccountPlan) -> UnrecoveredAccount {
    match plan {
        AccountPlan::Unrecovered(entry) => *entry,
        AccountPlan::Skip { reason } => panic!("expected unrecovered, got skip: {reason}"),
        AccountPlan::Heal(_) => panic!("expected unrecovered, got a heal"),
    }
}

fn delta_puts(plan: &HealPlan) -> Vec<&DeltaObject> {
    plan.write
        .delta_writes
        .iter()
        .filter_map(|write| match write {
            HealDeltaWrite::Put(delta) => Some(delta),
            HealDeltaWrite::Delete { .. } => None,
        })
        .collect()
}

fn delta_deletes(plan: &HealPlan) -> Vec<u64> {
    plan.write
        .delta_writes
        .iter()
        .filter_map(|write| match write {
            HealDeltaWrite::Delete { nonce } => Some(*nonce),
            HealDeltaWrite::Put(_) => None,
        })
        .collect()
}

fn proposal_deletes(plan: &HealPlan) -> Vec<String> {
    plan.write
        .proposal_writes
        .iter()
        .filter_map(|write| match write {
            HealProposalWrite::Delete { commitment } => Some(commitment.clone()),
            HealProposalWrite::Put { .. } => None,
        })
        .collect()
}

fn proposal_puts(plan: &HealPlan) -> Vec<(&String, &DeltaObject)> {
    plan.write
        .proposal_writes
        .iter()
        .filter_map(|write| match write {
            HealProposalWrite::Put {
                commitment,
                proposal,
            } => Some((commitment, proposal)),
            HealProposalWrite::Delete { .. } => None,
        })
        .collect()
}

fn verdict_of(plan: &HealPlan, row: RowKind, id: &str) -> RowVerdict {
    plan.actions
        .iter()
        .find(|action| action.row == row && action.id == id)
        .unwrap_or_else(|| panic!("no action for {row:?} {id}"))
        .verdict
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

const SCHEMES: [SignatureScheme; 2] = [SignatureScheme::Falcon, SignatureScheme::Ecdsa];

#[tokio::test]
async fn a_takeover_is_healed_from_the_creation_delta_and_canonical_rows() {
    for scheme in SCHEMES {
        let world = World::new(scheme, 2);
        let history = History::new(&world);
        let mut deltas = history.canonical_rows(&world);
        deltas[2].new_commitment = Some(world.forged_commitment());

        let plan = heal(plan_account(
            &world.client,
            world.inputs(history.chain(), deltas),
        ));

        assert_eq!(plan.write.state.commitment, history.tip().commitment);
        assert_eq!(plan.write.state.nonce, history.tip().nonce);
        assert_eq!(plan.write.state.state_json, history.tip().state_json);
        assert_eq!(plan.write.state.created_at, EARLIER);
        assert_eq!(plan.write.state.updated_at, NOW);
        assert_eq!(
            plan.write.expected_state_commitment,
            world.forged_commitment()
        );
        assert_eq!(
            plan.write.auth,
            world.true_auth(&[&world.signers[0], &world.signers[1]])
        );
        assert_eq!(
            plan.true_signers,
            sorted(world.signers.iter().map(Key::commitment).collect())
        );
        assert_eq!(
            plan.guardian_commitment.as_deref(),
            Some(world.guardian_commitment.as_str())
        );
        assert!(!plan.write.has_pending_candidate);
        assert_configure_accepts(&world, &plan).await;

        let puts = delta_puts(&plan);
        assert_eq!(
            puts.len(),
            1,
            "only the row with a forged commitment changes"
        );
        assert_eq!(puts[0].nonce, 3);
        assert_eq!(
            puts[0].new_commitment.as_deref(),
            Some(history.commitment(2))
        );
        assert_eq!(puts[0].status, DeltaStatus::canonical(EARLIER.to_string()));
        assert!(delta_deletes(&plan).is_empty());
    }
}

/// The healed auth is the one `/configure` accepts for the healed state:
/// the state's signer set and schemes, and its guardian scheme.
async fn assert_configure_accepts(world: &World, plan: &HealPlan) {
    let state_json = &plan.write.state.state_json;
    assert_eq!(
        world
            .client
            .should_update_auth(state_json, &plan.write.auth)
            .await
            .expect("signer set extracts"),
        Some(plan.write.auth.clone())
    );
    world
        .client
        .validate_guardian_commitment(
            state_json,
            &world.guardian_commitment,
            plan.write.auth.guardian_scheme(),
        )
        .expect("the auth names the guardian scheme the state stores");
    world
        .client
        .validate_multisig_auth(state_json)
        .expect("guarded multisig");
}

#[tokio::test]
async fn a_mixed_scheme_account_heals_to_a_per_signer_auth() {
    let world = World::with_guardian(SignatureScheme::Falcon, 2, SignatureScheme::Ecdsa);
    let history = History::new(&world);

    let plan = heal(plan_account(
        &world.client,
        world.inputs(history.chain(), history.canonical_rows(&world)),
    ));

    assert_eq!(
        plan.write.auth,
        Auth::Miden {
            cosigners: world
                .signers
                .iter()
                .map(|key| MidenCosigner {
                    commitment: key.commitment(),
                    scheme: SignatureScheme::Falcon,
                })
                .collect(),
            guardian_scheme: SignatureScheme::Ecdsa,
        }
    );
    assert_configure_accepts(&world, &plan).await;
}

#[tokio::test]
async fn the_path_continues_through_a_candidate_and_a_retained_row() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);
    let deltas = vec![
        history.creation(&world),
        world.row(
            2,
            history.commitment(0),
            Some(history.commitment(1)),
            history.payloads[1].clone(),
            DeltaStatus::retained(EARLIER.to_string(), RetainReason::Diverged),
        ),
        world.row(
            3,
            history.commitment(1),
            None,
            history.payloads[2].clone(),
            candidate(),
        ),
    ];

    let plan = heal(plan_account(
        &world.client,
        world.inputs(history.chain(), deltas),
    ));

    let puts = delta_puts(&plan);
    assert_eq!(puts.len(), 2);
    for (put, index) in puts.iter().zip([1, 2]) {
        assert_eq!(
            put.new_commitment.as_deref(),
            Some(history.commitment(index))
        );
        assert_eq!(put.status, DeltaStatus::canonical(NOW.to_string()));
    }
    assert!(!plan.write.has_pending_candidate);
    assert_eq!(plan.write.state.commitment, history.tip().commitment);
}

#[tokio::test]
async fn a_superseded_row_on_the_path_replaces_the_row_at_its_nonce() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);
    let attacker_payload = world.plain_payload(40);
    let deltas = vec![
        history.creation(&world),
        world.row(
            2,
            &world.forged_commitment(),
            None,
            attacker_payload,
            candidate(),
        ),
        world.row(
            3,
            history.commitment(1),
            Some(history.commitment(2)),
            history.payloads[2].clone(),
            canonical(),
        ),
    ];
    let superseded = vec![world.row(
        2,
        history.commitment(0),
        None,
        history.payloads[1].clone(),
        DeltaStatus::retained(EARLIER.to_string(), RetainReason::RetryExhausted),
    )];

    let plan = heal(plan_account(
        &world.client,
        world.inputs_with(history.chain(), deltas, superseded, Vec::new()),
    ));

    let puts = delta_puts(&plan);
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].nonce, 2);
    assert_eq!(puts[0].prev_commitment, history.commitment(0));
    assert_eq!(puts[0].status, DeltaStatus::canonical(NOW.to_string()));
    assert!(delta_deletes(&plan).is_empty(), "the put replaces the row");
    assert!(
        plan.actions
            .iter()
            .any(|action| action.id == "2" && action.verdict == RowVerdict::Delete)
    );
    assert!(!plan.write.has_pending_candidate);
}

#[tokio::test]
async fn an_update_signers_delta_on_the_path_changes_the_true_signer_set() {
    for scheme in SCHEMES {
        let world = World::new(scheme, 2);
        let created = world.created();
        let newcomer = Key::new(scheme);
        let rotate = world.replace_signer_payload(1, &newcomer, 2);
        let rotated = world.apply(&created, &rotate);
        let rotation_proposal = world.proposal(
            2,
            &created.commitment,
            &rotate,
            vec![
                world.signers[0].sign(&rotate),
                world.signers[1].sign(&rotate),
            ],
            "update_signers",
        );
        let next = world.plain_payload(3);
        let by_removed = world.proposal(
            3,
            &rotated.commitment,
            &next,
            vec![world.signers[1].sign(&next)],
            "custom",
        );
        let other = world.plain_payload(4);
        let by_newcomer = world.proposal(
            3,
            &rotated.commitment,
            &other,
            vec![newcomer.sign(&other)],
            "custom",
        );
        let deltas = vec![
            world.creation_row(canonical()),
            world.row(2, &created.commitment, None, rotate.clone(), candidate()),
        ];
        let chain = ChainRead::Mismatch {
            commitment: rotated.commitment.clone(),
        };

        let plan = heal(plan_account(
            &world.client,
            world.inputs_with(
                chain,
                deltas,
                Vec::new(),
                vec![
                    rotation_proposal.clone(),
                    by_removed.clone(),
                    by_newcomer.clone(),
                ],
            ),
        ));

        assert_eq!(
            plan.write.auth,
            world.true_auth(&[&world.signers[0], &newcomer])
        );
        assert_eq!(
            plan.true_signers,
            sorted(vec![world.signers[0].commitment(), newcomer.commitment()])
        );
        let deleted = proposal_deletes(&plan);
        assert!(
            deleted.contains(&rotation_proposal.commitment),
            "stale base"
        );
        assert!(
            deleted.contains(&by_removed.commitment),
            "signer was removed"
        );
        assert!(!deleted.contains(&by_newcomer.commitment));
        assert_eq!(
            verdict_of(&plan, RowKind::Proposal, &by_newcomer.commitment),
            RowVerdict::Keep
        );
    }
}

#[tokio::test]
async fn attacker_proposals_are_deleted() {
    for scheme in SCHEMES {
        let world = World::new(scheme, 2);
        let history = History::new(&world);
        let tip = history.commitment(2).to_string();
        let on_forged = world.plain_payload(10);
        let unsigned = world.plain_payload(11);
        let attacker_signed = world.plain_payload(12);
        let proposals = vec![
            world.proposal(
                1,
                &world.forged_commitment(),
                &on_forged,
                vec![world.attacker.sign(&on_forged)],
                "custom",
            ),
            world.proposal(4, &tip, &unsigned, Vec::new(), "custom"),
            world.proposal(
                4,
                &tip,
                &attacker_signed,
                vec![world.attacker.sign(&attacker_signed)],
                "switch_guardian",
            ),
        ];
        let ids: Vec<String> = proposals
            .iter()
            .map(|record| record.commitment.clone())
            .collect();

        let plan = heal(plan_account(
            &world.client,
            world.inputs_with(
                history.chain(),
                history.canonical_rows(&world),
                Vec::new(),
                proposals,
            ),
        ));

        assert_eq!(sorted(proposal_deletes(&plan)), sorted(ids));
        assert!(proposal_puts(&plan).is_empty());
    }
}

#[tokio::test]
async fn a_real_proposal_is_kept_and_an_added_attacker_signature_is_removed() {
    for scheme in SCHEMES {
        let world = World::new(scheme, 2);
        let history = History::new(&world);
        let tip = history.commitment(2).to_string();
        let clean = world.plain_payload(20);
        let mixed = world.plain_payload(21);
        let clean_proposal = world.proposal(
            4,
            &tip,
            &clean,
            vec![world.signers[0].sign(&clean)],
            "custom",
        );
        let real_signature = world.signers[1].sign(&mixed);
        let mixed_proposal = world.proposal(
            4,
            &tip,
            &mixed,
            vec![real_signature.clone(), world.attacker.sign(&mixed)],
            "custom",
        );

        let plan = heal(plan_account(
            &world.client,
            world.inputs_with(
                history.chain(),
                history.canonical_rows(&world),
                Vec::new(),
                vec![clean_proposal.clone(), mixed_proposal.clone()],
            ),
        ));

        assert!(proposal_deletes(&plan).is_empty());
        assert_eq!(
            verdict_of(&plan, RowKind::Proposal, &clean_proposal.commitment),
            RowVerdict::Keep
        );
        let puts = proposal_puts(&plan);
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].0, &mixed_proposal.commitment);
        let DeltaStatus::Pending { cosigner_sigs, .. } = &puts[0].1.status else {
            panic!("the kept proposal stays pending");
        };
        assert_eq!(cosigner_sigs, &vec![real_signature]);
        assert_eq!(
            verdict_of(&plan, RowKind::Proposal, &mixed_proposal.commitment),
            RowVerdict::KeepWithFewerSignatures
        );
    }
}

#[tokio::test]
async fn a_stale_real_proposal_is_deleted() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);
    let stale = world.plain_payload(30);
    let proposal = world.proposal(
        3,
        history.commitment(1),
        &stale,
        vec![world.signers[0].sign(&stale)],
        "custom",
    );

    let plan = heal(plan_account(
        &world.client,
        world.inputs_with(
            history.chain(),
            history.canonical_rows(&world),
            Vec::new(),
            vec![proposal.clone()],
        ),
    ));

    assert_eq!(proposal_deletes(&plan), vec![proposal.commitment]);
}

#[tokio::test]
async fn an_attacker_candidate_on_a_forged_base_and_an_off_path_canonical_row_are_deleted() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);
    let forged_payload = world.plain_payload(50);
    let mut deltas = history.canonical_rows(&world);
    deltas.push(world.row(
        4,
        &world.forged_commitment(),
        None,
        forged_payload.clone(),
        candidate(),
    ));
    deltas.push(world.row(
        9,
        &world.forged_commitment(),
        Some(&world.forged_commitment()),
        world.plain_payload(51),
        canonical(),
    ));
    let attacker_proposal = world.proposal(
        4,
        &world.forged_commitment(),
        &forged_payload,
        vec![world.attacker.sign(&forged_payload)],
        "custom",
    );

    let plan = heal(plan_account(
        &world.client,
        world.inputs_with(history.chain(), deltas, Vec::new(), vec![attacker_proposal]),
    ));

    assert_eq!(sorted_nonces(delta_deletes(&plan)), vec![4, 9]);
    assert!(delta_puts(&plan).is_empty());
    assert!(!plan.write.has_pending_candidate);
}

fn sorted_nonces(mut nonces: Vec<u64>) -> Vec<u64> {
    nonces.sort_unstable();
    nonces
}

#[tokio::test]
async fn a_candidate_on_the_true_base_without_a_passing_proposal_is_retained() {
    for scheme in SCHEMES {
        let world = World::new(scheme, 2);
        let history = History::new(&world);
        let tip = history.commitment(2).to_string();
        let parked = world.plain_payload(60);
        let unapproved = world.proposal(
            4,
            &tip,
            &parked,
            vec![world.attacker.sign(&parked)],
            "custom",
        );
        let mut deltas = history.canonical_rows(&world);
        deltas.push(world.row(4, &tip, None, parked, candidate()));
        deltas.push(world.row(
            5,
            &tip,
            None,
            world.plain_payload(61),
            DeltaStatus::discarded(EARLIER.to_string()),
        ));

        let plan = heal(plan_account(
            &world.client,
            world.inputs_with(history.chain(), deltas, Vec::new(), vec![unapproved]),
        ));

        let puts = delta_puts(&plan);
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].nonce, 4);
        assert_eq!(
            puts[0].status,
            DeltaStatus::retained(NOW.to_string(), RetainReason::Orphaned)
        );
        assert_eq!(verdict_of(&plan, RowKind::Delta, "4"), RowVerdict::Retain);
        assert_eq!(verdict_of(&plan, RowKind::Delta, "5"), RowVerdict::Keep);
        assert!(delta_deletes(&plan).is_empty());
        assert!(!plan.write.has_pending_candidate);
    }
}

#[tokio::test]
async fn an_approved_candidate_on_the_true_base_is_kept_and_extends_the_base() {
    for scheme in SCHEMES {
        let world = World::new(scheme, 2);
        let history = History::new(&world);
        let tip = history.commitment(2).to_string();
        let queued = world.plain_payload(70);
        let approval = world.proposal(
            4,
            &tip,
            &queued,
            vec![world.signers[0].sign(&queued)],
            "custom",
        );
        let post_state = world.apply(history.tip(), &queued);
        let follow_up = world.plain_payload(71);
        let on_candidate = world.proposal(
            5,
            &post_state.commitment,
            &follow_up,
            vec![world.signers[1].sign(&follow_up)],
            "custom",
        );
        let mut deltas = history.canonical_rows(&world);
        deltas.push(world.row(4, &tip, Some(&post_state.commitment), queued, candidate()));

        let plan = heal(plan_account(
            &world.client,
            world.inputs_with(
                history.chain(),
                deltas,
                Vec::new(),
                vec![approval.clone(), on_candidate.clone()],
            ),
        ));

        assert!(delta_puts(&plan).is_empty());
        assert!(delta_deletes(&plan).is_empty());
        assert_eq!(verdict_of(&plan, RowKind::Delta, "4"), RowVerdict::Keep);
        assert!(plan.write.has_pending_candidate);
        assert_eq!(
            verdict_of(&plan, RowKind::Proposal, &on_candidate.commitment),
            RowVerdict::Keep
        );
        assert!(proposal_deletes(&plan).is_empty());
        assert_eq!(
            verdict_of(&plan, RowKind::Proposal, &approval.commitment),
            RowVerdict::Keep
        );
    }
}

#[tokio::test]
async fn no_creation_delta_is_no_base_and_needs_the_earliest_base() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);
    let deltas = history.canonical_rows(&world).split_off(1);

    let entry = unrecovered(plan_account(
        &world.client,
        world.inputs(history.chain(), deltas),
    ));

    assert_eq!(entry.reason, RecoveryReason::NoBase);
    assert_eq!(
        entry.needed.state_with_commitment.as_deref(),
        Some(history.commitment(0))
    );
    assert_eq!(entry.creation_delta.status, "none");
    assert_eq!(entry.replay.base, "none");
    assert!(entry.pause);
    assert!(!entry.release);
}

#[tokio::test]
async fn no_rows_at_all_is_no_base_and_needs_the_on_chain_state() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);

    let entry = unrecovered(plan_account(
        &world.client,
        world.inputs(history.chain(), Vec::new()),
    ));

    assert_eq!(entry.reason, RecoveryReason::NoBase);
    assert_eq!(
        entry.needed.state_with_commitment.as_deref(),
        Some(history.commitment(2))
    );
}

#[tokio::test]
async fn rows_that_end_early_are_replay_short() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);
    let mut deltas = history.canonical_rows(&world);
    deltas.truncate(2);
    let attacker_proposal = world.plain_payload(80);
    let proposals = vec![world.proposal(
        3,
        history.commitment(1),
        &attacker_proposal,
        vec![
            world.signers[0].sign(&attacker_proposal),
            world.attacker.sign(&attacker_proposal),
        ],
        "custom",
    )];

    let entry = unrecovered(plan_account(
        &world.client,
        world.inputs_with(history.chain(), deltas, Vec::new(), proposals),
    ));

    assert_eq!(entry.reason, RecoveryReason::ReplayShort);
    assert_eq!(entry.replay.base, "creation_delta");
    assert_eq!(entry.replay.rows_applied, 2);
    assert_eq!(
        entry.replay.last_commitment.as_deref(),
        Some(history.commitment(1))
    );
    assert_eq!(entry.replay.last_nonce, history.states[1].nonce);
    assert_eq!(
        entry.replay.last_signers,
        Some(vec![world.signers[0].commitment()])
    );
    assert_eq!(
        entry.needed.state_with_commitment.as_deref(),
        Some(history.commitment(2))
    );
    assert_eq!(entry.creation_delta.status, "canonical");
    assert_eq!(
        entry.creation_delta.new_commitment.as_deref(),
        Some(history.commitment(0))
    );
    assert!(entry.delta_rows.iter().all(|row| row.applied));
    assert_eq!(
        sorted(entry.proposals[0].verified_signers.clone()),
        sorted(vec![
            world.signers[0].commitment(),
            world.attacker.commitment()
        ])
    );
    assert_eq!(entry.stored.signers, vec![world.attacker.commitment()]);
    assert!(entry.pause);
    let serialized = serde_json::to_string(&entry).expect("entry serializes");
    assert!(!serialized.contains("tx_summary"));
    assert!(!serialized.contains("signature"));
}

#[tokio::test]
async fn a_switch_guardian_proposal_at_the_stop_is_left_this_guardian_and_still_pauses() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);
    let mut deltas = history.canonical_rows(&world);
    deltas.truncate(2);
    let switch = world.plain_payload(90);
    let proposals = vec![world.proposal(
        3,
        history.commitment(1),
        &switch,
        vec![world.attacker.sign(&switch)],
        "switch_guardian",
    )];

    let entry = unrecovered(plan_account(
        &world.client,
        world.inputs_with(history.chain(), deltas, Vec::new(), proposals),
    ));

    assert_eq!(entry.reason, RecoveryReason::LeftThisGuardian);
    assert!(entry.pause);
    assert!(!entry.release);
}

#[tokio::test]
async fn a_canonical_link_that_does_not_apply_is_undecodable() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let history = History::new(&world);
    let mut deltas = history.canonical_rows(&world);
    deltas[1].delta_payload = json!({ "not": "a summary" });

    let entry = unrecovered(plan_account(
        &world.client,
        world.inputs(history.chain(), deltas),
    ));

    assert_eq!(entry.reason, RecoveryReason::UndecodableRow);
    assert_eq!(entry.replay.rows_applied, 1);
    assert!(entry.pause);
}

#[tokio::test]
async fn absent_without_a_creation_delta_is_skipped() {
    let world = World::new(SignatureScheme::Falcon, 1);

    let plan = plan_account(&world.client, world.inputs(ChainRead::Absent, Vec::new()));

    assert!(matches!(plan, AccountPlan::Skip { .. }));
}

#[tokio::test]
async fn absent_with_canonical_history_is_an_rpc_error_that_does_not_pause() {
    let world = World::new(SignatureScheme::Falcon, 1);

    let entry = unrecovered(plan_account(
        &world.client,
        world.inputs(ChainRead::Absent, vec![world.creation_row(canonical())]),
    ));

    assert_eq!(entry.reason, RecoveryReason::RpcError);
    assert_eq!(entry.chain.status, "absent");
    assert!(!entry.pause);
    assert!(!entry.release);
}

#[tokio::test]
async fn absent_with_a_pending_creation_delta_on_the_stored_state_is_skipped() {
    let world = World::new(SignatureScheme::Falcon, 1);
    let mut inputs = world.inputs(ChainRead::Absent, vec![world.creation_row(candidate())]);
    inputs.stored.commitment = world.initial_commitment.clone();

    let plan = plan_account(&world.client, inputs);

    assert!(matches!(plan, AccountPlan::Skip { .. }));
}

#[tokio::test]
async fn absent_with_a_pending_creation_delta_on_another_state_is_creation_delta_pending() {
    let world = World::new(SignatureScheme::Falcon, 1);

    let entry = unrecovered(plan_account(
        &world.client,
        world.inputs(ChainRead::Absent, vec![world.creation_row(candidate())]),
    ));

    assert_eq!(entry.reason, RecoveryReason::CreationDeltaPending);
    assert_eq!(entry.creation_delta.status, "candidate");
    assert_eq!(
        entry.needed.state_with_commitment.as_deref(),
        Some(world.initial_commitment.as_str())
    );
    assert!(entry.pause);
}
