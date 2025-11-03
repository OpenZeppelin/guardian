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

use miden_objects::account::{AccountId, Signature};
use miden_objects::assembly::diagnostics::NamedSource;
use miden_objects::{Felt, Hasher};

const MULTISIG_PSM_AUTH: &str = include_str!("../masm/multisig-psm.masm");

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

pub fn create_multisig_psm_account(
    threshold: u64,
    cosigner_commitments: &[&str],
    psm_server_pubkey_hex: &str,
    init_seed: [u8; 32],
) -> Account {
    let psm_pubkey_bytes =
        hex::decode(&psm_server_pubkey_hex[2..]).expect("Failed to decode PSM pubkey");
    let psm_commitment_word =
        Word::read_from_bytes(&psm_pubkey_bytes).expect("Failed to convert PSM commitment to Word");

    let num_cosigners = cosigner_commitments.len() as u64;

    let slot_0 = StorageSlot::Value(Word::from([threshold as u32, num_cosigners as u32, 0, 0]));

    let mut client_pubkeys_map = StorageMap::new();
    for (i, commitment_hex) in cosigner_commitments.iter().enumerate() {
        let pubkey_bytes = hex::decode(&commitment_hex[2..])
            .expect(&format!("Failed to decode cosigner {} pubkey", i));
        let commitment_word = Word::read_from_bytes(&pubkey_bytes).expect(&format!(
            "Failed to convert cosigner {} commitment to Word",
            i
        ));

        let _ = client_pubkeys_map.insert(Word::from([i as u32, 0, 0, 0]), commitment_word);
    }
    let slot_1 = StorageSlot::Map(client_pubkeys_map);

    let slot_2 = StorageSlot::Map(StorageMap::new());

    let mut proc_thresholds_map = StorageMap::new();
    // Seed with a sentinel so the map’s root differs from the empty-map root used elsewhere.
    // The sentinel key/value sit outside the real procedure-root space and never get read.
    proc_thresholds_map
        .insert(
            Word::from([u32::MAX, u32::MAX, u32::MAX, u32::MAX]),
            Word::from([1u32, 0, 0, 0]),
        )
        .expect("procedure threshold sentinel");
    let slot_3 = StorageSlot::Map(proc_thresholds_map);
    let slot_4 = StorageSlot::Value(Word::from([1u32, 0, 0, 0]));

    let mut psm_key_map = StorageMap::new();
    let _ = psm_key_map.insert(Word::from([0u32, 0, 0, 0]), psm_commitment_word);
    let slot_5 = StorageSlot::Map(psm_key_map);

    let auth_component = AccountComponent::compile(
        MULTISIG_PSM_AUTH.to_string(),
        TransactionKernel::assembler(),
        vec![slot_0, slot_1, slot_2, slot_3, slot_4, slot_5],
    )
    .expect("Failed to compile multisig+PSM auth component")
    .with_supports_all_types();

    AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(auth_component)
        .with_component(BasicWallet)
        .build()
        .expect("Failed to build account")
}

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

    let digest = Hasher::hash_elements(&payload);
    let config_hash: Word = digest.into();
    (config_hash, payload)
}

pub fn build_update_signers_script() -> Result<TransactionScript, String> {
    let multisig_psm_source = NamedSource::new("account_auth::multisig_psm", MULTISIG_PSM_AUTH);

    let multisig_psm_library = TransactionKernel::assembler()
        .assemble_library([multisig_psm_source])
        .map_err(|err| format!("Failed to compile multisig+PSM library: {err}"))?;

    let tx_script_code = "
        use.account_auth::multisig_psm

        begin
            exec.multisig_psm::update_signers_and_threshold
        end
    ";

    let tx_script = ScriptBuilder::new(true)
        .with_dynamically_linked_library(&multisig_psm_library)
        .map_err(|err| format!("Failed to link multisig+PSM library: {err}"))?
        .compile_tx_script(tx_script_code)
        .map_err(|err| format!("Failed to compile transaction script: {err}"))?;

    Ok(tx_script)
}

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
    let script = build_update_signers_script().map_err(|err| MultisigError::Assembly(err))?;

    let request = TransactionRequestBuilder::new()
        .custom_script(script)
        .script_arg(config_hash)
        .extend_advice_map([(config_hash, config_values)])
        .extend_advice_map(extra_advice)
        .auth_arg(salt)
        .build()?;

    Ok((request, config_hash))
}

pub fn build_signature_advice_entry(
    pubkey_commitment: Word,
    message: Word,
    signature: &Signature,
) -> (Word, Vec<Felt>) {
    let key = Hasher::merge(&[pubkey_commitment, message]);
    let values = signature.to_prepared_signature();
    (key, values)
}

pub async fn execute_transaction_for_summary<AUTH>(
    client: &mut Client<AUTH>,
    account_id: AccountId,
    request: TransactionRequest,
) -> Result<TransactionSummary, MultisigError>
where
    AUTH: TransactionAuthenticator + Sync + 'static,
{
    match client.new_transaction(account_id, request).await {
        Ok(_) => Err(MultisigError::UnexpectedSuccess),
        Err(ClientError::TransactionExecutorError(TransactionExecutorError::Unauthorized(
            summary,
        ))) => Ok(*summary),
        Err(ClientError::TransactionExecutorError(err)) => Err(MultisigError::Executor(err)),
        Err(err) => Err(MultisigError::Client(err)),
    }
}
