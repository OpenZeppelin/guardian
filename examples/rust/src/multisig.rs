use std::fmt;

use miden_client::account::Account;
use miden_client::assembly::CodeBuilder;
use miden_client::transaction::{
    TransactionAuthenticator, TransactionExecutorError, TransactionRequest,
    TransactionRequestBuilder, TransactionRequestError, TransactionScript, TransactionSummary,
};
use miden_client::{Client, ClientError, Deserializable, Word};
use miden_confidential_contracts::masm_builder::get_multisig_library;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::auth::Signature;
use miden_protocol::account::AccountId;
use miden_protocol::{Felt, Hasher};

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

/// Create a multisig GUARDIAN account with 2-of-2 threshold
pub fn create_multisig_guardian_account(
    client1_pubkey_hex: &str,
    client2_pubkey_hex: &str,
    guardian_server_pubkey_hex: &str,
    init_seed: [u8; 32],
) -> Account {
    let guardian_pubkey_bytes =
        hex::decode(&guardian_server_pubkey_hex[2..]).expect("Failed to decode GUARDIAN pubkey");
    let guardian_commitment = Word::read_from_bytes(&guardian_pubkey_bytes)
        .expect("Failed to convert GUARDIAN commitment to Word");

    let client1_pubkey_bytes =
        hex::decode(&client1_pubkey_hex[2..]).expect("Failed to decode client1 pubkey");
    let client1_commitment = Word::read_from_bytes(&client1_pubkey_bytes)
        .expect("Failed to convert client1 commitment to Word");

    let client2_pubkey_bytes =
        hex::decode(&client2_pubkey_hex[2..]).expect("Failed to decode client2 pubkey");
    let client2_commitment = Word::read_from_bytes(&client2_pubkey_bytes)
        .expect("Failed to convert client2 commitment to Word");

    let config = MultisigGuardianConfig::new(
        2, // 2-of-2 threshold
        vec![client1_commitment, client2_commitment],
        guardian_commitment,
    );

    MultisigGuardianBuilder::new(config)
        .with_seed(init_seed)
        .build()
        .expect("Failed to build MultisigGuardian account")
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
    let multisig_library =
        get_multisig_library().map_err(|err| format!("Failed to get multisig library: {err}"))?;

    let tx_script_code = "
        use oz_multisig::multisig
        begin
            call.multisig::update_signers_and_threshold
        end
    ";

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(multisig_library)
        .map_err(|err| format!("Failed to link multisig library: {err}"))?
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
