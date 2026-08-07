//! Assembles the [`PartialBlockchain`] an execution needs, from node RPC only.
//!
//! GUARDIAN keeps no synced chain state. Each execution acquires the chain MMR from a cold
//! start, which is affordable because `SyncChainMmr`'s payload is the peak set — logarithmic
//! in chain length, not proportional to it.
//!
//! The reference block is always the committed tip, so it needs no authentication path of its
//! own. Only blocks that input notes were created in need paths, which is why transactions
//! that consume no notes need nothing beyond the peaks.

use std::collections::{BTreeMap, BTreeSet};

use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::merkle::MerklePath;
use miden_protocol::crypto::merkle::mmr::{Forest, MmrDelta, MmrPeaks, PartialMmr};
use miden_protocol::transaction::{InputNote, InputNotes, PartialBlockchain};
use miden_rpc_client::MidenRpcClient;

/// Genesis is the single leaf a cold-start partial MMR is seeded with, so the delta the node
/// returns for `current_client_block_height = 0` applies cleanly on top of it.
const GENESIS_BLOCK: u32 = 0;
/// Miden node 0.15's `QueryParamNoteTagLimit`; one transaction may contain up to 1,024 notes.
const MAX_SYNC_NOTES_TAGS: usize = 1_000;

/// A chain view sufficient to execute one transaction: the reference block plus proofs that
/// every note block it depends on belongs to the same chain.
pub struct ChainView {
    pub reference_block: BlockHeader,
    pub blockchain: PartialBlockchain,
}

/// Builds the chain view for an execution.
///
/// Historical note paths are derived only from authenticated input notes. Transactions with no
/// input notes, and transactions whose input notes are all unauthenticated, skip `SyncNotes`
/// entirely. The caller must therefore pass the prepared [`InputNotes`], not infer this from the
/// serialized `TransactionRequest` alone: a request does not encode which notes the executor has
/// authenticated inclusion data for.
///
/// Every proof is anchored to the same forest and the same reference tip: the partial MMR is
/// advanced to the committed tip once, and each note block's path is tracked against that
/// single forest. Mixing forests would produce paths that do not verify.
pub async fn build_chain_view(
    rpc: &mut MidenRpcClient,
    input_notes: &InputNotes<InputNote>,
) -> Result<ChainView, String> {
    let (mut partial_mmr, reference_block) = acquire_chain_mmr(rpc).await?;

    let tracked = track_authenticated_note_blocks(
        rpc,
        &mut partial_mmr,
        input_notes,
        reference_block.block_num(),
    )
    .await?;

    let blockchain = PartialBlockchain::new(partial_mmr, tracked.into_values().collect::<Vec<_>>())
        .map_err(|e| format!("failed to build partial blockchain: {e}"))?;

    Ok(ChainView {
        reference_block,
        blockchain,
    })
}

/// Seeds a partial MMR at genesis, advances it to the committed tip, and checks the result
/// against the tip header's own chain commitment.
pub(super) async fn acquire_chain_mmr(
    rpc: &mut MidenRpcClient,
) -> Result<(PartialMmr, BlockHeader), String> {
    let genesis = fetch_block_header(rpc, Some(GENESIS_BLOCK), false).await?.0;

    // `current_client_block_height = 0` asserts genesis is already present, so the partial MMR
    // must start as a one-leaf forest whose only peak is the genesis commitment. Applying the
    // delta to an empty MMR would leave the forests misaligned.
    let genesis_forest = Forest::new(1).map_err(|e| format!("invalid genesis forest: {e}"))?;
    let seed = MmrPeaks::new(genesis_forest, vec![genesis.commitment()])
        .map_err(|e| format!("failed to seed MMR peaks at genesis: {e}"))?;
    let mut partial_mmr = PartialMmr::from_peaks(seed);

    let response = rpc.sync_chain_mmr(GENESIS_BLOCK).await?;

    let reference_block: BlockHeader = response
        .block_header
        .ok_or_else(|| "SyncChainMmr returned no sync-target block header".to_string())?
        .try_into()
        .map_err(|e| format!("invalid sync-target block header: {e}"))?;

    let delta: MmrDelta = response
        .mmr_delta
        .ok_or_else(|| "SyncChainMmr returned no MMR delta".to_string())?
        .try_into()
        .map_err(|e| format!("invalid MMR delta: {e}"))?;

    partial_mmr
        .apply(delta)
        .map_err(|e| format!("failed to apply MMR delta: {e}"))?;

    verify_against_reference(&partial_mmr, &reference_block)?;

    Ok((partial_mmr, reference_block))
}

/// Checks that an assembled chain MMR actually corresponds to the reference block.
///
/// This is the correctness gate for the whole assembly: peaks are only usable if they hash to
/// the commitment the reference block itself carries. Without it the executor fails deep inside
/// the kernel with an opaque error; here it fails at the boundary naming the cause.
pub fn verify_against_reference(
    partial_mmr: &PartialMmr,
    reference_block: &BlockHeader,
) -> Result<(), String> {
    let derived = partial_mmr.peaks().hash_peaks();
    let expected = reference_block.chain_commitment();
    if derived != expected {
        return Err(format!(
            "chain MMR does not match the reference block: peaks hash to {derived}, \
             block {} commits to {expected}",
            reference_block.block_num()
        ));
    }
    Ok(())
}

/// Fetches authenticated note blocks through `SyncNotes`, whose explicit upper bound pins every
/// returned path to the execution forest. For reference block `N`, `block_to` must be `N - 1`:
/// `SyncNotes` opens paths at forest `block_to + 1`, while the reference header commits to the
/// `N`-leaf forest containing blocks `0..N-1`.
async fn track_authenticated_note_blocks(
    rpc: &mut MidenRpcClient,
    partial_mmr: &mut PartialMmr,
    input_notes: &InputNotes<InputNote>,
    reference_block: BlockNumber,
) -> Result<BTreeMap<BlockNumber, BlockHeader>, String> {
    let note_blocks_by_tag = authenticated_note_query(input_notes);

    if note_blocks_by_tag.is_empty() {
        return Ok(BTreeMap::new());
    }

    let block_to = reference_block.as_u32().checked_sub(1).ok_or_else(|| {
        "cannot authenticate input notes against the genesis reference block".to_string()
    })?;
    if note_blocks_by_tag
        .values()
        .flatten()
        .any(|block| block.as_u32() > block_to)
    {
        return Err(format!(
            "authenticated input note block must precede reference block {}",
            reference_block.as_u32()
        ));
    }

    let mut tracked = BTreeMap::new();
    let note_tags: Vec<u32> = note_blocks_by_tag.keys().copied().collect();
    for note_tag_chunk in note_tags.chunks(MAX_SYNC_NOTES_TAGS) {
        let mut expected_blocks: BTreeSet<BlockNumber> = note_tag_chunk
            .iter()
            .flat_map(|tag| {
                note_blocks_by_tag
                    .get(tag)
                    .expect("tag came from the note-block map")
                    .iter()
                    .copied()
            })
            .collect();
        let mut block_from = expected_blocks
            .first()
            .expect("every authenticated note tag has a block")
            .as_u32();

        loop {
            let response = rpc
                .sync_notes(block_from, block_to, note_tag_chunk.to_vec())
                .await?;
            let page = response
                .pagination_info
                .ok_or_else(|| "SyncNotes returned no pagination information".to_string())?;
            if page.block_num < block_from || page.block_num > block_to {
                return Err(format!(
                    "SyncNotes page ended outside the requested range: from={block_from}, \
                     to={block_to}, end={}",
                    page.block_num
                ));
            }

            for block in response.blocks {
                let header: BlockHeader = block
                    .block_header
                    .ok_or_else(|| "SyncNotes returned a note block without a header".to_string())?
                    .try_into()
                    .map_err(|e| format!("invalid SyncNotes block header: {e}"))?;
                if !expected_blocks.remove(&header.block_num())
                    || tracked.contains_key(&header.block_num())
                {
                    continue;
                }
                let path: MerklePath = block
                    .mmr_path
                    .ok_or_else(|| {
                        format!(
                            "SyncNotes returned no MMR path for note block {}",
                            header.block_num().as_u32()
                        )
                    })?
                    .try_into()
                    .map_err(|e| format!("invalid SyncNotes MMR path: {e}"))?;

                partial_mmr
                    .track(header.block_num().as_usize(), header.commitment(), &path)
                    .map_err(|e| {
                        format!(
                            "failed to track note block {} against reference block {}: {e}",
                            header.block_num().as_u32(),
                            reference_block.as_u32()
                        )
                    })?;
                tracked.insert(header.block_num(), header);
            }

            if expected_blocks.is_empty() {
                break;
            }
            if page.block_num == block_to {
                let missing = expected_blocks
                    .iter()
                    .map(|block| block.as_u32().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "SyncNotes did not return authenticated input note blocks: {missing}"
                ));
            }

            block_from = page.block_num + 1;
        }
    }

    Ok(tracked)
}

pub(super) fn authenticated_note_query(
    input_notes: &InputNotes<InputNote>,
) -> BTreeMap<u32, BTreeSet<BlockNumber>> {
    let mut query = BTreeMap::<u32, BTreeSet<BlockNumber>>::new();
    for input_note in input_notes {
        if let Some(location) = input_note.location() {
            query
                .entry(input_note.note().metadata().tag().as_u32())
                .or_default()
                .insert(location.block_num());
        }
    }
    query
}

async fn fetch_block_header(
    rpc: &mut MidenRpcClient,
    block_num: Option<u32>,
    include_mmr_proof: bool,
) -> Result<(BlockHeader, Option<MerklePath>), String> {
    let response = rpc.get_block_header(block_num, include_mmr_proof).await?;

    let header: BlockHeader = response
        .block_header
        .ok_or_else(|| format!("node returned no header for block {block_num:?}"))?
        .try_into()
        .map_err(|e| format!("invalid block header for {block_num:?}: {e}"))?;

    let path = match response.mmr_path {
        Some(path) => Some(
            MerklePath::try_from(path)
                .map_err(|e| format!("invalid MMR path for block {block_num:?}: {e}"))?,
        ),
        None => None,
    };

    Ok((header, path))
}
