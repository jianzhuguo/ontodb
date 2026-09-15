// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Raft log storage backed by a dedicated LSM engine.
//!
//! Log entries are stored with keys like `log/<index>` and metadata
//! under `meta/vote` and `meta/hard_state`.

use std::collections::BTreeMap;
use std::ops::RangeBounds;
use std::sync::Arc;

use openraft::storage::RaftLogStorage;
use openraft::{
    Entry, LogId, LogState, RaftLogReader, Vote,
};
use tokio::sync::RwLock;

use crate::error::RaftError;
use crate::types::OntoRaftConfig;

/// In-memory Raft log storage.
///
/// For production, this should be backed by persistent storage (WAL + SSTable).
/// This implementation keeps all log entries in memory for simplicity.
/// A production version would persist to disk via the LSM engine.
pub struct OntoLogStore {
    /// Committed log id.
    committed: Option<LogId<u64>>,

    /// The vote.
    vote: Option<Vote<u64>>,

    /// Log entries indexed by log index.
    log: BTreeMap<u64, Entry<OntoRaftConfig>>,

    /// The last purged log id.
    purged: Option<LogId<u64>>,
}

impl OntoLogStore {
    pub fn new() -> Self {
        Self {
            committed: None,
            vote: None,
            log: BTreeMap::new(),
            purged: None,
        }
    }
}

impl RaftLogReader<OntoRaftConfig> for OntoLogStore {
    async fn get_log_state(&mut self) -> Result<LogState<OntoRaftConfig>, openraft::StorageError<u64>> {
        let last = self.log.iter().next_back().map(|(_, entry)| entry.log_id);
        let last_purged = self.purged;

        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id: last,
        })
    }

    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Send + Sync>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<OntoRaftConfig>>, openraft::StorageError<u64>> {
        let entries: Vec<_> = self.log.range(range).map(|(_, e)| e.clone()).collect();
        Ok(entries)
    }
}

impl RaftLogStorage<OntoRaftConfig> for OntoLogStore {
    type LogReader = Self;

    async fn get_log_reader(&mut self) -> Self::LogReader {
        // Return a copy — for an in-memory store this is fine.
        // A persistent store would return a read handle.
        OntoLogStore {
            committed: self.committed,
            vote: self.vote,
            log: self.log.clone(),
            purged: self.purged,
        }
    }

    async fn save_vote(&mut self, vote: &Vote<u64>) -> Result<(), openraft::StorageError<u64>> {
        self.vote = Some(*vote);
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<u64>>, openraft::StorageError<u64>> {
        Ok(self.vote)
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: openraft::storage::LogFlushed<OntoRaftConfig>,
    ) -> Result<(), openraft::StorageError<u64>>
    where
        I: IntoIterator<Item = Entry<OntoRaftConfig>> + Send,
    {
        for entry in entries {
            self.log.insert(entry.log_id.index, entry);
        }
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<u64>) -> Result<(), openraft::StorageError<u64>> {
        // Remove all entries with index > log_id.index
        let keys_to_remove: Vec<u64> = self.log
            .range(log_id.index + 1..)
            .map(|(k, _)| *k)
            .collect();
        for key in keys_to_remove {
            self.log.remove(&key);
        }
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId<u64>) -> Result<(), openraft::StorageError<u64>> {
        // Remove all entries with index <= log_id.index
        let keys_to_remove: Vec<u64> = self.log
            .range(..=log_id.index)
            .map(|(k, _)| *k)
            .collect();
        for key in keys_to_remove {
            self.log.remove(&key);
        }
        self.purged = Some(log_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openraft::storage::RaftLogStorage;
    use openraft::{CommittedLeaderId, Entry, EntryPayload};
    use crate::types::OntoRequest;

    fn make_entry(index: u64, term: u64, key: &[u8], value: &[u8]) -> Entry<OntoRaftConfig> {
        Entry {
            log_id: LogId::new(CommittedLeaderId::new(term, 1), index),
            payload: EntryPayload::Normal(OntoRequest::Put {
                key: key.to_vec(),
                value: value.to_vec(),
            }),
        }
    }

    #[tokio::test]
    async fn test_log_store_empty_state() {
        let mut store = OntoLogStore::new();
        let state = store.get_log_state().await.unwrap();
        assert!(state.last_log_id.is_none());
        assert!(state.last_purged_log_id.is_none());
    }

    #[tokio::test]
    async fn test_log_store_append_and_read() {
        let mut store = OntoLogStore::new();
        let entries = vec![
            make_entry(1, 1, b"k1", b"v1"),
            make_entry(2, 1, b"k2", b"v2"),
            make_entry(3, 1, b"k3", b"v3"),
        ];
        let cb = openraft::storage::LogFlushed::new(None);
        store.append(entries, cb).await.unwrap();

        let state = store.get_log_state().await.unwrap();
        assert!(state.last_log_id.is_some());
        assert_eq!(state.last_log_id.unwrap().index, 3);

        let read_entries = store.try_get_log_entries(1..4).await.unwrap();
        assert_eq!(read_entries.len(), 3);
        assert_eq!(read_entries[0].log_id.index, 1);
        assert_eq!(read_entries[2].log_id.index, 3);
    }

    #[tokio::test]
    async fn test_log_store_append_range_query() {
        let mut store = OntoLogStore::new();
        let entries = vec![
            make_entry(1, 1, b"a", b"1"),
            make_entry(2, 1, b"b", b"2"),
            make_entry(3, 1, b"c", b"3"),
            make_entry(4, 1, b"d", b"4"),
            make_entry(5, 1, b"e", b"5"),
        ];
        let cb = openraft::storage::LogFlushed::new(None);
        store.append(entries, cb).await.unwrap();

        // Range 2..4 should return entries 2, 3
        let subset = store.try_get_log_entries(2..4).await.unwrap();
        assert_eq!(subset.len(), 2);
        assert_eq!(subset[0].log_id.index, 2);
        assert_eq!(subset[1].log_id.index, 3);
    }

    #[tokio::test]
    async fn test_log_store_truncate() {
        let mut store = OntoLogStore::new();
        let entries = vec![
            make_entry(1, 1, b"a", b"1"),
            make_entry(2, 1, b"b", b"2"),
            make_entry(3, 1, b"c", b"3"),
            make_entry(4, 1, b"d", b"4"),
            make_entry(5, 1, b"e", b"5"),
        ];
        let cb = openraft::storage::LogFlushed::new(None);
        store.append(entries, cb).await.unwrap();

        // Truncate after index 2 (remove 3, 4, 5)
        store.truncate(LogId::new(CommittedLeaderId::new(1, 1), 2)).await.unwrap();

        let state = store.get_log_state().await.unwrap();
        assert_eq!(state.last_log_id.unwrap().index, 2);

        let remaining = store.try_get_log_entries(1..10).await.unwrap();
        assert_eq!(remaining.len(), 2);
    }

    #[tokio::test]
    async fn test_log_store_purge() {
        let mut store = OntoLogStore::new();
        let entries = vec![
            make_entry(1, 1, b"a", b"1"),
            make_entry(2, 1, b"b", b"2"),
            make_entry(3, 1, b"c", b"3"),
            make_entry(4, 1, b"d", b"4"),
        ];
        let cb = openraft::storage::LogFlushed::new(None);
        store.append(entries, cb).await.unwrap();

        // Purge entries <= index 2
        store.purge(LogId::new(CommittedLeaderId::new(1, 1), 2)).await.unwrap();

        let state = store.get_log_state().await.unwrap();
        assert_eq!(state.last_purged_log_id.unwrap().index, 2);

        let remaining = store.try_get_log_entries(1..10).await.unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].log_id.index, 3);
    }

    #[tokio::test]
    async fn test_log_store_vote_save_read() {
        let mut store = OntoLogStore::new();

        // Initially no vote
        assert!(store.read_vote().await.unwrap().is_none());

        // Save a vote
        let vote = Vote::new(1, 1);
        store.save_vote(&vote).await.unwrap();

        // Read it back
        let read_vote = store.read_vote().await.unwrap().unwrap();
        assert_eq!(read_vote.leader_id.node_id, 1);
        assert_eq!(read_vote.leader_id.term, 1);
    }

    #[tokio::test]
    async fn test_log_store_get_reader() {
        let mut store = OntoLogStore::new();
        let entries = vec![make_entry(1, 1, b"k", b"v")];
        let cb = openraft::storage::LogFlushed::new(None);
        store.append(entries, cb).await.unwrap();

        let mut reader = store.get_log_reader().await;
        let state = reader.get_log_state().await.unwrap();
        assert_eq!(state.last_log_id.unwrap().index, 1);
    }
}
