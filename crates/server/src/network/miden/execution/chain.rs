//! The chain view one execution attempt runs against.
//!
//! Guardian keeps no synced chain state. Each attempt takes the node's committed tip as its
//! reference block `R`, rebuilds the chain MMR at forest `R` from a genesis-seeded `SyncChainMmr`
//! (its payload is the peak set, logarithmic in chain length), and tracks every block the
//! transaction authenticates against `R`: the summary's bound block and the creation block of
//! each authenticated input note. The executor never asks for the bound block itself, so the
//! tracked set has to be supplied here.

use std::collections::{BTreeMap, BTreeSet};

use miden_client::rpc::domain::sync::SyncTarget;
use miden_client::rpc::{NodeRpcClient, RpcError};
use miden_client::transaction::TransactionRequest;
use miden_protocol::Word;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::mmr::{Forest, MmrPeaks, PartialMmr};
use miden_protocol::protocol_config::ProtocolConfig;
use miden_protocol::transaction::{InputNote, InputNotes, PartialBlockchain};

/// Everything an attempt reads from the chain, fixed at one reference block.
#[derive(Debug, Clone)]
pub struct ChainView {
    genesis_commitment: Word,
    reference_header: BlockHeader,
    protocol_config: ProtocolConfig,
    blockchain: PartialBlockchain,
}

impl ChainView {
    pub fn genesis_commitment(&self) -> Word {
        self.genesis_commitment
    }

    pub fn reference_header(&self) -> &BlockHeader {
        &self.reference_header
    }

    pub fn reference_block(&self) -> BlockNumber {
        self.reference_header.block_num()
    }

    pub fn protocol_config(&self) -> &ProtocolConfig {
        &self.protocol_config
    }

    pub fn blockchain(&self) -> &PartialBlockchain {
        &self.blockchain
    }

    /// Whether the executor can authenticate `block` against the reference block.
    pub fn authenticates(&self, block: BlockNumber) -> bool {
        block == self.reference_block() || self.blockchain.contains_block(block)
    }
}

/// Why a chain view could not be assembled.
#[derive(Debug, thiserror::Error)]
pub enum ChainViewError {
    /// The node has not reached a block the transaction must authenticate. Retryable once the
    /// node catches up.
    #[error("node tip {tip} is behind required block {required}")]
    ChainBehind {
        tip: BlockNumber,
        required: BlockNumber,
    },
    /// The node served chain data that does not authenticate against itself.
    #[error("inconsistent chain data: {0}")]
    Inconsistent(String),
    #[error("chain read failed: {0}")]
    Rpc(#[from] RpcError),
}

/// The blocks a request's execution must authenticate: the blocks the request declares (its
/// bound block among them) and the creation block of every authenticated input note.
pub fn blocks_to_track(
    request: &TransactionRequest,
    input_notes: &InputNotes<InputNote>,
) -> BTreeSet<BlockNumber> {
    request
        .block_numbers()
        .iter()
        .copied()
        .chain(
            input_notes
                .iter()
                .filter_map(|note| note.location().map(|location| location.block_num())),
        )
        .collect()
}

/// Builds the chain view at the node's committed tip, tracking `tracked` blocks.
pub async fn build_chain_view(
    rpc: &dyn NodeRpcClient,
    tracked: &BTreeSet<BlockNumber>,
) -> Result<ChainView, ChainViewError> {
    let (genesis, _) = rpc
        .get_block_header_by_number(Some(BlockNumber::GENESIS), false)
        .await?;
    if genesis.block_num() != BlockNumber::GENESIS {
        return Err(ChainViewError::Inconsistent(format!(
            "node answered the genesis header request with block {}",
            genesis.block_num()
        )));
    }

    let mut partial_mmr = seeded_at_genesis(&genesis)?;
    let sync = rpc
        .sync_chain_mmr(BlockNumber::GENESIS, SyncTarget::CommittedChainTip)
        .await?;
    partial_mmr
        .apply(sync.mmr_delta)
        .map_err(|e| ChainViewError::Inconsistent(format!("MMR delta does not apply: {e}")))?;
    let reference_header = sync.block_header;
    let protocol_config = sync.protocol_config.ok_or_else(|| {
        ChainViewError::Inconsistent(
            "a genesis-seeded chain sync carried no protocol configuration".to_string(),
        )
    })?;
    ensure_peaks_match(&partial_mmr, &reference_header)?;
    if protocol_config.to_commitment() != reference_header.protocol_config_commitment() {
        return Err(ChainViewError::Inconsistent(format!(
            "protocol configuration does not match block {}",
            reference_header.block_num()
        )));
    }

    let reference = reference_header.block_num();
    if let Some(required) = tracked.iter().copied().find(|block| *block > reference) {
        return Err(ChainViewError::ChainBehind {
            tip: reference,
            required,
        });
    }

    let mut headers = BTreeMap::new();
    for block in tracked.iter().copied().filter(|block| *block < reference) {
        let (header, proof) = rpc.get_block_header_with_proof(block).await?;
        if header.block_num() != block {
            return Err(ChainViewError::Inconsistent(format!(
                "node answered the header request for block {block} with block {}",
                header.block_num()
            )));
        }
        let proof = proof.with_forest(partial_mmr.forest()).map_err(|e| {
            ChainViewError::Inconsistent(format!(
                "MMR path for block {block} does not fit forest {}: {e}",
                partial_mmr.forest().num_leaves()
            ))
        })?;
        partial_mmr
            .track(block.as_usize(), header.commitment(), proof.merkle_path())
            .map_err(|e| {
                ChainViewError::Inconsistent(format!(
                    "block {block} does not authenticate against block {reference}: {e}"
                ))
            })?;
        headers.insert(block, header);
    }

    let blockchain = PartialBlockchain::new(partial_mmr, headers.into_values())
        .map_err(|e| ChainViewError::Inconsistent(format!("partial blockchain: {e}")))?;

    Ok(ChainView {
        genesis_commitment: genesis.commitment(),
        reference_header,
        protocol_config,
        blockchain,
    })
}

/// A `SyncChainMmr` from height 0 assumes genesis is already present, so the partial MMR starts
/// as the one-leaf forest whose only peak is the genesis commitment.
fn seeded_at_genesis(genesis: &BlockHeader) -> Result<PartialMmr, ChainViewError> {
    let forest = Forest::new(1).expect("a one-leaf forest is valid");
    MmrPeaks::new(forest, vec![genesis.commitment()])
        .map(PartialMmr::from_peaks)
        .map_err(|e| ChainViewError::Inconsistent(format!("genesis peaks: {e}")))
}

/// The assembled peaks are usable only if they hash to the chain commitment the reference
/// header carries.
fn ensure_peaks_match(
    partial_mmr: &PartialMmr,
    reference_header: &BlockHeader,
) -> Result<(), ChainViewError> {
    let derived = partial_mmr.peaks().hash_peaks();
    if derived != reference_header.chain_commitment() {
        return Err(ChainViewError::Inconsistent(format!(
            "chain MMR peaks hash to {derived}, block {} commits to {}",
            reference_header.block_num(),
            reference_header.chain_commitment()
        )));
    }
    Ok(())
}
