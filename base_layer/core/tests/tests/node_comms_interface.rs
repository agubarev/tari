//  Copyright 2022. The Tari Project
//
//  Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
//  following conditions are met:
//
//  1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
//  disclaimer.
//
//  2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
//  following disclaimer in the documentation and/or other materials provided with the distribution.
//
//  3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
//  products derived from this software without specific prior written permission.
//
//  THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
//  INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//  DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//  SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//  SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//  WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
//  USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use tari_common::configuration::Network;
use tari_common_types::types::PublicKey;
use tari_comms::test_utils::mocks::create_connectivity_mock;
use tari_core::{
    base_node::comms_interface::{
        InboundNodeCommsHandlers,
        NodeCommsRequest,
        NodeCommsResponse,
        OutboundNodeCommsInterface,
    },
    chain_storage::{BlockchainDatabaseConfig, DbTransaction, Validators},
    consensus::ConsensusManager,
    covenants::Covenant,
    mempool::{Mempool, MempoolConfig},
    test_helpers::{
        blockchain::{create_store_with_consensus_and_validators_and_config, create_test_blockchain_db},
        create_consensus_rules,
    },
    transactions::{
        tari_amount::MicroTari,
        test_helpers::{create_utxo, spend_utxos},
        transaction_components::{OutputFeatures, TransactionOutput, TransactionOutputVersion, UnblindedOutput},
        CryptoFactories,
    },
    txn_schema,
    validation::{mocks::MockValidator, transaction::TransactionChainLinkedValidator},
};
use tari_crypto::keys::PublicKey as PublicKeyTrait;
use tari_script::{inputs, script, TariScript};
use tari_service_framework::reply_channel;
use tokio::sync::{broadcast, mpsc};

use crate::helpers::block_builders::append_block;

fn new_mempool() -> Mempool {
    let rules = create_consensus_rules();
    let mempool_validator = MockValidator::new(true);
    Mempool::new(MempoolConfig::default(), rules, Box::new(mempool_validator))
}

#[tokio::test]
async fn inbound_get_metadata() {
    let store = create_test_blockchain_db();
    let mempool = new_mempool();

    let network = Network::LocalNet;
    let consensus_manager = ConsensusManager::builder(network).build();
    let (block_event_sender, _) = broadcast::channel(50);
    let (request_sender, _) = reply_channel::unbounded();
    let (block_sender, _) = mpsc::unbounded_channel();
    let outbound_nci = OutboundNodeCommsInterface::new(request_sender, block_sender.clone());

    let (connectivity, _) = create_connectivity_mock();
    let inbound_nch = InboundNodeCommsHandlers::new(
        block_event_sender,
        store.clone().into(),
        mempool,
        consensus_manager,
        outbound_nci,
        connectivity,
    );
    let block = store.fetch_block(0, true).unwrap().block().clone();

    if let Ok(NodeCommsResponse::ChainMetadata(received_metadata)) =
        inbound_nch.handle_request(NodeCommsRequest::GetChainMetadata).await
    {
        assert_eq!(received_metadata.height_of_longest_chain(), 0);
        assert_eq!(received_metadata.best_block(), &block.hash());
        assert_eq!(received_metadata.pruning_horizon(), 0);
    } else {
        panic!();
    }
}

#[tokio::test]
async fn inbound_fetch_kernel_by_excess_sig() {
    let store = create_test_blockchain_db();
    let mempool = new_mempool();

    let network = Network::LocalNet;
    let consensus_manager = ConsensusManager::builder(network).build();
    let (block_event_sender, _) = broadcast::channel(50);
    let (request_sender, _) = reply_channel::unbounded();
    let (block_sender, _) = mpsc::unbounded_channel();
    let outbound_nci = OutboundNodeCommsInterface::new(request_sender, block_sender.clone());
    let (connectivity, _) = create_connectivity_mock();
    let inbound_nch = InboundNodeCommsHandlers::new(
        block_event_sender,
        store.clone().into(),
        mempool,
        consensus_manager,
        outbound_nci,
        connectivity,
    );
    let block = store.fetch_block(0, true).unwrap().block().clone();
    let sig = block.body.kernels()[0].excess_sig.clone();

    if let Ok(NodeCommsResponse::TransactionKernels(received_kernels)) = inbound_nch
        .handle_request(NodeCommsRequest::FetchKernelByExcessSig(sig))
        .await
    {
        assert_eq!(received_kernels.len(), 1);
        assert_eq!(received_kernels[0], block.body.kernels()[0]);
    } else {
        panic!("kernel not found");
    }
}

#[tokio::test]
async fn inbound_fetch_headers() {
    let store = create_test_blockchain_db();
    let mempool = new_mempool();
    let network = Network::LocalNet;
    let consensus_manager = ConsensusManager::builder(network).build();
    let (block_event_sender, _) = broadcast::channel(50);
    let (request_sender, _) = reply_channel::unbounded();
    let (block_sender, _) = mpsc::unbounded_channel();
    let outbound_nci = OutboundNodeCommsInterface::new(request_sender, block_sender);
    let (connectivity, _) = create_connectivity_mock();
    let inbound_nch = InboundNodeCommsHandlers::new(
        block_event_sender,
        store.clone().into(),
        mempool,
        consensus_manager,
        outbound_nci,
        connectivity,
    );
    let header = store.fetch_block(0, true).unwrap().header().clone();

    if let Ok(NodeCommsResponse::BlockHeaders(received_headers)) =
        inbound_nch.handle_request(NodeCommsRequest::FetchHeaders(0..=0)).await
    {
        assert_eq!(received_headers.len(), 1);
        assert_eq!(*received_headers[0].header(), header);
    } else {
        panic!();
    }
}

#[tokio::test]
async fn inbound_fetch_utxos() {
    let factories = CryptoFactories::default();

    let store = create_test_blockchain_db();
    let mempool = new_mempool();
    let network = Network::LocalNet;
    let consensus_manager = ConsensusManager::builder(network).build();
    let (block_event_sender, _) = broadcast::channel(50);
    let (request_sender, _) = reply_channel::unbounded();
    let (block_sender, _) = mpsc::unbounded_channel();
    let outbound_nci = OutboundNodeCommsInterface::new(request_sender, block_sender);
    let (connectivity, _) = create_connectivity_mock();
    let inbound_nch = InboundNodeCommsHandlers::new(
        block_event_sender,
        store.clone().into(),
        mempool,
        consensus_manager,
        outbound_nci,
        connectivity,
    );
    let block = store.fetch_block(0, true).unwrap().block().clone();
    let utxo_1 = block.body.outputs()[0].clone();
    let hash_1 = utxo_1.hash();

    let (utxo_2, _, _) = create_utxo(
        MicroTari(10_000),
        &factories,
        &Default::default(),
        &TariScript::default(),
        &Covenant::default(),
        MicroTari::zero(),
    );
    let hash_2 = utxo_2.hash();

    // Only retrieve a subset of the actual hashes, including a fake hash in the list
    if let Ok(NodeCommsResponse::TransactionOutputs(received_utxos)) = inbound_nch
        .handle_request(NodeCommsRequest::FetchMatchingUtxos(vec![hash_1, hash_2]))
        .await
    {
        assert_eq!(received_utxos.len(), 1);
        assert_eq!(received_utxos[0], utxo_1);
    } else {
        panic!();
    }
}

#[tokio::test]
async fn inbound_fetch_blocks() {
    let store = create_test_blockchain_db();
    let mempool = new_mempool();
    let (block_event_sender, _) = broadcast::channel(50);
    let network = Network::LocalNet;
    let consensus_manager = ConsensusManager::builder(network).build();
    let (request_sender, _) = reply_channel::unbounded();
    let (block_sender, _) = mpsc::unbounded_channel();
    let outbound_nci = OutboundNodeCommsInterface::new(request_sender, block_sender);
    let (connectivity, _) = create_connectivity_mock();
    let inbound_nch = InboundNodeCommsHandlers::new(
        block_event_sender,
        store.clone().into(),
        mempool,
        consensus_manager,
        outbound_nci,
        connectivity,
    );
    let block = store.fetch_block(0, true).unwrap().block().clone();

    if let Ok(NodeCommsResponse::HistoricalBlocks(received_blocks)) = inbound_nch
        .handle_request(NodeCommsRequest::FetchMatchingBlocks {
            range: 0..=0,
            compact: true,
        })
        .await
    {
        assert_eq!(received_blocks.len(), 1);
        assert_eq!(*received_blocks[0].block(), block);
    } else {
        panic!();
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn inbound_fetch_blocks_before_horizon_height() {
    let factories = CryptoFactories::default();
    let consensus_manager = ConsensusManager::builder(Network::LocalNet).build();
    let block0 = consensus_manager.get_genesis_block();
    let validators = Validators::new(
        MockValidator::new(true),
        MockValidator::new(true),
        MockValidator::new(true),
    );
    let config = BlockchainDatabaseConfig {
        pruning_horizon: 3,
        pruning_interval: 1,
        ..Default::default()
    };
    let store = create_store_with_consensus_and_validators_and_config(consensus_manager.clone(), validators, config);
    let mempool_validator = TransactionChainLinkedValidator::new(store.clone(), consensus_manager.clone());
    let mempool = Mempool::new(
        MempoolConfig::default(),
        consensus_manager.clone(),
        Box::new(mempool_validator),
    );
    let (block_event_sender, _) = broadcast::channel(50);
    let (request_sender, _) = reply_channel::unbounded();
    let (block_sender, _) = mpsc::unbounded_channel();
    let outbound_nci = OutboundNodeCommsInterface::new(request_sender, block_sender);
    let (connectivity, _) = create_connectivity_mock();
    let inbound_nch = InboundNodeCommsHandlers::new(
        block_event_sender,
        store.clone().into(),
        mempool,
        consensus_manager.clone(),
        outbound_nci,
        connectivity,
    );
    let script = script!(Nop);
    let amount = MicroTari(10_000);
    let (utxo, key, offset) = create_utxo(
        amount,
        &factories,
        &Default::default(),
        &script,
        &Covenant::default(),
        MicroTari::zero(),
    );
    let mut txn = DbTransaction::new();
    txn.insert_utxo(
        utxo.clone(),
        *block0.hash(),
        0,
        block0.header().output_mmr_size as u32,
        0,
    );
    if let Err(e) = store.commit(txn) {
        panic!("{}", e);
    }

    let metadata_signature = TransactionOutput::create_metadata_signature(
        TransactionOutputVersion::get_current_version(),
        amount,
        &key,
        &script,
        &OutputFeatures::default(),
        &offset,
        &Covenant::default(),
        &utxo.encrypted_data,
        utxo.minimum_value_promise,
    )
    .unwrap();
    let unblinded_output = UnblindedOutput::new_current_version(
        amount,
        key.clone(),
        Default::default(),
        script,
        inputs!(PublicKey::from_secret_key(&key)),
        key,
        PublicKey::from_secret_key(&offset),
        metadata_signature,
        0,
        Covenant::default(),
        utxo.encrypted_data,
        utxo.minimum_value_promise,
    );

    let txn = txn_schema!(
        from: vec![unblinded_output],
        to: vec![MicroTari(5_000), MicroTari(4_000)]
    );
    let (txn, _) = spend_utxos(txn);
    let block1 = append_block(&store, &block0, vec![txn], &consensus_manager, 1.into()).unwrap();
    let block2 = append_block(&store, &block1, vec![], &consensus_manager, 1.into()).unwrap();
    let block3 = append_block(&store, &block2, vec![], &consensus_manager, 1.into()).unwrap();
    let block4 = append_block(&store, &block3, vec![], &consensus_manager, 1.into()).unwrap();
    let _block5 = append_block(&store, &block4, vec![], &consensus_manager, 1.into()).unwrap();

    if let Ok(NodeCommsResponse::HistoricalBlocks(received_blocks)) = inbound_nch
        .handle_request(NodeCommsRequest::FetchMatchingBlocks {
            range: 1..=1,
            compact: true,
        })
        .await
    {
        assert_eq!(received_blocks.len(), 1);
        assert_eq!(received_blocks[0].pruned_outputs().len(), 1)
    } else {
        panic!();
    }

    if let Ok(NodeCommsResponse::HistoricalBlocks(received_blocks)) = inbound_nch
        .handle_request(NodeCommsRequest::FetchMatchingBlocks {
            range: 2..=2,
            compact: true,
        })
        .await
    {
        assert_eq!(received_blocks.len(), 1);
        assert_eq!(received_blocks[0].block(), block2.block());
    } else {
        panic!();
    }
}
