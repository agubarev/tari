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

pub mod config;
pub mod error;
pub mod handle;

mod input_selection;
pub use input_selection::{UtxoSelectionCriteria, UtxoSelectionFilter, UtxoSelectionOrdering};

mod recovery;
pub mod resources;
pub mod service;
pub mod storage;
mod tasks;

use std::{marker::PhantomData, sync::Arc};

use futures::future;
use log::*;
use tari_comms::NodeIdentity;
use tari_core::{consensus::NetworkConsensus, transactions::CryptoFactories};
use tari_key_manager::key_manager_service::{storage::database::KeyManagerBackend, KeyManagerHandle};
use tari_service_framework::{
    async_trait,
    reply_channel,
    ServiceInitializationError,
    ServiceInitializer,
    ServiceInitializerContext,
};
use tokio::sync::broadcast;

use crate::{
    base_node_service::handle::BaseNodeServiceHandle,
    connectivity_service::WalletConnectivityHandle,
    output_manager_service::{
        config::OutputManagerServiceConfig,
        handle::OutputManagerHandle,
        service::OutputManagerService,
        storage::database::{OutputManagerBackend, OutputManagerDatabase},
    },
};

const LOG_TARGET: &str = "wallet::output_manager_service::initializer";

pub struct OutputManagerServiceInitializer<T, TKeyManagerInterface>
where T: OutputManagerBackend
{
    config: OutputManagerServiceConfig,
    backend: Option<T>,
    factories: CryptoFactories,
    network: NetworkConsensus,
    node_identity: Arc<NodeIdentity>,
    phantom: PhantomData<TKeyManagerInterface>,
}

impl<T, TKeyManagerInterface> OutputManagerServiceInitializer<T, TKeyManagerInterface>
where T: OutputManagerBackend + 'static
{
    pub fn new(
        config: OutputManagerServiceConfig,
        backend: T,
        factories: CryptoFactories,
        network: NetworkConsensus,
        node_identity: Arc<NodeIdentity>,
    ) -> Self {
        Self {
            config,
            backend: Some(backend),
            factories,
            network,
            node_identity,
            phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<T, TKeyManagerInterface> ServiceInitializer for OutputManagerServiceInitializer<T, TKeyManagerInterface>
where
    T: OutputManagerBackend + 'static,
    TKeyManagerInterface: KeyManagerBackend + 'static,
{
    async fn initialize(&mut self, context: ServiceInitializerContext) -> Result<(), ServiceInitializationError> {
        let (sender, receiver) = reply_channel::unbounded();
        let (publisher, _) = broadcast::channel(self.config.event_channel_size);

        // Register handle before waiting for handles to be ready
        let oms_handle = OutputManagerHandle::new(sender, publisher.clone());
        context.register_handle(oms_handle);

        let backend = self
            .backend
            .take()
            .expect("Cannot start Output Manager Service without setting a storage backend");
        let factories = self.factories.clone();
        let config = self.config.clone();
        let constants = self.network.create_consensus_constants().pop().unwrap();
        let node_identity = self.node_identity.clone();
        context.spawn_when_ready(move |handles| async move {
            let base_node_service_handle = handles.expect_handle::<BaseNodeServiceHandle>();
            let connectivity = handles.expect_handle::<WalletConnectivityHandle>();
            let key_manager = handles.expect_handle::<KeyManagerHandle<TKeyManagerInterface>>();

            let service = OutputManagerService::new(
                config,
                receiver,
                OutputManagerDatabase::new(backend),
                publisher,
                factories,
                constants,
                handles.get_shutdown_signal(),
                base_node_service_handle,
                connectivity,
                node_identity,
                key_manager,
            )
            .await
            .expect("Could not initialize Output Manager Service")
            .start();

            futures::pin_mut!(service);
            future::select(service, handles.get_shutdown_signal()).await;
            info!(target: LOG_TARGET, "Output manager service shutdown");
        });
        Ok(())
    }
}
