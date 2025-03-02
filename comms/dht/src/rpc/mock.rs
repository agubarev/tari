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

use tari_comms::protocol::rpc::{
    mock::{RpcMock, RpcMockMethodState},
    Request,
    RpcStatus,
    Streaming,
};

use crate::{
    proto::rpc::{GetCloserPeersRequest, GetPeersRequest, GetPeersResponse},
    rpc::DhtRpcService,
};

// TODO: This mock can be generated
#[derive(Debug, Clone, Default)]
pub struct DhtRpcServiceMock {
    pub get_closer_peers: RpcMockMethodState<GetCloserPeersRequest, Vec<GetPeersResponse>>,
    pub get_peers: RpcMockMethodState<GetPeersRequest, Vec<GetPeersResponse>>,
}

impl DhtRpcServiceMock {
    pub fn new() -> Self {
        Default::default()
    }
}

#[tari_comms::async_trait]
impl DhtRpcService for DhtRpcServiceMock {
    async fn get_closer_peers(
        &self,
        request: Request<GetCloserPeersRequest>,
    ) -> Result<Streaming<GetPeersResponse>, RpcStatus> {
        self.server_streaming(request, &self.get_closer_peers).await
    }

    async fn get_peers(&self, request: Request<GetPeersRequest>) -> Result<Streaming<GetPeersResponse>, RpcStatus> {
        self.server_streaming(request, &self.get_peers).await
    }
}

impl RpcMock for DhtRpcServiceMock {}
