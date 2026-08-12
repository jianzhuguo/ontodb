//! Combined Raft storage implementing RaftStorage for use with openraft::storage::Adaptor.
//!
//! This combines log storage and state machine into a single struct.
//! In production, the state machine data would be backed by LsmEngine.

use std::collections::BTreeMap;

use openraft::storage::{RaftLogReader, RaftSnapshotBuilder, RaftStorage};
use openraft::{
    Entry, EntryPayload, LogId, LogState, Snapshot, SnapshotMeta,
    StorageError, StoredMembership, Vote,
};

use crate::types::{NodeId, OntoRaftConfig, OntoRequest, OntoResponse};

/// Combined Raft storage for OntoDB.
///
/// Holds both the Raft log and the state machine data.
/// Use `openraft::storage::Adaptor::new(store)` to get implementors
/// of RaftLogStorage and RaftStateMachine.
pub struct OntoRaftStore {
    /// Persisted vote.
    vote: Option<Vote<NodeId>>,
    /// Log entries indexed by log index.
    log: BTreeMap<u64, Entry<OntoRaftConfig>>,
    /// Last purged log id.
    purged: Option<LogId<NodeId>>,
    /// State machine: KV store.
    sm_data: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Last applied log id in state machine.
    last_applied: Option<LogId<NodeId>>,
    /// Last applied membership.
    last_membership: StoredMembership<NodeId, openraft::BasicNode>,
    /// Current snapshot.
    snapshot: Option<Vec<u8>>,
    /// Snapshot metadata.
    snapshot_meta: Option<SnapshotMeta<NodeId, openraft::BasicNode>>,
}

impl Default for OntoRaftStore {
    fn default() -> Self {
        Self::new()
    }
}

impl OntoRaftStore {
    pub fn new() -> Self {
        Self {
            vote: None,
            log: BTreeMap::new(),
            purged: None,
            sm_data: BTreeMap::new(),
            last_applied: None,
            last_membership: StoredMembership::default(),
            snapshot: None,
            snapshot_meta: None,
        }
    }

    /// Apply a single request to the state machine.
    fn apply_request(&mut self, req: OntoRequest) -> OntoResponse {
        match req {
            OntoRequest::Put { key, value } => {
                self.sm_data.insert(key, value);
                OntoResponse::Success(None)
            }
            OntoRequest::Delete { key } => {
                self.sm_data.remove(&key);
                OntoResponse::Success(None)
            }
            OntoRequest::Batch { ops } => {
                let mut count = 0;
                for op in ops {
                    self.apply_request(op);
                    count += 1;
                }
                OntoResponse::Success(Some(format!("{} ops applied", count)))
            }
            OntoRequest::ConfigChange { config_json } => {
                // Store config in the KV store under a reserved key
                self.sm_data.insert(b"__config__".to_vec(), config_json);
                OntoResponse::Success(Some("config applied".to_string()))
            }
        }
    }
}

/// RaftLogReader implementation — reads log entries.
impl RaftLogReader<OntoRaftConfig> for OntoRaftStore {
    async fn try_get_log_entries<RB: std::ops::RangeBounds<u64> + Clone + Send>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<OntoRaftConfig>>, StorageError<NodeId>> {
        let entries: Vec<_> = self.log.range(range).map(|(_, e)| e.clone()).collect();
        Ok(entries)
    }
}

/// RaftSnapshotBuilder implementation — builds snapshots.
impl RaftSnapshotBuilder<OntoRaftConfig> for OntoRaftStore {
    async fn build_snapshot(&mut self) -> Result<Snapshot<OntoRaftConfig>, StorageError<NodeId>> {
        let data = serde_json::to_vec(&self.sm_data)
            .map_err(|e| StorageError::IO {
                source: openraft::StorageIOError::new(
                    openraft::ErrorSubject::Snapshot(None),
                    openraft::ErrorVerb::Write,
                    &std::io::Error::other(e.to_string()),
                ),
            })?;

        let meta = SnapshotMeta {
            last_log_id: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot_id: format!("{:?}", self.last_applied),
        };

        self.snapshot = Some(data.clone());
        self.snapshot_meta = Some(meta.clone());

        Ok(Snapshot {
            meta,
            snapshot: Box::new(std::io::Cursor::new(data)),
        })
    }
}

/// RaftStorage implementation — the combined storage trait.
///
/// This is used with `openraft::storage::Adaptor` to provide
/// RaftLogStorage and RaftStateMachine implementations.
impl RaftStorage<OntoRaftConfig> for OntoRaftStore {
    type LogReader = Self;
    type SnapshotBuilder = Self;

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.vote = Some(*vote);
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        Ok(self.vote)
    }

    async fn get_log_state(&mut self) -> Result<LogState<OntoRaftConfig>, StorageError<NodeId>> {
        let last = self.log.iter().next_back().map(|(_, e)| e.log_id);
        Ok(LogState {
            last_purged_log_id: self.purged,
            last_log_id: last,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        // For in-memory store, return a clone
        OntoRaftStore {
            vote: self.vote,
            log: self.log.clone(),
            purged: self.purged,
            sm_data: self.sm_data.clone(),
            last_applied: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot: self.snapshot.clone(),
            snapshot_meta: self.snapshot_meta.clone(),
        }
    }

    async fn append_to_log<I>(&mut self, entries: I) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<OntoRaftConfig>> + openraft::OptionalSend,
    {
        for entry in entries {
            self.log.insert(entry.log_id.index, entry);
        }
        Ok(())
    }

    async fn delete_conflict_logs_since(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        let keys: Vec<u64> = self.log.range(log_id.index..).map(|(k, _)| *k).collect();
        for key in keys {
            self.log.remove(&key);
        }
        Ok(())
    }

    async fn purge_logs_upto(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        let keys: Vec<u64> = self.log.range(..=log_id.index).map(|(k, _)| *k).collect();
        for key in keys {
            self.log.remove(&key);
        }
        self.purged = Some(log_id);
        Ok(())
    }

    async fn last_applied_state(
        &mut self,
    ) -> Result<(Option<LogId<NodeId>>, StoredMembership<NodeId, openraft::BasicNode>), StorageError<NodeId>> {
        Ok((self.last_applied, self.last_membership.clone()))
    }

    async fn apply_to_state_machine(
        &mut self,
        entries: &[Entry<OntoRaftConfig>],
    ) -> Result<Vec<OntoResponse>, StorageError<NodeId>> {
        let mut responses = Vec::new();
        for entry in entries {
            self.last_applied = Some(entry.log_id);
            match &entry.payload {
                EntryPayload::Blank => {
                    responses.push(OntoResponse::Success(None));
                }
                EntryPayload::Normal(req) => {
                    let resp = self.apply_request(req.clone());
                    responses.push(resp);
                }
                EntryPayload::Membership(mem) => {
                    self.last_membership = StoredMembership::new(Some(entry.log_id), mem.clone());
                    responses.push(OntoResponse::Success(None));
                }
            }
        }
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        OntoRaftStore {
            vote: self.vote,
            log: self.log.clone(),
            purged: self.purged,
            sm_data: self.sm_data.clone(),
            last_applied: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot: None,
            snapshot_meta: None,
        }
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<std::io::Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(std::io::Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, openraft::BasicNode>,
        snapshot: Box<std::io::Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let data = snapshot.into_inner();
        if let Ok(sm_data) = serde_json::from_slice::<BTreeMap<Vec<u8>, Vec<u8>>>(&data) {
            self.sm_data = sm_data;
        }
        self.last_applied = meta.last_log_id;
        self.last_membership = StoredMembership::new(meta.last_log_id, meta.last_membership.membership().clone());
        self.snapshot = Some(data);
        self.snapshot_meta = Some(meta.clone());
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<OntoRaftConfig>>, StorageError<NodeId>> {
        match (&self.snapshot, &self.snapshot_meta) {
            (Some(data), Some(meta)) => Ok(Some(Snapshot {
                meta: meta.clone(),
                snapshot: Box::new(std::io::Cursor::new(data.clone())),
            })),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openraft::storage::RaftLogReader;

    #[tokio::test]
    async fn test_store_apply_put_and_get() {
        let mut store = OntoRaftStore::new();

        let entry = Entry {
            log_id: LogId::new(openraft::CommittedLeaderId::new(1, 1), 1),
            payload: EntryPayload::Normal(OntoRequest::Put {
                key: b"hello".to_vec(),
                value: b"world".to_vec(),
            }),
        };

        let responses = store.apply_to_state_machine(&[entry]).await.unwrap();
        assert_eq!(responses.len(), 1);
        assert!(matches!(&responses[0], OntoResponse::Success(None)));
        assert_eq!(store.sm_data.get(b"hello".as_slice()), Some(&b"world".to_vec()));
    }

    #[tokio::test]
    async fn test_store_apply_delete() {
        let mut store = OntoRaftStore::new();
        store.sm_data.insert(b"key1".to_vec(), b"val1".to_vec());

        let entry = Entry {
            log_id: LogId::new(openraft::CommittedLeaderId::new(1, 1), 1),
            payload: EntryPayload::Normal(OntoRequest::Delete {
                key: b"key1".to_vec(),
            }),
        };

        store.apply_to_state_machine(&[entry]).await.unwrap();
        assert!(store.sm_data.get(b"key1".as_slice()).is_none());
    }

    #[tokio::test]
    async fn test_store_log_append_and_read() {
        let mut store = OntoRaftStore::new();

        let entry = Entry {
            log_id: LogId::new(openraft::CommittedLeaderId::new(1, 1), 1),
            payload: EntryPayload::Normal(OntoRequest::Put {
                key: b"k".to_vec(),
                value: b"v".to_vec(),
            }),
        };

        store.append_to_log(vec![entry]).await.unwrap();

        let entries = store.try_get_log_entries(1..2).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].log_id.index, 1);
    }

    #[tokio::test]
    async fn test_store_batch_apply() {
        let mut store = OntoRaftStore::new();

        let entry = Entry {
            log_id: LogId::new(openraft::CommittedLeaderId::new(1, 1), 1),
            payload: EntryPayload::Normal(OntoRequest::Batch {
                ops: vec![
                    OntoRequest::Put { key: b"a".to_vec(), value: b"1".to_vec() },
                    OntoRequest::Put { key: b"b".to_vec(), value: b"2".to_vec() },
                ],
            }),
        };

        let responses = store.apply_to_state_machine(&[entry]).await.unwrap();
        assert!(matches!(&responses[0], OntoResponse::Success(_)));
        assert_eq!(store.sm_data.len(), 2);
    }
}
