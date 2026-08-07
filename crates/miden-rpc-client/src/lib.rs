//! Minimal Miden RPC client using miden-node-proto crate
use miden_protocol::{account::AccountId, utils::serde::Serializable};
use tonic::{
    transport::{Channel, ClientTlsConfig},
    Request,
};

pub use miden_node_proto::generated::{account, blockchain, note, primitives, rpc, transaction};
pub use rpc::api_client::ApiClient;

/// Per-request deadline applied to the channel. Without one, a hung
/// node holds a caller (and everything awaiting it) indefinitely —
/// concurrent callers share the multiplexed channel, so no request may
/// be allowed to wait forever.
const RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Simple wrapper around the tonic-generated ApiClient
pub struct MidenRpcClient {
    client: ApiClient<Channel>,
}

impl MidenRpcClient {
    pub async fn connect(endpoint: impl Into<String>) -> Result<Self, String> {
        let endpoint_str = endpoint.into();

        let channel = Channel::from_shared(endpoint_str.clone())
            .map_err(|e| format!("Invalid endpoint: {e}"))?
            .timeout(RPC_TIMEOUT)
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(|e| format!("TLS config error: {e}"))?
            .connect()
            .await
            .map_err(|e| format!("Failed to connect to {endpoint_str}: {e}"))?;

        let client = ApiClient::new(channel);

        Ok(Self { client })
    }

    /// Builds a client over a lazily-created channel that is never proactively
    /// connected and skips TLS root loading. This lets pure, non-RPC call paths
    /// be unit-tested without a network or a system certificate store; issuing
    /// an actual RPC on the resulting client will fail to connect.
    pub fn lazy_unconnected(endpoint: impl Into<String>) -> Result<Self, String> {
        let channel = Channel::from_shared(endpoint.into())
            .map_err(|e| format!("Invalid endpoint: {e}"))?
            .connect_lazy();

        Ok(Self {
            client: ApiClient::new(channel),
        })
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
            .submit_proven_tx(Request::new(request))
            .await
            .map_err(|e| format!("SubmitProvenTx RPC failed: {e}"))?;

        Ok(())
    }

    /// Fetch the chain MMR delta needed to advance a partial MMR to the committed tip.
    ///
    /// `current_client_block_height` is the last block the caller **already has** in its
    /// partial MMR, so `0` means "genesis is present". The response payload carries merge
    /// authentication nodes plus new peaks — it is logarithmic in chain length, not
    /// proportional to it — which is what makes acquiring peaks from a cold start affordable.
    pub async fn sync_chain_mmr(
        &mut self,
        current_client_block_height: u32,
    ) -> Result<rpc::SyncChainMmrResponse, String> {
        let request = rpc::SyncChainMmrRequest {
            current_client_block_height,
            finality_level: rpc::FinalityLevel::Committed as i32,
        };

        let response = self
            .client
            .sync_chain_mmr(Request::new(request))
            .await
            .map_err(|e| format!("SyncChainMmr RPC failed: {e}"))?;

        Ok(response.into_inner())
    }

    /// Fetches one page of notes whose tags match within an explicit, inclusive block range.
    ///
    /// Keeping `block_to` explicit is important for transaction witness assembly: every returned
    /// block MMR path is generated for the forest at `block_to + 1`, so callers can pin note
    /// proofs to the forest committed by the reference block returned by `SyncChainMmr`.
    pub async fn sync_notes(
        &mut self,
        block_from: u32,
        block_to: u32,
        note_tags: Vec<u32>,
    ) -> Result<rpc::SyncNotesResponse, String> {
        let request = sync_notes_request(block_from, block_to, note_tags)?;

        let response = self
            .client
            .sync_notes(Request::new(request))
            .await
            .map_err(|e| format!("SyncNotes RPC failed: {e}"))?;

        Ok(response.into_inner())
    }

    /// Legacy note-sync wrapper. Account syncing is not supported by the raw node RPC client.
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

        self.sync_notes(block_num, u32::MAX, note_tags).await
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

    /// Fetch account commitment from the Miden network. Takes `&self`:
    /// the tonic client is cloned per call (a cheap handle onto the
    /// same multiplexed HTTP/2 channel), so concurrent callers never
    /// serialize on this client.
    pub async fn get_account_commitment(&self, account_id: &AccountId) -> Result<String, String> {
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
            .clone()
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

fn sync_notes_request(
    block_from: u32,
    block_to: u32,
    note_tags: Vec<u32>,
) -> Result<rpc::SyncNotesRequest, String> {
    if block_from > block_to {
        return Err(format!(
            "invalid SyncNotes block range: block_from {block_from} exceeds block_to {block_to}"
        ));
    }

    Ok(rpc::SyncNotesRequest {
        block_range: Some(rpc::BlockRange {
            block_from,
            block_to,
        }),
        note_tags,
    })
}

#[cfg(test)]
mod tests {
    use super::sync_notes_request;

    #[test]
    fn sync_notes_request_preserves_the_reference_block_upper_bound() {
        let request = sync_notes_request(17, 42, vec![7, 11]).expect("valid range");
        let range = request.block_range.expect("block range is required");

        assert_eq!(range.block_from, 17);
        assert_eq!(range.block_to, 42);
        assert_eq!(request.note_tags, vec![7, 11]);
    }

    #[test]
    fn sync_notes_request_rejects_an_inverted_range() {
        let error = sync_notes_request(43, 42, vec![7]).expect_err("range must be rejected");

        assert!(error.contains("block_from 43 exceeds block_to 42"));
    }
}
