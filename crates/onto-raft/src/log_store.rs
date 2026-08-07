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
