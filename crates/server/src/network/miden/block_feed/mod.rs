//! The committed-block feed of a Miden node, for block watch
//! (`jobs::block_watch`, issue #585): a `BlockSubscription` stream source,
//! a `GetBlockByNumber` polling source, and the factory that opens them.
//! Everything here is Miden-specific; block watch itself only sees
//! [`ObservedBlock`]s through the network-agnostic [`BlockSource`] traits.

mod poll;
mod stream;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::OnceCell;

use miden_rpc_client::{
    MidenRpcClient, RpcClientError, RpcClientSettings, RpcReadMode, blockchain,
};

use crate::jobs::block_watch::{
    AccountUpdate, BlockSource, BlockSourceError, BlockSourceFactory, ObservedBlock,
};
use crate::secret::CredentialUrl;
use poll::PollSource;
use stream::StreamSource;

impl From<RpcClientError> for BlockSourceError {
    fn from(error: RpcClientError) -> Self {
        match error {
            RpcClientError::Call { status, .. } => Self::Status {
                code: status.code(),
                message: status.message().to_string(),
            },
            RpcClientError::MalformedResponse { reason, .. } => Self::Malformed(reason),
            other => Self::Transport(other.to_string()),
        }
    }
}

/// Reduce a committed block to an [`ObservedBlock`].
pub(crate) fn observed_block(
    block: &blockchain::SignedBlock,
    committed_chain_tip: u32,
) -> Result<ObservedBlock, BlockSourceError> {
    let decoded = miden_rpc_client::block_account_updates(block)
        .map_err(|e| BlockSourceError::Malformed(e.to_string()))?;
    Ok(ObservedBlock {
        block_num: decoded.block_num,
        committed_chain_tip: committed_chain_tip.max(decoded.block_num),
        updates: decoded
            .updates
            .into_iter()
            .map(|update| AccountUpdate {
                account_id: update.account_id.to_hex(),
                commitment: update.commitment,
                is_public: update.is_public,
            })
            .collect(),
        undecodable_updates: decoded.undecodable.len(),
    })
}

/// Opens stream and poll sources against one node endpoint. The unary
/// client (polls and tip reads) is connected on first use and shared;
/// every stream gets a channel of its own.
pub struct MidenBlockSourceFactory {
    endpoint: CredentialUrl,
    settings: RpcClientSettings,
    poll_interval: Duration,
    unary: OnceCell<Arc<MidenRpcClient>>,
}

impl MidenBlockSourceFactory {
    pub(crate) fn new(
        endpoint: CredentialUrl,
        settings: RpcClientSettings,
        poll_interval: Duration,
    ) -> Self {
        Self {
            endpoint,
            settings,
            poll_interval,
            unary: OnceCell::new(),
        }
    }

    async fn unary(&self) -> Result<Arc<MidenRpcClient>, BlockSourceError> {
        self.unary
            .get_or_try_init(|| async {
                MidenRpcClient::connect_with_settings(
                    self.endpoint.expose_secret(),
                    self.settings.clone(),
                )
                .await
                .map(Arc::new)
                .map_err(BlockSourceError::from)
            })
            .await
            .cloned()
    }
}

#[async_trait]
impl BlockSourceFactory for MidenBlockSourceFactory {
    async fn open_stream(&self, from: u32) -> Result<Box<dyn BlockSource>, BlockSourceError> {
        Ok(Box::new(
            StreamSource::open(self.endpoint.expose_secret(), from, self.settings.timeout())
                .await?,
        ))
    }

    async fn open_poll(&self, from: u32) -> Result<Box<dyn BlockSource>, BlockSourceError> {
        Ok(Box::new(PollSource::new(
            self.unary().await?,
            from,
            self.poll_interval,
        )))
    }

    async fn chain_tip(&self) -> Result<u32, BlockSourceError> {
        let client = self.unary().await?;
        let started = std::time::Instant::now();
        let tip = client.get_chain_tip(RpcReadMode::SingleAttempt).await;
        crate::network::miden::record_rpc("get_chain_tip", started, tip.is_ok());
        Ok(tip?)
    }

    async fn genesis(&self) -> Result<Option<String>, BlockSourceError> {
        let client = self.unary().await?;
        let started = std::time::Instant::now();
        let status = client.get_status().await;
        crate::network::miden::record_rpc("get_status", started, status.is_ok());
        Ok(status?
            .genesis_commitment
            .as_ref()
            .and_then(miden_rpc_client::word_to_hex))
    }

    async fn serves_block(&self, block_num: u32) -> Result<bool, BlockSourceError> {
        let client = self.unary().await?;
        let started = std::time::Instant::now();
        let block = client
            .get_block(block_num, RpcReadMode::SingleAttempt)
            .await;
        crate::network::miden::record_rpc("get_block", started, block.is_ok());
        Ok(block?.is_some())
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use miden_protocol::account::AccountId;
    use miden_rpc_client::test_node;

    #[test]
    fn a_block_becomes_its_account_updates_with_canonical_ids() {
        let public = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        let private = AccountId::from_hex("0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d").unwrap();
        let block = test_node::block(
            9,
            vec![
                test_node::account_update(public, [0xab; 32], true),
                test_node::account_update(private, [0xcd; 32], false),
            ],
        );
        let observed = observed_block(&block, 12).unwrap();
        assert_eq!(observed.block_num, 9);
        assert_eq!(observed.committed_chain_tip, 12);
        assert_eq!(
            observed.updates,
            vec![
                AccountUpdate {
                    account_id: "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b".into(),
                    commitment: format!("0x{}", "ab".repeat(32)),
                    is_public: true,
                },
                AccountUpdate {
                    account_id: "0x1d1d1d1c1d1d1d011d1d1d1d1d1d1d".into(),
                    commitment: format!("0x{}", "cd".repeat(32)),
                    is_public: false,
                },
            ]
        );
        // A tip behind the block (a node that reported it before
        // committing) never makes the lag negative.
        assert_eq!(observed_block(&block, 3).unwrap().committed_chain_tip, 9);

        assert_eq!(observed.undecodable_updates, 0);
        let mut broken = block.clone();
        broken.body = None;
        assert!(matches!(
            observed_block(&broken, 9),
            Err(BlockSourceError::Malformed(_))
        ));
        // An entry that cannot be read is counted; the rest is kept.
        let mut partly = block.clone();
        partly.body.as_mut().unwrap().updated_accounts[0].account_id = None;
        let observed = observed_block(&partly, 9).expect("the rest is read");
        assert_eq!(observed.updates.len(), 1);
        assert_eq!(observed.updates[0].account_id, private.to_hex());
        assert_eq!(observed.undecodable_updates, 1);
    }
}
