// Copyright 2020, The Tari Project
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

use std::{error::Error, time::Duration};

use multiaddr::Protocol;
use tari_shutdown::Shutdown;
use tari_test_utils::unpack_enum;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
    time::timeout,
};

use crate::{
    backoff::ConstantBackoff,
    connection_manager::{
        dialer::{Dialer, DialerRequest},
        listener::PeerListener,
        manager::ConnectionManagerEvent,
        ConnectionManagerConfig,
        ConnectionManagerError,
    },
    net_address::{MultiaddressesWithStats, PeerAddressSource},
    noise::NoiseConfig,
    peer_manager::PeerFeatures,
    protocol::ProtocolId,
    test_utils::{build_peer_manager, node_identity::build_node_identity},
    transports::MemoryTransport,
};

#[tokio::test]
async fn listen() -> Result<(), Box<dyn Error>> {
    let (event_tx, _) = mpsc::channel(1);
    let mut shutdown = Shutdown::new();
    let peer_manager = build_peer_manager();
    let node_identity = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
    let noise_config = NoiseConfig::new(node_identity.clone());
    let listener = PeerListener::new(
        Default::default(),
        "/memory/0".parse()?,
        MemoryTransport,
        noise_config.clone(),
        event_tx,
        peer_manager,
        node_identity,
        shutdown.to_signal(),
    );

    let mut bind_addr = listener.listen().await?;

    unpack_enum!(Protocol::Memory(port) = bind_addr.pop().unwrap());
    assert!(port > 0);

    shutdown.trigger();

    Ok(())
}

#[tokio::test]
async fn smoke() {
    // This test sets up Dialer and Listener components, uses the Dialer to dial the Listener,
    // asserts the emitted events are correct, opens a substream, sends a small message over the substream,
    // receives and checks the message and then disconnects and shuts down.
    let (event_tx, mut event_rx) = mpsc::channel(10);
    let mut shutdown = Shutdown::new();

    let node_identity1 = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
    let noise_config1 = NoiseConfig::new(node_identity1.clone());
    let expected_proto = ProtocolId::from_static(b"/tari/test-proto");
    let supported_protocols = vec![expected_proto.clone()];
    let peer_manager1 = build_peer_manager();
    let mut listener = PeerListener::new(
        Default::default(),
        "/memory/0".parse().unwrap(),
        MemoryTransport,
        noise_config1,
        event_tx.clone(),
        peer_manager1.clone(),
        node_identity1.clone(),
        shutdown.to_signal(),
    );
    listener.set_supported_protocols(supported_protocols.clone());

    // Get the listening address of the peer
    let address = listener.listen().await.unwrap();

    let node_identity2 = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
    let noise_config2 = NoiseConfig::new(node_identity2.clone());
    let (request_tx, request_rx) = mpsc::channel(1);
    let peer_manager2 = build_peer_manager();
    let mut dialer = Dialer::new(
        ConnectionManagerConfig::default(),
        node_identity2.clone(),
        peer_manager2.clone(),
        MemoryTransport,
        noise_config2,
        ConstantBackoff::new(Duration::from_millis(100)),
        request_rx,
        event_tx,
        shutdown.to_signal(),
    );
    dialer.set_supported_protocols(supported_protocols.clone());

    let dialer_fut = tokio::spawn(dialer.run());

    let mut peer = node_identity1.to_peer();
    peer.addresses = MultiaddressesWithStats::from_addresses_with_source(vec![address], &PeerAddressSource::Config);
    peer.set_id_for_test(1);

    let (reply_tx, reply_rx) = oneshot::channel();
    request_tx
        .send(DialerRequest::Dial(Box::new(peer), Some(reply_tx)))
        .await
        .unwrap();

    let mut outbound_peer_conn = reply_rx.await.unwrap().unwrap();

    // Open a substream
    {
        let mut out_stream = outbound_peer_conn
            .open_substream(&ProtocolId::from_static(b"/tari/test-proto"))
            .await
            .unwrap();
        out_stream.stream.write_all(b"HELLO").await.unwrap();
        out_stream.stream.flush().await.unwrap();
    }

    // Read PeerConnected events - we don't know which connection is which
    unpack_enum!(ConnectionManagerEvent::PeerConnected(conn1) = event_rx.recv().await.unwrap());
    unpack_enum!(ConnectionManagerEvent::PeerConnected(_conn2) = event_rx.recv().await.unwrap());

    // Next event should be a NewInboundSubstream has been received
    let listen_event = event_rx.recv().await.unwrap();
    {
        unpack_enum!(ConnectionManagerEvent::NewInboundSubstream(node_id, proto, in_stream) = listen_event);
        assert_eq!(&node_id, node_identity2.node_id());
        assert_eq!(proto, expected_proto);

        let mut buf = [0u8; 5];
        in_stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, *b"HELLO");
    }

    conn1.disconnect().await.unwrap();

    shutdown.trigger();

    let peer2 = peer_manager1
        .find_by_node_id(node_identity2.node_id())
        .await
        .unwrap()
        .unwrap();
    let peer1 = peer_manager2
        .find_by_node_id(node_identity1.node_id())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(&peer1.public_key, node_identity1.public_key());
    assert_eq!(&peer2.public_key, node_identity2.public_key());

    timeout(Duration::from_secs(5), dialer_fut).await.unwrap().unwrap();
}

#[tokio::test]
async fn banned() {
    let (event_tx, mut event_rx) = mpsc::channel(10);
    let mut shutdown = Shutdown::new();

    let node_identity1 = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
    let noise_config1 = NoiseConfig::new(node_identity1.clone());
    let expected_proto = ProtocolId::from_static(b"/tari/test-proto");
    let supported_protocols = vec![expected_proto.clone()];
    let peer_manager1 = build_peer_manager();
    let mut listener = PeerListener::new(
        Default::default(),
        "/memory/0".parse().unwrap(),
        MemoryTransport,
        noise_config1,
        event_tx.clone(),
        peer_manager1.clone(),
        node_identity1.clone(),
        shutdown.to_signal(),
    );
    listener.set_supported_protocols(supported_protocols.clone());

    // Get the listener address of the peer
    let address = listener.listen().await.unwrap();

    let node_identity2 = build_node_identity(PeerFeatures::COMMUNICATION_NODE);
    // The listener has banned the dialer peer
    let mut peer = node_identity2.to_peer();
    peer.ban_for(Duration::from_secs(60 * 60), "".to_string());
    peer_manager1.add_peer(peer).await.unwrap();

    let noise_config2 = NoiseConfig::new(node_identity2.clone());
    let (request_tx, request_rx) = mpsc::channel(1);
    let peer_manager2 = build_peer_manager();
    let mut dialer = Dialer::new(
        ConnectionManagerConfig::default(),
        node_identity2.clone(),
        peer_manager2.clone(),
        MemoryTransport,
        noise_config2,
        ConstantBackoff::new(Duration::from_millis(100)),
        request_rx,
        event_tx,
        shutdown.to_signal(),
    );
    dialer.set_supported_protocols(supported_protocols);

    let dialer_fut = tokio::spawn(dialer.run());

    let mut peer = node_identity1.to_peer();
    peer.addresses = MultiaddressesWithStats::from_addresses_with_source(vec![address], &PeerAddressSource::Config);
    peer.set_id_for_test(1);

    let (reply_tx, reply_rx) = oneshot::channel();
    request_tx
        .send(DialerRequest::Dial(Box::new(peer), Some(reply_tx)))
        .await
        .unwrap();

    // Check that the dial failed. We're checking that the listener unexpectedly
    // closes the connection before the identity protocol has completed.
    let err = reply_rx.await.unwrap().unwrap_err();
    unpack_enum!(ConnectionManagerError::IdentityProtocolError(_err) = err);

    unpack_enum!(ConnectionManagerEvent::PeerInboundConnectFailed(err) = event_rx.recv().await.unwrap());
    unpack_enum!(ConnectionManagerError::PeerBanned = err);

    shutdown.trigger();

    timeout(Duration::from_secs(5), dialer_fut).await.unwrap().unwrap();
}
