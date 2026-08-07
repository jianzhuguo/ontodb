//! Raft network layer for inter-node communication.

use std::collections::BTreeMap;
use std::sync::Arc;

use openraft::error::{InstallSnapshotError, NetworkError, RPCError, RaftError};
use openraft::network::RPCOption;
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest,
    InstallSnapshotResponse, VoteRequest, VoteResponse,
};
use openraft::BasicNode;
use tokio::sync::RwLock;

use crate::types::{NodeId, OntoRaftConfig};

/// Network for a single Raft peer connection.
pub struct OntoRaftNetwork {
    target: NodeId,
    _nodes: Arc<RwLock<BTreeMap<NodeId, BasicNode>>>,
}

impl OntoRaftNetwork {
    pub fn new(target: NodeId, nodes: Arc<RwLock<BTreeMap<NodeId, BasicNode>>>) -> Self {
        Self { target, _nodes: nodes }
    }
}

impl openraft::RaftNetwork<OntoRaftConfig> for OntoRaftNetwork {
    async fn append_entries(
        &mut self,
        _req: AppendEntriesRequest<OntoRaftConfig>,
        _option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        // TODO: Send over TCP to self.target
        Err(RPCError::Network(NetworkError::new(&std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            format!("TCP transport not yet implemented for node {}", self.target),
        ))))
    }

    async fn install_snapshot(
        &mut self,
        _req: InstallSnapshotRequest<OntoRaftConfig>,
        _option: RPCOption,
    ) -> Result<InstallSnapshotResponse<NodeId>, RPCError<NodeId, BasicNode, openraft::error::RaftError<NodeId, InstallSnapshotError>>> {
        Err(RPCError::Network(NetworkError::new(&std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            format!("TCP transport not yet implemented for node {}", self.target),
        ))))
    }

    async fn vote(
        &mut self,
        _req: VoteRequest<NodeId>,
        _option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, BasicNode, RaftError<NodeId>>> {
        Err(RPCError::Network(NetworkError::new(&std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            format!("TCP transport not yet implemented for node {}", self.target),
        ))))
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
