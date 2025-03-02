//  Copyright 2020, The Tari Project
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

use std::{convert::TryInto, sync::Arc, time::Duration};

use futures::StreamExt;
use tari_comms::{
    peer_manager::{NodeDistance, NodeId, PeerFeatures},
    protocol::rpc::{mock::RpcRequestMock, RpcStatusCode},
    test_utils::node_identity::{build_node_identity, ordered_node_identities_by_distance},
    PeerManager,
};
use tari_test_utils::collect_recv;
use tari_utilities::ByteArray;

use crate::{
    proto::rpc::GetCloserPeersRequest,
    rpc::{DhtRpcService, DhtRpcServiceImpl},
    test_utils::build_peer_manager,
};

fn setup() -> (DhtRpcServiceImpl, RpcRequestMock, Arc<PeerManager>) {
    let peer_manager = build_peer_manager();
    let mock = RpcRequestMock::new(peer_manager.clone());
    let service = DhtRpcServiceImpl::new(peer_manager.clone());

    (service, mock, peer_manager)
}

// Unit tests for get_closer_peers request
mod get_closer_peers {
    use super::*;
    use crate::rpc::PeerInfo;

    #[tokio::test]
    async fn it_returns_empty_peer_stream() {
        let (service, mock, _) = setup();
        let node_identity = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
        let req = GetCloserPeersRequest {
            n: 10,
            excluded: vec![],
            closer_to: node_identity.node_id().to_vec(),
            include_clients: false,
        };

        let req = mock.request_with_context(node_identity.node_id().clone(), req);
        let mut peers_stream = service.get_closer_peers(req).await.unwrap();
        let next = peers_stream.next().await;
        // Empty stream
        assert!(next.is_none());
    }

    #[tokio::test]
    async fn it_returns_closest_peers() {
        let (service, mock, peer_manager) = setup();
        let node_identity = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
        let peers = ordered_node_identities_by_distance(node_identity.node_id(), 10, PeerFeatures::COMMUNICATION_NODE);
        for peer in &peers {
            peer_manager.add_peer(peer.to_peer()).await.unwrap();
        }
        let req = GetCloserPeersRequest {
            n: 15,
            excluded: vec![],
            closer_to: node_identity.node_id().to_vec(),
            include_clients: false,
        };

        let req = mock.request_with_context(node_identity.node_id().clone(), req);
        let peers_stream = service.get_closer_peers(req).await.unwrap();
        let results = collect_recv!(peers_stream.into_inner(), timeout = Duration::from_secs(10));
        assert_eq!(results.len(), 10);

        let peers = results
            .into_iter()
            .map(Result::unwrap)
            .map(|r| r.peer.unwrap())
            .map(|p| p.try_into().unwrap())
            .collect::<Vec<PeerInfo>>();

        let mut dist = NodeDistance::zero();
        for p in &peers {
            let current = NodeId::from_public_key(&p.public_key).distance(node_identity.node_id());
            assert!(dist < current);
            dist = current;
        }
    }

    #[tokio::test]
    async fn it_returns_n_peers() {
        let (service, mock, peer_manager) = setup();

        let node_identity = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
        let peers = ordered_node_identities_by_distance(node_identity.node_id(), 6, PeerFeatures::COMMUNICATION_NODE);
        for peer in &peers {
            peer_manager.add_peer(peer.to_peer()).await.unwrap();
        }
        let req = GetCloserPeersRequest {
            n: 5,
            excluded: vec![],
            closer_to: node_identity.node_id().to_vec(),
            include_clients: false,
        };

        let req = mock.request_with_context(node_identity.node_id().clone(), req);
        let peers_stream = service.get_closer_peers(req).await.unwrap();
        let results = peers_stream.collect::<Vec<_>>().await;
        assert_eq!(results.len(), 5);
    }

    #[tokio::test]
    async fn it_skips_excluded_peers() {
        let (service, mock, peer_manager) = setup();

        let node_identity = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
        let peers = ordered_node_identities_by_distance(node_identity.node_id(), 5, PeerFeatures::COMMUNICATION_NODE);
        for peer in &peers {
            peer_manager.add_peer(peer.to_peer()).await.unwrap();
        }
        let excluded_peer = peers.last().unwrap();
        let req = GetCloserPeersRequest {
            n: 100,
            excluded: vec![excluded_peer.node_id().to_vec()],
            closer_to: node_identity.node_id().to_vec(),
            include_clients: true,
        };

        let req = mock.request_with_context(node_identity.node_id().clone(), req);
        let peers_stream = service.get_closer_peers(req).await.unwrap();
        let results = collect_recv!(peers_stream.into_inner(), timeout = Duration::from_secs(10));
        let mut peers = results.into_iter().map(Result::unwrap).map(|r| r.peer.unwrap());
        assert!(peers.all(|p| p.public_key != excluded_peer.public_key().as_bytes()));
    }

    #[tokio::test]
    async fn it_errors_if_maximum_n_exceeded() {
        let (service, mock, _) = setup();
        let req = GetCloserPeersRequest {
            n: 5_000,
            ..Default::default()
        };

        let node_id = NodeId::default();
        let req = mock.request_with_context(node_id, req);
        let err = service.get_closer_peers(req).await.unwrap_err();
        assert_eq!(err.as_status_code(), RpcStatusCode::BadRequest);
    }
}

mod get_peers {
    use std::time::Duration;

    use tari_comms::test_utils::node_identity::build_many_node_identities;

    use super::*;
    use crate::{proto::rpc::GetPeersRequest, rpc::PeerInfo};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn it_returns_empty_peer_stream() {
        let (service, mock, _) = setup();
        let node_identity = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
        let req = GetPeersRequest {
            n: 10,
            include_clients: false,
        };

        let req = mock.request_with_context(node_identity.node_id().clone(), req);
        let mut peers_stream = service.get_peers(req).await.unwrap();
        let next = peers_stream.next().await;
        // Empty stream
        assert!(next.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn it_returns_all_peers() {
        let (service, mock, peer_manager) = setup();
        let nodes = build_many_node_identities(3, PeerFeatures::COMMUNICATION_NODE);
        let clients = build_many_node_identities(2, PeerFeatures::COMMUNICATION_CLIENT);
        for peer in nodes.iter().chain(clients.iter()) {
            peer_manager.add_peer(peer.to_peer()).await.unwrap();
        }
        let req = GetPeersRequest {
            n: 0,
            include_clients: true,
        };

        let peers_stream = service
            .get_peers(mock.request_with_context(Default::default(), req))
            .await
            .unwrap();
        let results = collect_recv!(peers_stream.into_inner(), timeout = Duration::from_secs(10));
        assert_eq!(results.len(), 5);

        let peers = results
            .into_iter()
            .map(Result::unwrap)
            .map(|r| r.peer.unwrap())
            .map(|p| p.try_into().unwrap())
            .collect::<Vec<PeerInfo>>();

        assert_eq!(peers.iter().filter(|p| p.peer_features.is_client()).count(), 2);
        assert_eq!(peers.iter().filter(|p| p.peer_features.is_node()).count(), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn it_excludes_clients() {
        let (service, mock, peer_manager) = setup();
        let nodes = build_many_node_identities(3, PeerFeatures::COMMUNICATION_NODE);
        let clients = build_many_node_identities(2, PeerFeatures::COMMUNICATION_CLIENT);
        for peer in nodes.iter().chain(clients.iter()) {
            peer_manager.add_peer(peer.to_peer()).await.unwrap();
        }
        let req = GetPeersRequest {
            n: 0,
            include_clients: false,
        };

        let peers_stream = service
            .get_peers(mock.request_with_context(Default::default(), req))
            .await
            .unwrap();
        let results = collect_recv!(peers_stream.into_inner(), timeout = Duration::from_secs(10));
        assert_eq!(results.len(), 3);

        let peers = results
            .into_iter()
            .map(Result::unwrap)
            .map(|r| r.peer.unwrap())
            .map(|p| p.try_into().unwrap())
            .collect::<Vec<PeerInfo>>();

        assert!(peers.iter().all(|p| p.peer_features.is_node()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn it_returns_n_peers() {
        let (service, mock, peer_manager) = setup();

        let node_identity = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
        let peers = build_many_node_identities(3, PeerFeatures::COMMUNICATION_NODE);
        for peer in &peers {
            peer_manager.add_peer(peer.to_peer()).await.unwrap();
        }
        let req = GetPeersRequest {
            n: 2,
            include_clients: false,
        };

        let req = mock.request_with_context(node_identity.node_id().clone(), req);
        let peers_stream = service.get_peers(req).await.unwrap();
        let results = peers_stream.collect::<Vec<_>>().await;
        assert_eq!(results.len(), 2);
    }
}
