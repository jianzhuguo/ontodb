//! Raft state machine that applies committed log entries to the OntoDB storage engine.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use openraft::storage::RaftStateMachine;
use openraft::{Entry, EntryPayload, LogId, Snapshot, SnapshotMeta, StorageError, StoredMembership};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::types::{OntoRaftConfig, OntoRequest, OntoResponse};

/// The state machine for OntoDB Raft replication.
///
/// Applies committed write operations (PUT/DELETE) to a key-value store.
/// In production, this would wrap the main LsmEngine.
pub struct OntoStateMachine {
    /// The KV store — in production, this would be an LsmEngine handle.
    /// For now, we use a BTreeMap for simplicity and testability.
    pub data: BTreeMap<Vec<u8>, Vec<u8>>,

    /// Last applied log id.
    pub last_applied: Option<LogId<u64>>,

    /// Last applied membership.
    pub last_membership: StoredMembership<u64, openraft::BasicNode>,

    /// Snapshot bytes (latest).
    snapshot: Option<Vec<u8>>,

    /// Shared config store for cross-node config synchronization.
    pub config_store: Option<crate::config_sync::SharedConfigStore>,
}

impl OntoStateMachine {
    pub fn new() -> Self {
        Self {
            data: BTreeMap::new(),
            last_applied: None,
            last_membership: StoredMembership::default(),
            snapshot: None,
            config_store: None,
        }
    }

    /// Set the shared config store for config sync.
    pub fn with_config_store(mut self, store: crate::config_sync::SharedConfigStore) -> Self {
        self.config_store = Some(store);
        self
    }

    /// Apply a single request to the state machine.
    fn apply_request(&mut self, req: OntoRequest) -> OntoResponse {
        match req {
            OntoRequest::Put { key, value } => {
                self.data.insert(key, value);
                OntoResponse::Success(None)
            }
            OntoRequest::Delete { key } => {
                self.data.remove(&key);
                OntoResponse::Success(None)
            }
            OntoRequest::Batch { ops } => {
                let mut responses = Vec::new();
                for op in ops {
                    responses.push(self.apply_request(op));
                }
                for resp in &responses {
                    if matches!(resp, OntoResponse::Error(_)) {
                        return resp.clone();
                    }
                }
                OntoResponse::Success(Some(format!("{} operations applied", responses.len())))
            }
            OntoRequest::ConfigChange { config_json } => {
                // Apply config change to the shared config store
                if let Some(ref store) = self.config_store {
                    match store.apply(&config_json) {
                        Ok(()) => OntoResponse::Success(Some("config applied via Raft".to_string())),
                        Err(e) => OntoResponse::Error(e),
                    }
                } else {
                    // No config store — just store the raw JSON
                    self.data.insert(b"__config__".to_vec(), config_json);
                    OntoResponse::Success(Some("config stored (no shared store)".to_string()))
                }
            }
        }
    }
}

impl RaftStateMachine<OntoRaftConfig> for OntoStateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<u64>>, StoredMembership<u64, openraft::BasicNode>), StorageError<u64>> {
        Ok((self.last_applied, self.last_membership.clone()))
    }

    async fn apply<I>(
        &mut self,
        entries: I,
    ) -> Result<Vec<OntoResponse>, StorageError<u64>>
    where
        I: IntoIterator<Item = Entry<OntoRaftConfig>> + Send,
    {
        let mut responses = Vec::new();

        for entry in entries {
            self.last_applied = Some(entry.log_id);

            match entry.payload {
                EntryPayload::Blank => {
                    responses.push(OntoResponse::Success(None));
                }
                EntryPayload::Normal(req) => {
                    let resp = self.apply_request(req);
                    responses.push(resp);
                }
                EntryPayload::Membership(ref mem) => {
                    self.last_membership = StoredMembership::new(Some(entry.log_id), mem.clone());
                    responses.push(OntoResponse::Success(None));
                }
            }
        }

        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        // Return a clone for snapshot building
        OntoStateMachine {
            data: self.data.clone(),
            last_applied: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot: None,
        }
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Cursor<Vec<u8>>, StorageError<u64>> {
        Ok(Cursor::new(Vec::new()))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<u64, openraft::BasicNode>,
        snapshot: Vec<u8>,
    ) -> Result<(), StorageError<u64>> {
        // Deserialize snapshot and replace state
        if let Ok(data) = serde_json::from_slice::<BTreeMap<Vec<u8>, Vec<u8>>>(&snapshot) {
            self.data = data;
        }
        self.last_applied = meta.last_log_id;
        self.last_membership = StoredMembership::new(
            meta.last_log_id,
            meta.last_membership.membership().clone(),
        );
        self.snapshot = Some(snapshot);
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<OntoRaftConfig>>, StorageError<u64>> {
        if let Some(ref data) = self.snapshot {
            let meta = SnapshotMeta {
                last_log_id: self.last_applied,
                last_membership: self.last_membership.clone(),
                snapshot_id: format!("{:?}", self.last_applied),
            };
            Ok(Some(Snapshot {
                meta,
                snapshot: Box::new(Cursor::new(data.clone())),
            }))
        } else {
            Ok(None)
        }
    }
}

impl RaftStateMachine<OntoRaftConfig> for &mut OntoStateMachine {
    type SnapshotBuilder = OntoStateMachine;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<u64>>, StoredMembership<u64, openraft::BasicNode>), StorageError<u64>> {
        (**self).applied_state().await
    }

    async fn apply<I>(
        &mut self,
        entries: I,
    ) -> Result<Vec<OntoResponse>, StorageError<u64>>
    where
        I: IntoIterator<Item = Entry<OntoRaftConfig>> + Send,
    {
        (**self).apply(entries).await
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        OntoStateMachine {
            data: self.data.clone(),
            last_applied: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot: None,
        }
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Cursor<Vec<u8>>, StorageError<u64>> {
        Ok(Cursor::new(Vec::new()))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<u64, openraft::BasicNode>,
        snapshot: Vec<u8>,
    ) -> Result<(), StorageError<u64>> {
        if let Ok(data) = serde_json::from_slice::<BTreeMap<Vec<u8>, Vec<u8>>>(&snapshot) {
            self.data = data;
        }
        self.last_applied = meta.last_log_id;
        self.last_membership = StoredMembership::new(
            meta.last_log_id,
            meta.last_membership.membership().clone(),
        );
        self.snapshot = Some(snapshot);
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<OntoRaftConfig>>, StorageError<u64>> {
        if let Some(ref data) = self.snapshot {
            let meta = SnapshotMeta {
                last_log_id: self.last_applied,
                last_membership: self.last_membership.clone(),
                snapshot_id: format!("{:?}", self.last_applied),
            };
            Ok(Some(Snapshot {
                meta,
                snapshot: Box::new(Cursor::new(data.clone())),
            }))
        } else {
            Ok(None)
        }
    }
}
