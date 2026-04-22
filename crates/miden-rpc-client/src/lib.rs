//! Minimal Miden RPC client using miden-node-proto crate
use miden_protocol::{account::AccountId, utils::serde::Serializable};
use tonic::{
    transport::{Channel, ClientTlsConfig},
    Request,
};

pub use miden_node_proto::generated::{
    account, block_producer, blockchain, note, primitives, rpc, store, transaction,
};
pub use rpc::api_client::ApiClient;

/// Simple wrapper around the tonic-generated ApiClient
pub struct MidenRpcClient {
    client: ApiClient<Channel>,
}

impl MidenRpcClient {
    pub async fn connect(endpoint: impl Into<String>) -> Result<Self, String> {
        let endpoint_str = endpoint.into();

        let channel = Channel::from_shared(endpoint_str.clone())
            .map_err(|e| format!("Invalid endpoint: {e}"))?
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(|e| format!("TLS config error: {e}"))?
            .connect()
            .await
            .map_err(|e| format!("Failed to connect to {endpoint_str}: {e}"))?;

        let client = ApiClient::new(channel);

        Ok(Self { client })
    }

    /// Get the underlying tonic ApiClient for full access to all RPC methods:
    pub fn client_mut(&mut self) -> &mut ApiClient<Channel> {
        &mut self.client
    }

    /// Get the status of the Miden node
    pub async fn get_status(&mut self) -> Result<rpc::RpcStatus, String> {
        let response = self
            .client
            .status(Request::new(()))
            .await
            .map_err(|e| format!("Status RPC failed: {e}"))?;

        Ok(response.into_inner())
    }

    /// Get block header by number with optional MMR proof
    pub async fn get_block_header(
        &mut self,
        block_num: Option<u32>,
        include_mmr_proof: bool,
    ) -> Result<rpc::BlockHeaderByNumberResponse, String> {
        let request = rpc::BlockHeaderByNumberRequest {
            block_num,
            include_mmr_proof: Some(include_mmr_proof),
        };

        let response = self
            .client
            .get_block_header_by_number(Request::new(request))
            .await
            .map_err(|e| format!("GetBlockHeaderByNumber RPC failed: {e}"))?;

        Ok(response.into_inner())
    }

    /// Submit a proven transaction to the network
    pub async fn submit_transaction(&mut self, proven_tx_bytes: Vec<u8>) -> Result<(), String> {
        let request = transaction::ProvenTransaction {
            transaction: proven_tx_bytes,
            transaction_inputs: None,
        };

        self.client
            .submit_proven_transaction(Request::new(request))
            .await
            .map_err(|e| format!("SubmitProvenTransaction RPC failed: {e}"))?;

        Ok(())
    }

    /// Sync state for specified accounts and note tags
    pub async fn sync_state(
        &mut self,
        block_num: u32,
        account_ids: Vec<Vec<u8>>,
        note_tags: Vec<u32>,
    ) -> Result<rpc::SyncNotesResponse, String> {
        if !account_ids.is_empty() {
            return Err(
                "Account syncing moved out of the raw node RPC wrapper in Miden 0.14; use miden-client state sync APIs for account state".to_string(),
            );
        }

        let request = rpc::SyncNotesRequest {
            block_range: Some(rpc::BlockRange {
                block_from: block_num,
                block_to: None,
            }),
            note_tags,
        };

        let response = self
            .client
            .sync_notes(Request::new(request))
            .await
            .map_err(|e| format!("SyncNotes RPC failed: {e}"))?;

        Ok(response.into_inner())
    }

    /// Check nullifiers and get their proofs
    pub async fn check_nullifiers(
        &mut self,
        nullifiers: Vec<primitives::Digest>,
    ) -> Result<rpc::CheckNullifiersResponse, String> {
        let request = rpc::NullifierList { nullifiers };

        let response = self
            .client
            .check_nullifiers(Request::new(request))
            .await
            .map_err(|e| format!("CheckNullifiers RPC failed: {e}"))?;

        Ok(response.into_inner())
    }

    /// Get notes by their IDs
    pub async fn get_notes_by_id(
        &mut self,
        note_ids: Vec<primitives::Digest>,
    ) -> Result<note::CommittedNoteList, String> {
        let note_ids = note_ids
            .into_iter()
            .map(|id| note::NoteId { id: Some(id) })
            .collect();
        let request = note::NoteIdList { ids: note_ids };

        let response = self
            .client
            .get_notes_by_id(Request::new(request))
            .await
            .map_err(|e| format!("GetNotesById RPC failed: {e}"))?;

        Ok(response.into_inner())
    }

    /// Fetch account commitment from the Miden network
    pub async fn get_account_commitment(
        &mut self,
        account_id: &AccountId,
    ) -> Result<String, String> {
        let account_id_bytes = account_id.to_bytes();

        let request = Request::new(rpc::AccountRequest {
            account_id: Some(account::AccountId {
                id: account_id_bytes.to_vec(),
            }),
            block_num: None,
            details: None,
        });

        let response = self
            .client
            .get_account(request)
            .await
            .map_err(|e| format!("RPC call failed: {e}"))?;

        let account_response = response.into_inner();

        // Get commitment from witness (which contains the state commitment)
        let witness = account_response
            .witness
            .ok_or_else(|| "No witness in account response".to_string())?;

        let commitment = witness
            .commitment
            .ok_or_else(|| "No commitment in witness".to_string())?;

        // Convert Digest to hex string
        let bytes = [
            commitment.d0.to_le_bytes(),
            commitment.d1.to_le_bytes(),
            commitment.d2.to_le_bytes(),
            commitment.d3.to_le_bytes(),
        ]
        .concat();

        Ok(format!("0x{}", hex::encode(bytes)))
    }

    /// Fetch full account details including serialized account data
    pub async fn get_account_details(
        &mut self,
        account_id: &AccountId,
    ) -> Result<rpc::AccountResponse, String> {
        let account_id_bytes = account_id.to_bytes();

        let request = Request::new(rpc::AccountRequest {
            account_id: Some(account::AccountId {
                id: account_id_bytes.to_vec(),
            }),
            block_num: None,
            details: None,
        });

        let response = self
            .client
            .get_account(request)
            .await
            .map_err(|e| format!("RPC call failed: {e}"))?;

        Ok(response.into_inner())
    }
}
