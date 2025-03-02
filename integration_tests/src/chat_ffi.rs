//   Copyright 2023. The Tari Project
//
//   Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
//   following conditions are met:
//
//   1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
//   disclaimer.
//
//   2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
//   following disclaimer in the documentation and/or other materials provided with the distribution.
//
//   3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
//   products derived from this software without specific prior written permission.
//
//   THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
//   INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//   DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//   SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//   SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//   WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
//   USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use std::{
    ffi::{c_void, CString},
    fs,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

type ClientFFI = c_void;

use libc::{c_char, c_int};
use rand::rngs::OsRng;
use tari_chat_client::{database, ChatClient};
use tari_common::configuration::{MultiaddrList, Network};
use tari_common_types::tari_address::TariAddress;
use tari_comms::{
    multiaddr::Multiaddr,
    peer_manager::{Peer, PeerFeatures},
    NodeIdentity,
};
use tari_comms_dht::{store_forward::SafConfig, DbConnectionUrl, DhtConfig, NetworkDiscoveryConfig};
use tari_contacts::contacts_service::{service::ContactOnlineStatus, types::Message};
use tari_p2p::{P2pConfig, TcpTransportConfig, TransportConfig};
use tari_utilities::message_format::MessageFormat;

use crate::{get_base_dir, get_port};

#[cfg_attr(windows, link(name = "tari_chat_ffi.dll"))]
#[cfg_attr(not(windows), link(name = "tari_chat_ffi"))]
extern "C" {
    pub fn create_chat_client(
        config: *mut c_void,
        identity_file_path: *const c_char,
        db_path: *const c_char,
        seed_peers: *mut c_void,
        network_str: *const c_char,
        out_error: *const c_int,
    ) -> *mut ClientFFI;
    pub fn send_message(client: *mut ClientFFI, receiver: *mut c_void, message: *const c_char, out_error: *const c_int);
    pub fn add_contact(client: *mut ClientFFI, address: *mut c_void, out_error: *const c_int);
    pub fn check_online_status(client: *mut ClientFFI, address: *mut c_void, out_error: *const c_int) -> c_int;
    pub fn get_all_messages(client: *mut ClientFFI, sender: *mut c_void, out_error: *const c_int) -> *mut c_void;
}

#[derive(Debug)]
pub struct PtrWrapper(*mut ClientFFI);
unsafe impl Send for PtrWrapper {}

#[derive(Debug)]
pub struct ChatFFI {
    ptr: Arc<Mutex<PtrWrapper>>,
    pub identity: NodeIdentity,
}

#[async_trait]
impl ChatClient for ChatFFI {
    async fn add_contact(&self, address: &TariAddress) {
        let client = self.ptr.lock().unwrap();

        let address_ptr = Box::into_raw(Box::new(address.to_owned())) as *mut c_void;

        let out_error = Box::into_raw(Box::new(0));
        unsafe { add_contact(client.0, address_ptr, out_error) }
    }

    async fn check_online_status(&self, address: &TariAddress) -> ContactOnlineStatus {
        let client = self.ptr.lock().unwrap();

        let address_ptr = Box::into_raw(Box::new(address.clone())) as *mut c_void;

        let result;
        let out_error = Box::into_raw(Box::new(0));
        unsafe { result = check_online_status(client.0, address_ptr, out_error) }

        ContactOnlineStatus::from_byte(result as u8).expect("A valid u8 from FFI status")
    }

    async fn send_message(&self, receiver: TariAddress, message: String) {
        let client = self.ptr.lock().unwrap();

        let message_c_str = CString::new(message).unwrap();
        let message_c_char: *const c_char = CString::into_raw(message_c_str) as *const c_char;

        let receiver_ptr = Box::into_raw(Box::new(receiver)) as *mut c_void;
        let out_error = Box::into_raw(Box::new(0));

        unsafe {
            send_message(client.0, receiver_ptr, message_c_char, out_error);
        }
    }

    async fn get_all_messages(&self, address: &TariAddress) -> Vec<Message> {
        let client = self.ptr.lock().unwrap();

        let address_ptr = Box::into_raw(Box::new(address.clone())) as *mut c_void;

        let messages;
        unsafe {
            let out_error = Box::into_raw(Box::new(0));
            let all_messages = get_all_messages(client.0, address_ptr, out_error) as *mut Vec<Message>;
            messages = (*all_messages).clone();
        }

        messages
    }

    fn identity(&self) -> &NodeIdentity {
        &self.identity
    }
}

pub async fn spawn_ffi_chat_client(name: &str, seed_peers: Vec<Peer>) -> ChatFFI {
    let port = get_port(18000..18499).unwrap();
    let base_dir = get_base_dir()
        .join("ffi_chat_clients")
        .join(format!("port_{}", port))
        .join(name);

    let (identity, identity_path) = identity_file(port, &base_dir);
    let identity_path_c_str = CString::new(identity_path.into_os_string().into_string().unwrap()).unwrap();
    let identity_path_c_char: *const c_char = CString::into_raw(identity_path_c_str) as *const c_char;

    let config = test_config(&base_dir, &identity);
    let config_ptr = Box::into_raw(Box::new(config.clone())) as *mut c_void;

    let network = Network::LocalNet;
    let network_c_str = CString::new(network.to_string()).unwrap();
    let network_c_char: *const c_char = CString::into_raw(network_c_str) as *const c_char;

    let db_path = database::create_chat_storage(&config.datastore_path).unwrap();
    let db_path_c_str = CString::new(db_path.into_os_string().into_string().unwrap()).unwrap();
    let db_path_c_char: *const c_char = CString::into_raw(db_path_c_str) as *const c_char;
    database::create_peer_storage(&config.datastore_path);

    let seed_peers_ptr = Box::into_raw(Box::new(seed_peers)) as *mut c_void;

    let client_ptr;

    let out_error = Box::into_raw(Box::new(0));

    unsafe {
        client_ptr = create_chat_client(
            config_ptr,
            identity_path_c_char,
            db_path_c_char,
            seed_peers_ptr,
            network_c_char,
            out_error,
        );
    }

    ChatFFI {
        ptr: Arc::new(Mutex::new(PtrWrapper(client_ptr))),
        identity,
    }
}

fn test_config(base_dir: &PathBuf, identity: &NodeIdentity) -> P2pConfig {
    let mut config = P2pConfig {
        datastore_path: base_dir.clone(),
        dht: DhtConfig {
            database_url: DbConnectionUrl::file("dht.sqlite"),
            network_discovery: NetworkDiscoveryConfig {
                enabled: true,
                ..NetworkDiscoveryConfig::default()
            },
            saf: SafConfig {
                auto_request: true,
                ..Default::default()
            },
            ..DhtConfig::default_local_test()
        },
        transport: TransportConfig::new_tcp(TcpTransportConfig {
            listener_address: identity.first_public_address().expect("No public address"),
            ..TcpTransportConfig::default()
        }),
        allow_test_addresses: true,
        public_addresses: MultiaddrList::from(vec![identity.first_public_address().expect("No public address")]),
        user_agent: "tari/chat-client/0.0.1".to_string(),
        ..P2pConfig::default()
    };
    config.set_base_path(base_dir);
    config
}

fn identity_file(port: u64, base_dir: &PathBuf) -> (NodeIdentity, PathBuf) {
    let address = Multiaddr::from_str(&format!("/ip4/127.0.0.1/tcp/{}", port)).unwrap();
    let identity = NodeIdentity::random(&mut OsRng, address, PeerFeatures::COMMUNICATION_NODE);

    fs::create_dir_all(base_dir).unwrap();
    let path = base_dir.join(format!("{}.json", identity.node_id()));
    fs::write(&path, identity.to_json().unwrap()).unwrap();

    (identity, path)
}
