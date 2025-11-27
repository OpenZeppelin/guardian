use std::fmt;

use miden_client::account::component::{AccountComponent, BasicWallet};
use miden_client::account::{
    Account, AccountBuilder, AccountStorageMode, AccountType, StorageMap, StorageSlot,
};
use miden_client::transaction::{
    TransactionAuthenticator, TransactionExecutorError, TransactionKernel, TransactionRequest,
    TransactionRequestBuilder, TransactionRequestError, TransactionScript, TransactionSummary,
};
use miden_client::{Client, ClientError, Deserializable, ScriptBuilder, Word};

use miden_objects::account::auth::Signature;
use miden_objects::account::AccountId;
use miden_objects::{Felt, Hasher};

// Load Multisig+PSM Auth MASM code from consolidated file
const MULTISIG_PSM_AUTH: &str = include_str!("../masm/multisig-psm.masm");

#[allow(dead_code)]
#[derive(Debug)]
pub enum MultisigError {
    Assembly(String),
    TransactionRequest(TransactionRequestError),
    Client(ClientError),
    Executor(TransactionExecutorError),
    UnexpectedSuccess,
}

impl fmt::Display for MultisigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MultisigError::Assembly(err) => write!(f, "assembly error: {}", err),
            MultisigError::TransactionRequest(err) => {
                write!(f, "transaction request error: {}", err)
            }
            MultisigError::Client(err) => write!(f, "client error: {}", err),
            MultisigError::Executor(err) => write!(f, "transaction executor error: {}", err),
            MultisigError::UnexpectedSuccess => write!(
                f,
                "transaction executed successfully when failure was expected"
            ),
        }
    }
}

impl std::error::Error for MultisigError {}

impl From<TransactionRequestError> for MultisigError {
    fn from(err: TransactionRequestError) -> Self {
        MultisigError::TransactionRequest(err)
    }
}

impl From<ClientError> for MultisigError {
    fn from(err: ClientError) -> Self {
        MultisigError::Client(err)
    }
}

impl From<TransactionExecutorError> for MultisigError {
    fn from(err: TransactionExecutorError) -> Self {
        MultisigError::Executor(err)
    }
}

/// Create a multisig PSM account with 2-of-2 threshold
pub fn create_multisig_psm_account(
    client1_pubkey_hex: &str,
    client2_pubkey_hex: &str,
    psm_server_pubkey_hex: &str,
    init_seed: [u8; 32],
) -> Account {
    // Convert pubkey commitments (Word) from hex to Word
    // The client sends public key commitments (32 bytes), not full keys
    let psm_pubkey_bytes =
        hex::decode(&psm_server_pubkey_hex[2..]).expect("Failed to decode PSM pubkey");
    let psm_commitment_word =
        Word::read_from_bytes(&psm_pubkey_bytes).expect("Failed to convert PSM commitment to Word");

    let client1_pubkey_bytes =
        hex::decode(&client1_pubkey_hex[2..]).expect("Failed to decode client1 pubkey");
    let client1_commitment_word = Word::read_from_bytes(&client1_pubkey_bytes)
        .expect("Failed to convert client1 commitment to Word");

    let client2_pubkey_bytes =
        hex::decode(&client2_pubkey_hex[2..]).expect("Failed to decode client2 pubkey");
    let client2_commitment_word = Word::read_from_bytes(&client2_pubkey_bytes)
        .expect("Failed to convert client2 commitment to Word");

    // Build multisig auth component with storage slots
    // Storage layout for multisig.masm:
    // Slot 0: [threshold, num_approvers, 0, 0]
    // Slot 1: Public keys map (client1, client2)
    // Slot 2: Executed transactions map (empty initially)
    // Slot 3: Procedure thresholds map (empty initially)
    // Slot 4: PSM selector [1,0,0,0] = ON
    // Slot 5: PSM public key map

    // Slot 0: Multisig config - require 2 out of 2 signatures
    let slot_0 = StorageSlot::Value(Word::from([2u32, 2, 0, 0]));

    // Slot 1: Client public key commitments map
    let mut client_pubkeys_map = StorageMap::new();
    let _ = client_pubkeys_map.insert(
        Word::from([0u32, 0, 0, 0]), // index 0 - client1
        client1_commitment_word,
    );
    let _ = client_pubkeys_map.insert(
        Word::from([1u32, 0, 0, 0]), // index 1 - client2
        client2_commitment_word,
    );
    let slot_1 = StorageSlot::Map(client_pubkeys_map);

    // Slot 2: Executed transactions map (empty)
    let slot_2 = StorageSlot::Map(StorageMap::new());

    // Slot 3: Procedure thresholds map seeded with sentinel so the root differs from slot 2
    let mut proc_thresholds_map = StorageMap::new();
    proc_thresholds_map
        .insert(
            Word::from([u32::MAX, u32::MAX, u32::MAX, u32::MAX]),
            Word::from([1u32, 0, 0, 0]),
        )
        .expect("procedure threshold sentinel");
    let slot_3 = StorageSlot::Map(proc_thresholds_map);

    // Slot 4: PSM selector [1,0,0,0] = ON
    let slot_4 = StorageSlot::Value(Word::from([1u32, 0, 0, 0]));

    // Slot 5: PSM public key commitment map (single entry at index 0)
    let mut psm_key_map = StorageMap::new();
    let _ = psm_key_map.insert(
        Word::from([0u32, 0, 0, 0]), // index 0
        psm_commitment_word,
    );
    let slot_5 = StorageSlot::Map(psm_key_map);

    // Compile the consolidated multisig+PSM auth component
    // All PSM logic is now embedded in the same MASM file
    let auth_component = AccountComponent::compile(
        MULTISIG_PSM_AUTH.to_string(),
        TransactionKernel::assembler(),
        vec![slot_0, slot_1, slot_2, slot_3, slot_4, slot_5],
    )
    .expect("Failed to compile multisig+PSM auth component")
    .with_supports_all_types();

    // Create account with both clients as cosigners
    // Use build() for real client (includes seed for new account deployment)
    // For MockChain tests, use build_existing() instead
    AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public) // Use Public mode like the test
        .with_auth_component(auth_component)
        .with_component(BasicWallet)
        .build()
        .expect("Failed to build account")
}

#[allow(dead_code)]
/// Builds the advice payload for a multisig configuration update and returns the
/// resulting commitment that must appear on the operand stack before invoking
/// `update_signers_and_threshold`.
pub fn build_multisig_config_advice(
    threshold: u64,
    signer_commitments: &[Word],
) -> (Word, Vec<Felt>) {
    let num_approvers = signer_commitments.len() as u64;

    let mut payload = Vec::with_capacity(4 + signer_commitments.len() * 4);
    payload.extend_from_slice(&[
        Felt::new(threshold),
        Felt::new(num_approvers),
        Felt::new(0),
        Felt::new(0),
    ]);

    for commitment in signer_commitments.iter().rev() {
        payload.extend_from_slice(commitment.as_elements());
    }

    let config_hash: Word = Hasher::hash_elements(&payload);
    (config_hash, payload)
}

#[allow(dead_code)]
pub fn build_update_signers_script() -> Result<TransactionScript, String> {
    // Compile the consolidated multisig+PSM library for use in transaction scripts
    // Pass source code directly (like miden-lib build.rs does) to avoid namespace issues
    let multisig_psm_library = TransactionKernel::assembler()
        .assemble_library([MULTISIG_PSM_AUTH])
        .map_err(|err| format!("Failed to compile multisig+PSM library: {err}"))?;

    // Build the transaction script that calls update_signers_and_threshold
    // Use call.:: syntax for dynamically linked library procedure calls (v0.12+)
    let tx_script_code = "
        begin
            call.::update_signers_and_threshold
        end
    ";

    let tx_script = ScriptBuilder::new(true)
        .with_dynamically_linked_library(&multisig_psm_library)
        .map_err(|err| format!("Failed to link multisig+PSM library: {err}"))?
        .compile_tx_script(tx_script_code)
        .map_err(|err| format!("Failed to compile transaction script: {err}"))?;

    Ok(tx_script)
}

#[allow(dead_code, clippy::result_large_err)]
/// Builds a `TransactionRequest` that executes `update_signers_and_threshold` using the
/// provided multisig configuration. Returns the request together with the advice map key
/// (`MULTISIG_CONFIG_HASH`) so it can be reused elsewhere (e.g. for signature lookups).
pub fn build_update_signers_transaction_request<I>(
    threshold: u64,
    signer_commitments: &[Word],
    salt: Word,
    extra_advice: I,
) -> Result<(TransactionRequest, Word), MultisigError>
where
    I: IntoIterator<Item = (Word, Vec<Felt>)>,
{
    let (config_hash, config_values) = build_multisig_config_advice(threshold, signer_commitments);
    let script = build_update_signers_script().map_err(MultisigError::Assembly)?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .script_arg(config_hash)
        .extend_advice_map([(config_hash, config_values)])
        .extend_advice_map(extra_advice)
        .auth_arg(salt)
        .build()?;

    Ok((request, config_hash))
}

#[allow(dead_code)]
pub fn build_signature_advice_entry(
    pubkey_commitment: Word,
    message: Word,
    signature: &Signature,
) -> (Word, Vec<Felt>) {
    let key = Hasher::merge(&[pubkey_commitment, message]);
    let values = signature.to_prepared_signature(message);
    (key, values)
}

#[allow(dead_code)]
/// Executes the provided transaction request against the given account. If authentication fails
/// with `Unauthorized`, the contained `TransactionSummary` is returned. Any other execution
/// result (including success) is surfaced as an error.
pub async fn execute_transaction_for_summary<AUTH>(
    client: &mut Client<AUTH>,
    account_id: AccountId,
    request: TransactionRequest,
) -> Result<TransactionSummary, MultisigError>
where
    AUTH: TransactionAuthenticator + Sync + 'static,
{
    match client.execute_transaction(account_id, request).await {
        Ok(_) => Err(MultisigError::UnexpectedSuccess),
        Err(ClientError::TransactionExecutorError(TransactionExecutorError::Unauthorized(
            summary,
        ))) => Ok(*summary),
        Err(ClientError::TransactionExecutorError(err)) => Err(MultisigError::Executor(err)),
        Err(err) => Err(MultisigError::Client(err)),
    }
}
