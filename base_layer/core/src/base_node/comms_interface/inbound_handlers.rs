// Copyright 2019. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use std::{
    convert::{TryFrom, TryInto},
    sync::Arc,
    time::{Duration, Instant},
};

use log::*;
use strum_macros::Display;
use tari_common_types::types::{BlockHash, HashOutput};
use tari_comms::{connectivity::ConnectivityRequester, peer_manager::NodeId};
use tari_utilities::hex::Hex;
use tokio::sync::Semaphore;

use crate::{
    base_node::{
        comms_interface::{
            error::CommsInterfaceError,
            local_interface::BlockEventSender,
            FetchMempoolTransactionsResponse,
            NodeCommsRequest,
            NodeCommsResponse,
            OutboundNodeCommsInterface,
        },
        metrics,
    },
    blocks::{Block, BlockBuilder, BlockHeader, ChainBlock, NewBlock, NewBlockTemplate},
    chain_storage::{async_db::AsyncBlockchainDb, BlockAddResult, BlockchainBackend, ChainStorageError, PrunedOutput},
    consensus::{ConsensusConstants, ConsensusManager},
    mempool::Mempool,
    proof_of_work::{Difficulty, PowAlgorithm},
    transactions::aggregated_body::AggregateBody,
    validation::helpers,
};

const LOG_TARGET: &str = "c::bn::comms_interface::inbound_handler";
const MAX_REQUEST_BY_BLOCK_HASHES: usize = 100;
const MAX_REQUEST_BY_KERNEL_EXCESS_SIGS: usize = 100;
const MAX_REQUEST_BY_UTXO_HASHES: usize = 100;

/// Events that can be published on the Validated Block Event Stream
/// Broadcast is to notify subscribers if this is a valid propagated block event
#[derive(Debug, Clone, Display)]
pub enum BlockEvent {
    ValidBlockAdded(Arc<Block>, BlockAddResult),
    AddBlockValidationFailed {
        block: Arc<Block>,
        source_peer: Option<NodeId>,
    },
    AddBlockErrored {
        block: Arc<Block>,
    },
    BlockSyncComplete(Arc<ChainBlock>, u64),
    BlockSyncRewind(Vec<Arc<ChainBlock>>),
}

/// The InboundNodeCommsInterface is used to handle all received inbound requests from remote nodes.
pub struct InboundNodeCommsHandlers<B> {
    block_event_sender: BlockEventSender,
    blockchain_db: AsyncBlockchainDb<B>,
    mempool: Mempool,
    consensus_manager: ConsensusManager,
    new_block_request_semaphore: Arc<Semaphore>,
    outbound_nci: OutboundNodeCommsInterface,
    connectivity: ConnectivityRequester,
}

impl<B> InboundNodeCommsHandlers<B>
where B: BlockchainBackend + 'static
{
    /// Construct a new InboundNodeCommsInterface.
    pub fn new(
        block_event_sender: BlockEventSender,
        blockchain_db: AsyncBlockchainDb<B>,
        mempool: Mempool,
        consensus_manager: ConsensusManager,
        outbound_nci: OutboundNodeCommsInterface,
        connectivity: ConnectivityRequester,
    ) -> Self {
        Self {
            block_event_sender,
            blockchain_db,
            mempool,
            consensus_manager,
            new_block_request_semaphore: Arc::new(Semaphore::new(1)),
            outbound_nci,
            connectivity,
        }
    }

    /// Handle inbound node comms requests from remote nodes and local services.
    // TODO: Break this function up into smaller pieces
    #[allow(clippy::too_many_lines)]
    pub async fn handle_request(&self, request: NodeCommsRequest) -> Result<NodeCommsResponse, CommsInterfaceError> {
        debug!(target: LOG_TARGET, "Handling remote request {}", request);
        match request {
            NodeCommsRequest::GetChainMetadata => Ok(NodeCommsResponse::ChainMetadata(
                self.blockchain_db.get_chain_metadata().await?,
            )),
            NodeCommsRequest::FetchHeaders(range) => {
                let headers = self.blockchain_db.fetch_chain_headers(range).await?;
                Ok(NodeCommsResponse::BlockHeaders(headers))
            },
            NodeCommsRequest::FetchHeadersByHashes(block_hashes) => {
                if block_hashes.len() > MAX_REQUEST_BY_BLOCK_HASHES {
                    return Err(CommsInterfaceError::InvalidRequest {
                        request: "FetchHeadersByHashes",
                        details: format!(
                            "Exceeded maximum block hashes request (max: {}, got:{})",
                            MAX_REQUEST_BY_BLOCK_HASHES,
                            block_hashes.len()
                        ),
                    });
                }
                let mut block_headers = Vec::with_capacity(block_hashes.len());
                for block_hash in block_hashes {
                    let block_hex = block_hash.to_hex();
                    match self.blockchain_db.fetch_chain_header_by_block_hash(block_hash).await? {
                        Some(block_header) => {
                            block_headers.push(block_header);
                        },
                        None => {
                            error!(target: LOG_TARGET, "Could not fetch headers with hashes:{}", block_hex);
                            return Err(CommsInterfaceError::InternalError(format!(
                                "Could not fetch headers with hashes:{}",
                                block_hex
                            )));
                        },
                    }
                }
                Ok(NodeCommsResponse::BlockHeaders(block_headers))
            },
            NodeCommsRequest::FetchMatchingUtxos(utxo_hashes) => {
                let mut res = Vec::with_capacity(utxo_hashes.len());
                for (pruned_output, spent) in (self.blockchain_db.fetch_utxos(utxo_hashes).await?)
                    .into_iter()
                    .flatten()
                {
                    if let PrunedOutput::NotPruned { output } = pruned_output {
                        if !spent {
                            res.push(output);
                        }
                    }
                }
                Ok(NodeCommsResponse::TransactionOutputs(res))
            },
            NodeCommsRequest::FetchMatchingBlocks { range, compact } => {
                let blocks = self.blockchain_db.fetch_blocks(range, compact).await?;
                Ok(NodeCommsResponse::HistoricalBlocks(blocks))
            },
            NodeCommsRequest::FetchBlocksByKernelExcessSigs(excess_sigs) => {
                if excess_sigs.len() > MAX_REQUEST_BY_KERNEL_EXCESS_SIGS {
                    return Err(CommsInterfaceError::InvalidRequest {
                        request: "FetchBlocksByKernelExcessSigs",
                        details: format!(
                            "Exceeded maximum number of kernel excess sigs in request (max: {}, got:{})",
                            MAX_REQUEST_BY_KERNEL_EXCESS_SIGS,
                            excess_sigs.len()
                        ),
                    });
                }
                let mut blocks = Vec::with_capacity(excess_sigs.len());
                for sig in excess_sigs {
                    let sig_hex = sig.get_signature().to_hex();
                    debug!(
                        target: LOG_TARGET,
                        "A peer has requested a block with kernel with sig {}", sig_hex
                    );
                    match self.blockchain_db.fetch_block_with_kernel(sig).await {
                        Ok(Some(block)) => blocks.push(block),
                        Ok(None) => warn!(
                            target: LOG_TARGET,
                            "Could not provide requested block containing kernel with sig {} to peer because not \
                             stored",
                            sig_hex
                        ),
                        Err(e) => warn!(
                            target: LOG_TARGET,
                            "Could not provide requested block containing kernel with sig {} to peer because: {}",
                            sig_hex,
                            e.to_string()
                        ),
                    }
                }
                Ok(NodeCommsResponse::HistoricalBlocks(blocks))
            },
            NodeCommsRequest::FetchBlocksByUtxos(commitments) => {
                if commitments.len() > MAX_REQUEST_BY_UTXO_HASHES {
                    return Err(CommsInterfaceError::InvalidRequest {
                        request: "FetchBlocksByUtxos",
                        details: format!(
                            "Exceeded maximum number of utxo hashes in request (max: {}, got:{})",
                            MAX_REQUEST_BY_UTXO_HASHES,
                            commitments.len()
                        ),
                    });
                }
                let mut blocks = Vec::with_capacity(commitments.len());
                for commitment in commitments {
                    let commitment_hex = commitment.to_hex();
                    debug!(
                        target: LOG_TARGET,
                        "A peer has requested a block with commitment {}", commitment_hex,
                    );
                    match self.blockchain_db.fetch_block_with_utxo(commitment).await {
                        Ok(Some(block)) => blocks.push(block),
                        Ok(None) => warn!(
                            target: LOG_TARGET,
                            "Could not provide requested block with commitment {} to peer because not stored",
                            commitment_hex,
                        ),
                        Err(e) => warn!(
                            target: LOG_TARGET,
                            "Could not provide requested block with commitment {} to peer because: {}",
                            commitment_hex,
                            e.to_string()
                        ),
                    }
                }
                Ok(NodeCommsResponse::HistoricalBlocks(blocks))
            },
            NodeCommsRequest::GetHeaderByHash(hash) => {
                let header = self.blockchain_db.fetch_chain_header_by_block_hash(hash).await?;
                Ok(NodeCommsResponse::BlockHeader(header))
            },
            NodeCommsRequest::GetBlockByHash(hash) => {
                let block = self.blockchain_db.fetch_block_by_hash(hash, false).await?;
                Ok(NodeCommsResponse::HistoricalBlock(Box::new(block)))
            },
            NodeCommsRequest::GetNewBlockTemplate(request) => {
                let best_block_header = self.blockchain_db.fetch_tip_header().await?;
                let mut header = BlockHeader::from_previous(best_block_header.header());
                let constants = self.consensus_manager.consensus_constants(header.height);
                header.version = constants.blockchain_version();
                header.pow.pow_algo = request.algo;

                let constants_weight = constants.get_max_block_weight_excluding_coinbase();
                let asking_weight = if request.max_weight > constants_weight || request.max_weight == 0 {
                    constants_weight
                } else {
                    request.max_weight
                };

                debug!(
                    target: LOG_TARGET,
                    "Fetching transactions with a maximum weight of {} for the template", asking_weight
                );
                let transactions = self
                    .mempool
                    .retrieve(asking_weight)
                    .await?
                    .into_iter()
                    .map(|tx| Arc::try_unwrap(tx).unwrap_or_else(|tx| (*tx).clone()))
                    .collect::<Vec<_>>();

                debug!(
                    target: LOG_TARGET,
                    "Adding {} transaction(s) to new block template",
                    transactions.len(),
                );

                let prev_hash = header.prev_hash;
                let height = header.height;

                let block_template = NewBlockTemplate::from_block(
                    header.into_builder().with_transactions(transactions).build(),
                    self.get_target_difficulty_for_next_block(request.algo, constants, prev_hash)
                        .await?,
                    self.consensus_manager.get_block_reward_at(height),
                );

                debug!(target: LOG_TARGET, "New template block: {}", block_template);
                debug!(
                    target: LOG_TARGET,
                    "New block template requested at height {}, weight: {}",
                    block_template.header.height,
                    block_template.body.calculate_weight(constants.transaction_weight())
                );
                trace!(target: LOG_TARGET, "{}", block_template);
                Ok(NodeCommsResponse::NewBlockTemplate(block_template))
            },
            NodeCommsRequest::GetNewBlock(block_template) => {
                debug!(target: LOG_TARGET, "Prepared block: {}", block_template);
                let block = self.blockchain_db.prepare_new_block(block_template).await?;
                let constants = self.consensus_manager.consensus_constants(block.header.height);
                debug!(
                    target: LOG_TARGET,
                    "Prepared new block from template (hash: {}, weight: {}, {})",
                    block.hash().to_hex(),
                    block.body.calculate_weight(constants.transaction_weight()),
                    block.body.to_counts_string()
                );
                Ok(NodeCommsResponse::NewBlock {
                    success: true,
                    error: None,
                    block: Some(block),
                })
            },
            NodeCommsRequest::GetBlockFromAllChains(hash) => {
                let block_hex = hash.to_hex();
                debug!(
                    target: LOG_TARGET,
                    "A peer has requested a block with hash {}", block_hex
                );

                let maybe_block = match self
                    .blockchain_db
                    .fetch_block_by_hash(hash, true)
                    .await
                    .unwrap_or_else(|e| {
                        warn!(
                            target: LOG_TARGET,
                            "Could not provide requested block {} to peer because: {}",
                            block_hex,
                            e.to_string()
                        );

                        None
                    }) {
                    None => self.blockchain_db.fetch_orphan(hash).await.map_or_else(
                        |e| {
                            warn!(
                                target: LOG_TARGET,
                                "Could not provide requested block {} to peer because: {}", block_hex, e,
                            );

                            None
                        },
                        Some,
                    ),
                    Some(block) => Some(block.try_into_block()?),
                };

                Ok(NodeCommsResponse::Block(Box::new(maybe_block)))
            },
            NodeCommsRequest::FetchKernelByExcessSig(signature) => {
                let kernels = match self.blockchain_db.fetch_kernel_by_excess_sig(signature).await {
                    Ok(Some((kernel, _))) => vec![kernel],
                    Ok(None) => vec![],
                    Err(err) => {
                        error!(target: LOG_TARGET, "Could not fetch kernel {}", err.to_string());
                        return Err(err.into());
                    },
                };

                Ok(NodeCommsResponse::TransactionKernels(kernels))
            },
            NodeCommsRequest::FetchMempoolTransactionsByExcessSigs { excess_sigs } => {
                let (transactions, not_found) = self.mempool.retrieve_by_excess_sigs(excess_sigs).await?;
                Ok(NodeCommsResponse::FetchMempoolTransactionsByExcessSigsResponse(
                    FetchMempoolTransactionsResponse {
                        transactions,
                        not_found,
                    },
                ))
            },
            NodeCommsRequest::FetchValidatorNodesKeys { height } => {
                let active_validator_nodes = self.blockchain_db.fetch_active_validator_nodes(height).await?;
                Ok(NodeCommsResponse::FetchValidatorNodesKeysResponse(
                    active_validator_nodes,
                ))
            },
            NodeCommsRequest::GetShardKey { height, public_key } => {
                let shard_key = self.blockchain_db.get_shard_key(height, public_key).await?;
                Ok(NodeCommsResponse::GetShardKeyResponse(shard_key))
            },
            NodeCommsRequest::FetchTemplateRegistrations {
                start_height,
                end_height,
            } => {
                let template_registrations = self
                    .blockchain_db
                    .fetch_template_registrations(start_height..=end_height)
                    .await?;
                Ok(NodeCommsResponse::FetchTemplateRegistrationsResponse(
                    template_registrations,
                ))
            },
            NodeCommsRequest::FetchUnspentUtxosInBlock { block_hash } => {
                let utxos = self.blockchain_db.fetch_outputs_in_block(block_hash).await?;
                Ok(NodeCommsResponse::TransactionOutputs(
                    utxos
                        .into_iter()
                        .filter_map(|utxo| utxo.into_unpruned_output())
                        .collect(),
                ))
            },
        }
    }

    /// Handles a `NewBlock` message. Only a single `NewBlock` message can be handled at once to prevent extraneous
    /// requests for the full block.
    /// This may (asynchronously) block until the other request(s) complete or time out and so should typically be
    /// executed in a dedicated task.
    pub async fn handle_new_block_message(
        &mut self,
        new_block: NewBlock,
        source_peer: NodeId,
    ) -> Result<(), CommsInterfaceError> {
        let block_hash = new_block.header.hash();

        if self.blockchain_db.inner().is_add_block_disabled() {
            info!(
                target: LOG_TARGET,
                "Ignoring block message ({}) because add_block is locked",
                block_hash.to_hex()
            );
            return Ok(());
        }

        // Only a single block request can complete at a time.
        // As multiple NewBlock requests arrive from propagation, this semaphore prevents multiple requests to nodes for
        // the same full block. The first request that succeeds will stop the node from requesting the block from any
        // other node (block_exists is true).
        // Arc clone to satisfy the borrow checker
        let semaphore = self.new_block_request_semaphore.clone();
        let _permit = semaphore.acquire().await.unwrap();

        if self.blockchain_db.block_exists(block_hash).await? {
            debug!(
                target: LOG_TARGET,
                "Block with hash `{}` already stored",
                block_hash.to_hex()
            );
            return Ok(());
        }

        debug!(
            target: LOG_TARGET,
            "Block with hash `{}` is unknown. Constructing block from known mempool transactions / requesting missing \
             transactions from peer '{}'.",
            block_hash.to_hex(),
            source_peer
        );

        let block = self.reconcile_block(source_peer.clone(), new_block).await?;
        self.handle_block(block, Some(source_peer)).await?;

        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn reconcile_block(
        &mut self,
        source_peer: NodeId,
        new_block: NewBlock,
    ) -> Result<Block, CommsInterfaceError> {
        let NewBlock {
            header,
            coinbase_kernel,
            coinbase_output,
            kernel_excess_sigs: excess_sigs,
        } = new_block;
        // If the block is empty, we dont have to check ask for the block, as we already have the full block available
        // to us.
        if excess_sigs.is_empty() {
            let block = BlockBuilder::new(header.version)
                .with_coinbase_utxo(coinbase_output, coinbase_kernel)
                .with_header(header)
                .build();
            return Ok(block);
        }

        let block_hash = header.hash();
        // We check the current tip and orphan status of the block because we cannot guarantee that mempool state is
        // correct and the mmr root calculation is only valid if the block is building on the tip.
        let current_meta = self.blockchain_db.get_chain_metadata().await?;
        if header.prev_hash != *current_meta.best_block() {
            debug!(
                target: LOG_TARGET,
                "Orphaned block #{}: ({}), current tip is: #{} ({}). We need to fetch the complete block from peer: \
                 ({})",
                header.height,
                block_hash.to_hex(),
                current_meta.height_of_longest_chain(),
                current_meta.best_block().to_hex(),
                source_peer,
            );
            if excess_sigs.is_empty() {
                let block = BlockBuilder::new(header.version)
                    .with_coinbase_utxo(coinbase_output, coinbase_kernel)
                    .with_header(header.clone())
                    .build();
                return Ok(block);
            }
            metrics::compact_block_tx_misses(header.height).set(excess_sigs.len() as i64);
            let block = self.request_full_block_from_peer(source_peer, block_hash).await?;
            return Ok(block);
        }

        // We know that the block is neither and orphan or a coinbase, so lets ask our mempool for the transactions
        let (known_transactions, missing_excess_sigs) = self.mempool.retrieve_by_excess_sigs(excess_sigs).await?;
        let known_transactions = known_transactions.into_iter().map(|tx| (*tx).clone()).collect();

        metrics::compact_block_tx_misses(header.height).set(missing_excess_sigs.len() as i64);

        let mut builder = BlockBuilder::new(header.version)
            .with_coinbase_utxo(coinbase_output, coinbase_kernel)
            .with_transactions(known_transactions);

        if missing_excess_sigs.is_empty() {
            debug!(
                target: LOG_TARGET,
                "All transactions for block #{} ({}) found in mempool",
                header.height,
                block_hash.to_hex()
            );
        } else {
            debug!(
                target: LOG_TARGET,
                "Requesting {} unknown transaction(s) from peer '{}'.",
                missing_excess_sigs.len(),
                source_peer
            );

            let FetchMempoolTransactionsResponse {
                transactions,
                not_found,
            } = self
                .outbound_nci
                .request_transactions_by_excess_sig(source_peer.clone(), missing_excess_sigs)
                .await?;

            // Add returned transactions to unconfirmed pool
            if !transactions.is_empty() {
                self.mempool.insert_all(transactions.clone()).await?;
            }

            if !not_found.is_empty() {
                warn!(
                    target: LOG_TARGET,
                    "Peer {} was not able to return all transactions for block #{} ({}). {} transaction(s) not found. \
                     Requesting full block.",
                    source_peer,
                    header.height,
                    block_hash.to_hex(),
                    not_found.len()
                );

                metrics::compact_block_full_misses(header.height).inc();
                let block = self.request_full_block_from_peer(source_peer, block_hash).await?;
                return Ok(block);
            }

            builder = builder.with_transactions(
                transactions
                    .into_iter()
                    .map(|tx| Arc::try_unwrap(tx).unwrap_or_else(|tx| (*tx).clone()))
                    .collect(),
            );
        }

        // NB: Add the header last because `with_transactions` etc updates the current header, but we have the final one
        // already
        builder = builder.with_header(header.clone());
        let block = builder.build();

        // Perform a sanity check on the reconstructed block, if the MMR roots don't match then it's possible one or
        // more transactions in our mempool had the same excess/signature for a *different* transaction.
        // This is extremely unlikely, but still possible. In case of a mismatch, request the full block from the peer.
        let (block, mmr_roots) = match self.blockchain_db.calculate_mmr_roots(block).await {
            Err(_) => {
                let block = self.request_full_block_from_peer(source_peer, block_hash).await?;
                return Ok(block);
            },
            Ok(v) => v,
        };
        if let Err(e) = helpers::check_mmr_roots(&header, &mmr_roots) {
            warn!(
                target: LOG_TARGET,
                "Reconstructed block #{} ({}) failed MMR check validation!. Requesting full block. Error: {}",
                header.height,
                block_hash.to_hex(),
                e,
            );

            metrics::compact_block_mmr_mismatch(header.height).inc();
            let block = self.request_full_block_from_peer(source_peer, block_hash).await?;
            return Ok(block);
        }

        Ok(block)
    }

    async fn request_full_block_from_peer(
        &mut self,
        source_peer: NodeId,
        block_hash: BlockHash,
    ) -> Result<Block, CommsInterfaceError> {
        match self
            .outbound_nci
            .request_blocks_by_hashes_from_peer(block_hash, Some(source_peer.clone()))
            .await?
        {
            Some(block) => Ok(block),
            None => {
                if let Err(e) = self
                    .connectivity
                    .ban_peer_until(
                        source_peer.clone(),
                        Duration::from_secs(100),
                        format!("Peer {} failed to return the block that was requested.", source_peer),
                    )
                    .await
                {
                    error!(target: LOG_TARGET, "Failed to ban peer: {}", e);
                }

                debug!(
                    target: LOG_TARGET,
                    "Peer `{}` failed to return the block that was requested.", source_peer
                );
                Err(CommsInterfaceError::InvalidPeerResponse(format!(
                    "Invalid response from peer `{}`: Peer failed to provide the block that was propagated",
                    source_peer
                )))
            },
        }
    }

    /// Handle inbound blocks from remote nodes and local services.
    ///
    /// ## Arguments
    /// block - the block to store
    /// new_block_msg - propagate this new block message
    /// source_peer - the peer that sent this new block message, or None if the block was generated by a local miner
    pub async fn handle_block(
        &mut self,
        block: Block,
        source_peer: Option<NodeId>,
    ) -> Result<BlockHash, CommsInterfaceError> {
        let block_hash = block.hash();
        let block_height = block.header.height;

        info!(
            target: LOG_TARGET,
            "Block #{} ({}) received from {}",
            block_height,
            block_hash.to_hex(),
            source_peer
                .as_ref()
                .map(|p| format!("remote peer: {}", p))
                .unwrap_or_else(|| "local services".to_string())
        );
        debug!(target: LOG_TARGET, "Incoming block: {}", block);
        let timer = Instant::now();
        let block = self.hydrate_block(block).await?;

        let add_block_result = self.blockchain_db.add_block(block.clone()).await;
        // Create block event on block event stream
        match add_block_result {
            Ok(block_add_result) => {
                debug!(
                    target: LOG_TARGET,
                    "Block #{} ({}) added ({}) to blockchain in {:.2?}",
                    block_height,
                    block_hash.to_hex(),
                    block_add_result,
                    timer.elapsed()
                );

                let should_propagate = match &block_add_result {
                    BlockAddResult::Ok(_) => true,
                    BlockAddResult::BlockExists => false,
                    BlockAddResult::OrphanBlock => false,
                    BlockAddResult::ChainReorg { .. } => true,
                };

                self.update_block_result_metrics(&block_add_result).await?;
                self.publish_block_event(BlockEvent::ValidBlockAdded(block.clone(), block_add_result));

                if should_propagate {
                    debug!(
                        target: LOG_TARGET,
                        "Propagate block ({}) to network.",
                        block_hash.to_hex()
                    );
                    let exclude_peers = source_peer.into_iter().collect();
                    let new_block_msg = NewBlock::from(&*block);
                    self.outbound_nci.propagate_block(new_block_msg, exclude_peers).await?;
                }
                Ok(block_hash)
            },

            Err(e @ ChainStorageError::ValidationError { .. }) => {
                let block_hash = block.hash();
                metrics::rejected_blocks(block.header.height, &block_hash).inc();
                warn!(
                    target: LOG_TARGET,
                    "Peer {} sent an invalid block: {}",
                    source_peer
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "<local request>".to_string()),
                    e
                );
                match source_peer {
                    Some(ref source_peer) => {
                        if let Err(e) = self
                            .connectivity
                            .ban_peer(source_peer.clone(), format!("Peer propagated invalid block: {}", e))
                            .await
                        {
                            error!(target: LOG_TARGET, "Failed to ban peer: {}", e);
                        }
                    },
                    // SECURITY: This indicates an issue in the transaction validator.
                    None => metrics::rejected_local_blocks(block.header.height, &block_hash).inc(),
                }
                self.publish_block_event(BlockEvent::AddBlockValidationFailed { block, source_peer });
                Err(e.into())
            },

            Err(e) => {
                metrics::rejected_blocks(block.header.height, &block.hash()).inc();
                self.publish_block_event(BlockEvent::AddBlockErrored { block });
                Err(e.into())
            },
        }
    }

    async fn hydrate_block(&mut self, block: Block) -> Result<Arc<Block>, CommsInterfaceError> {
        let block_hash = block.hash();
        let block_height = block.header.height;
        if block.body.inputs().is_empty() {
            debug!(
                target: LOG_TARGET,
                "Block #{} ({}) contains no inputs so nothing to hydrate",
                block_height,
                block_hash.to_hex(),
            );
            return Ok(Arc::new(block));
        }

        let timer = Instant::now();
        let (header, mut inputs, outputs, kernels) = block.dissolve();

        let db = self.blockchain_db.inner().db_read_access()?;
        for input in &mut inputs {
            if !input.is_compact() {
                continue;
            }

            let output_mined_info =
                db.fetch_output(&input.output_hash())?
                    .ok_or_else(|| CommsInterfaceError::InvalidFullBlock {
                        hash: block_hash,
                        details: format!("Output {} to be spent does not exist in db", input.output_hash()),
                    })?;

            match output_mined_info.output {
                PrunedOutput::Pruned { .. } => {
                    return Err(CommsInterfaceError::InvalidFullBlock {
                        hash: block_hash,
                        details: format!("Output {} to be spent is pruned", input.output_hash()),
                    });
                },
                PrunedOutput::NotPruned { output } => {
                    input.add_output_data(
                        output.version,
                        output.features,
                        output.commitment,
                        output.script,
                        output.sender_offset_public_key,
                        output.covenant,
                        output.encrypted_data,
                        output.minimum_value_promise,
                    );
                },
            }
        }
        debug!(
            target: LOG_TARGET,
            "Hydrated block #{} ({}) with {} input(s) in {:.2?}",
            block_height,
            block_hash.to_hex(),
            inputs.len(),
            timer.elapsed()
        );
        let block = Block::new(header, AggregateBody::new(inputs, outputs, kernels));
        Ok(Arc::new(block))
    }

    fn publish_block_event(&self, event: BlockEvent) {
        if let Err(event) = self.block_event_sender.send(Arc::new(event)) {
            debug!(target: LOG_TARGET, "No event subscribers. Event {} dropped.", event.0)
        }
    }

    async fn update_block_result_metrics(&self, block_add_result: &BlockAddResult) -> Result<(), CommsInterfaceError> {
        fn update_target_difficulty(block: &ChainBlock) {
            match block.header().pow_algo() {
                PowAlgorithm::Sha3 => {
                    metrics::target_difficulty_sha()
                        .set(i64::try_from(block.accumulated_data().target_difficulty.as_u64()).unwrap_or(i64::MAX));
                },
                PowAlgorithm::Monero => {
                    metrics::target_difficulty_monero()
                        .set(i64::try_from(block.accumulated_data().target_difficulty.as_u64()).unwrap_or(i64::MAX));
                },
            }
        }

        match block_add_result {
            BlockAddResult::Ok(ref block) => {
                #[allow(clippy::cast_possible_wrap)]
                metrics::tip_height().set(block.height() as i64);
                update_target_difficulty(block);
                let utxo_set_size = self.blockchain_db.utxo_count().await?;
                metrics::utxo_set_size().set(utxo_set_size.try_into().unwrap_or(i64::MAX));
            },
            BlockAddResult::ChainReorg { added, removed } => {
                if let Some(fork_height) = added.last().map(|b| b.height()) {
                    #[allow(clippy::cast_possible_wrap)]
                    metrics::tip_height().set(fork_height as i64);
                    metrics::reorg(fork_height, added.len(), removed.len()).inc();
                }
                for block in added {
                    update_target_difficulty(block);
                }
                let utxo_set_size = self.blockchain_db.utxo_count().await?;
                metrics::utxo_set_size().set(utxo_set_size.try_into().unwrap_or(i64::MAX));
            },
            BlockAddResult::OrphanBlock => {
                metrics::orphaned_blocks().inc();
            },
            _ => {},
        }
        Ok(())
    }

    async fn get_target_difficulty_for_next_block(
        &self,
        pow_algo: PowAlgorithm,
        constants: &ConsensusConstants,
        current_block_hash: HashOutput,
    ) -> Result<Difficulty, CommsInterfaceError> {
        let target_difficulty = self
            .blockchain_db
            .fetch_target_difficulty_for_next_block(pow_algo, current_block_hash)
            .await?;

        let target = target_difficulty.calculate(
            constants.min_pow_difficulty(pow_algo),
            constants.max_pow_difficulty(pow_algo),
        );
        debug!(target: LOG_TARGET, "Target difficulty {} for PoW {}", target, pow_algo);
        Ok(target)
    }
}

impl<B> Clone for InboundNodeCommsHandlers<B> {
    fn clone(&self) -> Self {
        Self {
            block_event_sender: self.block_event_sender.clone(),
            blockchain_db: self.blockchain_db.clone(),
            mempool: self.mempool.clone(),
            consensus_manager: self.consensus_manager.clone(),
            new_block_request_semaphore: self.new_block_request_semaphore.clone(),
            outbound_nci: self.outbound_nci.clone(),
            connectivity: self.connectivity.clone(),
        }
    }
}
