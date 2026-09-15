// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Raft network layer with TCP transport for inter-node communication.

use std::collections::BTreeMap;
use std::sync::Arc;

use openraft::error::{InstallSnapshotError, NetworkError, RPCError, RaftError};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::BasicNode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

use crate::types::{NodeId, OntoRaftConfig};

/// Network for a single Raft peer connection via TCP.
pub struct OntoRaftNetwork {
    target: NodeId,
    nodes: Arc<RwLock<BTreeMap<NodeId, BasicNode>>>,
}

impl OntoRaftNetwork {
    pub fn new(target: NodeId, nodes: Arc<RwLock<BTreeMap<NodeId, BasicNode>>>) -> Self {
        Self { target, nodes }
    }

    /// Get the target node's address.
    async fn target_addr(&self) -> Result<String, NetworkError> {
        let nodes = self.nodes.read().await;
        nodes
            .get(&self.target)
            .map(|n| n.addr.clone())
            .ok_or_else(|| {
                NetworkError::new(&std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("node {} not found", self.target),
                ))
            })
    }

    /// Connection timeout for Raft RPCs (30 seconds).
    const RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Send a serialized request and receive a response via TCP.
    async fn send_rpc<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        request: &Req,
    ) -> Result<Resp, NetworkError> {
        let addr = self.target_addr().await?;
        let mut stream = tokio::time::timeout(Self::RPC_TIMEOUT, TcpStream::connect(&addr))
            .await
            .map_err(|_| {
                NetworkError::new(&std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("connection to {} timed out", addr),
                ))
            })?
            .map_err(|e| {
                NetworkError::new(&std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    format!("failed to connect to {}: {}", addr, e),
                ))
            })?;

        stream.set_nodelay(true).ok();

        // Serialize request
        let req_bytes = serde_json::to_vec(request).map_err(|e| {
            NetworkError::new(&std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("serialization error: {}", e),
            ))
        })?;

        // Send length prefix + data
        let len = (req_bytes.len() as u32).to_be_bytes();
        stream
            .write_all(&len)
            .await
            .map_err(|e| NetworkError::new(&e))?;
        stream
            .write_all(&req_bytes)
            .await
            .map_err(|e| NetworkError::new(&e))?;
        stream.flush().await.map_err(|e| NetworkError::new(&e))?;

        // Read response length
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| NetworkError::new(&e))?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;

        // Enforce message size limit (16 MB)
        const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;
        if resp_len > MAX_MESSAGE_SIZE {
            return Err(NetworkError::new(&std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "response too large: {} bytes (max {})",
                    resp_len, MAX_MESSAGE_SIZE
                ),
            )));
        }

        // Read response data
        let mut resp_buf = vec![0u8; resp_len];
        stream
            .read_exact(&mut resp_buf)
            .await
            .map_err(|e| NetworkError::new(&e))?;

        // Deserialize response
        serde_json::from_slice(&resp_buf).map_err(|e| {
            NetworkError::new(&std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("deserialization error: {}", e),
            ))
        })
    }
}

impl openraft::RaftNetwork<OntoRaftConfig> for OntoRaftNetwork {
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<OntoRaftConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        self.send_rpc(&req).await.map_err(RPCError::Network)
    }

    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<OntoRaftConfig>,
        _option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, BasicNode, openraft::error::RaftError<NodeId, InstallSnapshotError>>,
    > {
        self.send_rpc(&req).await.map_err(RPCError::Network)
    }

    async fn vote(
        &mut self,
        req: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        self.send_rpc(&req).await.map_err(RPCError::Network)
    }
}

/// Network factory that creates connections to peers.
pub struct OntoRaftNetworkFactory {
    pub nodes: Arc<RwLock<BTreeMap<NodeId, BasicNode>>>,
}

impl OntoRaftNetworkFactory {
    pub fn new(nodes: Arc<RwLock<BTreeMap<NodeId, BasicNode>>>) -> Self {
        Self { nodes }
    }
}

impl openraft::RaftNetworkFactory<OntoRaftConfig> for OntoRaftNetworkFactory {
    type Network = OntoRaftNetwork;

    async fn new_client(&mut self, target: NodeId, _node: &BasicNode) -> Self::Network {
        OntoRaftNetwork::new(target, self.nodes.clone())
    }
}

/// Raft TCP server that handles incoming Raft RPC requests.
pub struct RaftTcpServer {
    addr: String,
}

impl RaftTcpServer {
    pub fn new(addr: impl Into<String>) -> Self {
        Self { addr: addr.into() }
    }

    /// Start the Raft TCP server.
    pub async fn start(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind(&self.addr).await?;
        println!("Raft TCP server listening on {}", self.addr);

        loop {
            let (stream, peer) = listener.accept().await?;
            println!("Raft connection from {}", peer);

            tokio::spawn(async move {
                if let Err(e) = handle_raft_connection(stream).await {
                    eprintln!("Raft connection error: {}", e);
                }
            });
        }
    }
}

/// Handle a single Raft TCP connection.
async fn handle_raft_connection(
    mut stream: TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        // Read request length
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let req_len = u32::from_be_bytes(len_buf) as usize;

        // Enforce message size limit (16 MB)
        const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;
        if req_len > MAX_MESSAGE_SIZE {
            tracing::warn!("Rejected oversized Raft message: {} bytes", req_len);
            break;
        }

        // Read request data
        let mut req_buf = vec![0u8; req_len];
        stream.read_exact(&mut req_buf).await?;

        // Process request (placeholder - would dispatch to Raft in production)
        let response = serde_json::json!({
            "status": "ok",
            "message": "Raft RPC received"
        });
        let resp_bytes = serde_json::to_vec(&response)?;

        // Send response
        let len = (resp_bytes.len() as u32).to_be_bytes();
        stream.write_all(&len).await?;
        stream.write_all(&resp_bytes).await?;
        stream.flush().await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_factory_creation() {
        let nodes = Arc::new(RwLock::new(BTreeMap::new()));
        let factory = OntoRaftNetworkFactory::new(nodes);
        assert!(factory.nodes.blocking_read().is_empty());
    }
}
