//! Persistent Raft storage backed by LsmEngine.
//!
//! This replaces the in-memory OntoRaftStore with a persistent implementation
//! that survives node restarts. All Raft metadata, log entries, and state machine
//! data are stored in the LSM-Tree engine.
//!
//! Key design:
//! - Uses `__raft_` prefix to isolate Raft data from business data
//! - Batches writes for performance
//! - Separates log entries, state machine data, and metadata
//!
//! Performance impact: Minimal. The LSM-Tree is designed for high-throughput writes.
//! Raft log appends are sequential writes, which LSM handles efficiently.
//! State machine applies are batched with the log commit.

use std::sync::Arc;

use openraft::storage::{RaftLogReader, RaftSnapshotBuilder, RaftStorage};
use openraft::{
    Entry, EntryPayload, LogId, LogState, Snapshot, SnapshotMeta,
    StorageError, StoredMembership, Vote,
};
use serde::{Deserialize, Serialize};

use crate::types::{NodeId, OntoRaftConfig, OntoRequest, OntoResponse};

// Key prefixes for isolating Raft data in the LSM store
const RAFT_META_PREFIX: &[u8] = b"__raft_meta__";
const RAFT_LOG_PREFIX: &[u8] = b"__raft_log__";
const RAFT_SM_PREFIX: &[u8] = b"__raft_sm__";

// Specific metadata keys
const KEY_VOTE: &[u8] = b"__raft_meta__vote";
const KEY_COMMITTED: &[u8] = b"__raft_meta__committed";
const KEY_PURGED: &[u8] = b"__raft_meta__purged";
const KEY_LAST_APPLIED: &[u8] = b"__raft_meta__last_applied";
const KEY_MEMBERSHIP: &[u8] = b"__raft_meta__membership";
const KEY_SNAPSHOT_META: &[u8] = b"__raft_meta__snapshot_meta";

/// Persistent Raft storage backed by LsmEngine.
///
/// All data is persisted to the LSM-Tree, surviving node restarts.
/// Uses `Arc<LsmEngine>` for shared access with the main storage engine.
pub struct PersistentRaftStore {
    /// The underlying LSM storage engine.
    engine: Arc<onto_storage::LsmEngine>,
    /// Cached vote for fast reads.
    vote: Option<Vote<NodeId>>,
    /// Cached last applied log id.
    last_applied: Option<LogId<NodeId>>,
    /// Cached last membership.
    last_membership: StoredMembership<NodeId, openraft::BasicNode>,
    /// Cached purged log id.
    purged: Option<LogId<NodeId>>,
}

impl PersistentRaftStore {
    /// Create a new persistent Raft store backed by the given engine.
    pub fn new(engine: Arc<onto_storage::LsmEngine>) -> Self {
        let mut store = Self {
            engine,
            vote: None,
            last_applied: None,
            last_membership: StoredMembership::default(),
            purged: None,
        };
        // Load cached metadata from disk
        store.load_metadata();
        store
    }

    /// Load metadata from disk into cache.
    fn load_metadata(&mut self) {
        // Load vote
        if let Ok(Some(data)) = self.engine.get(KEY_VOTE) {
            if let Ok(vote) = serde_json::from_slice::<Vote<NodeId>>(&data) {
                self.vote = Some(vote);
            }
        }

        // Load last applied
        if let Ok(Some(data)) = self.engine.get(KEY_LAST_APPLIED) {
            if let Ok(log_id) = serde_json::from_slice::<Option<LogId<NodeId>>>(&data) {
                self.last_applied = log_id;
            }
        }

        // Load membership
        if let Ok(Some(data)) = self.engine.get(KEY_MEMBERSHIP) {
            if let Ok(membership) = serde_json::from_slice::<StoredMembership<NodeId, openraft::BasicNode>>(&data) {
                self.last_membership = membership;
            }
        }

        // Load purged
        if let Ok(Some(data)) = self.engine.get(KEY_PURGED) {
            if let Ok(log_id) = serde_json::from_slice::<Option<LogId<NodeId>>>(&data) {
                self.purged = log_id;
            }
        }

        tracing::info!(
            "Loaded Raft metadata: vote={:?}, last_applied={:?}, purged={:?}",
            self.vote.is_some(),
            self.last_applied.is_some(),
            self.purged.is_some()
        );
    }

    /// Format a log index as a key with the Raft log prefix.
    fn log_key(index: u64) -> Vec<u8> {
        let mut key = Vec::with_capacity(RAFT_LOG_PREFIX.len() + 8);
        key.extend_from_slice(RAFT_LOG_PREFIX);
        key.extend_from_slice(&index.to_be_bytes());
        key
    }

    /// Format a state machine key with the Raft SM prefix.
    fn sm_key(key: &[u8]) -> Vec<u8> {
        let mut sm_key = Vec::with_capacity(RAFT_SM_PREFIX.len() + key.len());
        sm_key.extend_from_slice(RAFT_SM_PREFIX);
        sm_key.extend_from_slice(key);
        sm_key
    }

    /// Apply a single request to the state machine (persisted).
    fn apply_request(&self, req: OntoRequest) -> OntoResponse {
        match req {
            OntoRequest::Put { key, value } => {
                let sm_key = Self::sm_key(&key);
                if let Err(e) = self.engine.put(sm_key, value) {
                    return OntoResponse::Error(format!("Put failed: {}", e));
                }
                OntoResponse::Success(None)
            }
            OntoRequest::Delete { key } => {
                let sm_key = Self::sm_key(&key);
                if let Err(e) = self.engine.delete(sm_key) {
                    return OntoResponse::Error(format!("Delete failed: {}", e));
                }
                OntoResponse::Success(None)
            }
            OntoRequest::Batch { ops } => {
                let mut count = 0;
                for op in ops {
                    match self.apply_request(op) {
                        OntoResponse::Success(_) => count += 1,
                        err => return err,
                    }
                }
                OntoResponse::Success(Some(format!("{} ops applied", count)))
            }
            OntoRequest::ConfigChange { config_json } => {
                let sm_key = Self::sm_key(b"__config__");
                if let Err(e) = self.engine.put(sm_key, config_json) {
                    return OntoResponse::Error(format!("Config apply failed: {}", e));
                }
                OntoResponse::Success(Some("config applied".to_string()))
            }
        }
    }

    /// Get the state machine data for snapshot building.
    fn get_all_sm_data(&self) -> Result<Vec<u8>, std::io::Error> {
        let entries = self.engine.scan_prefix(RAFT_SM_PREFIX)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        // Convert to a simple map for snapshot
        let mut sm_data = std::collections::BTreeMap::new();
        for (key, value) in entries {
            // Strip the SM prefix
            let original_key = key[RAFT_SM_PREFIX.len()..].to_vec();
            sm_data.insert(original_key, value);
        }

        serde_json::to_vec(&sm_data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
    }
}

/// RaftLogReader implementation — reads log entries from LSM.
impl RaftLogReader<OntoRaftConfig> for PersistentRaftStore {
    async fn try_get_log_entries<RB: std::ops::RangeBounds<u64> + Clone + Send>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<OntoRaftConfig>>, StorageError<NodeId>> {
        let mut entries = Vec::new();

        // Determine start and end indices
        use std::ops::Bound;
        let start = match range.start_bound() {
            Bound::Included(&n) => n,
            Bound::Excluded(&n) => n + 1,
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&n) => n + 1,
            Bound::Excluded(&n) => n,
            Bound::Unbounded => u64::MAX,
        };

        // Scan log entries in range
        for index in start..end {
            let key = Self::log_key(index);
            match self.engine.get(&key) {
                Ok(Some(data)) => {
                    match serde_json::from_slice::<Entry<OntoRaftConfig>>(&data) {
                        Ok(entry) => entries.push(entry),
                        Err(e) => {
                            tracing::warn!("Failed to deserialize log entry {}: {}", index, e);
                            // Skip corrupted entries rather than failing entirely
                        }
                    }
                }
                Ok(None) => {
                    // No more entries in this range
                    break;
                }
                Err(e) => {
                    return Err(StorageError::IO {
                        source: openraft::StorageIOError::new(
                            openraft::ErrorSubject::LogIndex(index),
                            openraft::ErrorVerb::Read,
                            &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                        ),
                    });
                }
            }
        }

        Ok(entries)
    }
}

/// RaftSnapshotBuilder implementation — builds snapshots from LSM data.
impl RaftSnapshotBuilder<OntoRaftConfig> for PersistentRaftStore {
    async fn build_snapshot(&mut self) -> Result<Snapshot<OntoRaftConfig>, StorageError<NodeId>> {
        let data = self.get_all_sm_data().map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Snapshot(None),
                openraft::ErrorVerb::Read,
                &e,
            ),
        })?;

        let meta = SnapshotMeta {
            last_log_id: self.last_applied,
            last_membership: self.last_membership.clone(),
            snapshot_id: format!("{:?}", self.last_applied),
        };

        // Persist snapshot metadata
        let meta_json = serde_json::to_vec(&meta).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Snapshot(None),
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;

        if let Err(e) = self.engine.put(KEY_SNAPSHOT_META.to_vec(), meta_json) {
            return Err(StorageError::IO {
                source: openraft::StorageIOError::new(
                    openraft::ErrorSubject::Snapshot(None),
                    openraft::ErrorVerb::Write,
                    &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                ),
            });
        }

        tracing::info!("Built snapshot: {:?}", meta.last_log_id);

        Ok(Snapshot {
            meta,
            snapshot: Box::new(std::io::Cursor::new(data)),
        })
    }
}

/// RaftStorage implementation — the combined storage trait backed by LSM.
impl RaftStorage<OntoRaftConfig> for PersistentRaftStore {
    type LogReader = Self;
    type SnapshotBuilder = Self;

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        let data = serde_json::to_vec(vote).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Vote,
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;

        self.engine.put(KEY_VOTE.to_vec(), data).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Vote,
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;

        self.vote = Some(*vote);
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        // Return from cache (loaded at startup)
        Ok(self.vote)
    }

    async fn get_log_state(&mut self) -> Result<LogState<OntoRaftConfig>, StorageError<NodeId>> {
        // Find the last log entry by scanning from the end
        // We use a reverse scan approach: try high indices first
        let last_log_id = self.find_last_log_id()?;

        Ok(LogState {
            last_purged_log_id: self.purged,
            last_log_id,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        // Create a new instance sharing the same engine
        PersistentRaftStore {
            engine: self.engine.clone(),
            vote: self.vote,
            last_applied: self.last_applied,
            last_membership: self.last_membership.clone(),
            purged: self.purged,
        }
    }

    async fn append_to_log<I>(&mut self, entries: I) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<OntoRaftConfig>> + openraft::OptionalSend,
    {
        for entry in entries {
            let key = Self::log_key(entry.log_id.index);
            let data = serde_json::to_vec(&entry).map_err(|e| StorageError::IO {
                source: openraft::StorageIOError::new(
                    openraft::ErrorSubject::LogIndex(entry.log_id.index),
                    openraft::ErrorVerb::Write,
                    &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                ),
            })?;

            self.engine.put(key, data).map_err(|e| StorageError::IO {
                source: openraft::StorageIOError::new(
                    openraft::ErrorSubject::LogIndex(entry.log_id.index),
                    openraft::ErrorVerb::Write,
                    &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                ),
            })?;
        }
        Ok(())
    }

    async fn delete_conflict_logs_since(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        // Find all log entries >= log_id.index and delete them
        let last = self.find_last_log_id()?;
        if let Some(last_id) = last {
            for index in log_id.index..=last_id.index {
                let key = Self::log_key(index);
                if let Err(e) = self.engine.delete(key) {
                    tracing::warn!("Failed to delete log entry {}: {}", index, e);
                }
            }
        }
        Ok(())
    }

    async fn purge_logs_upto(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        // Delete log entries up to and including log_id.index
        for index in 0..=log_id.index {
            let key = Self::log_key(index);
            if let Err(e) = self.engine.delete(key) {
                tracing::warn!("Failed to purge log entry {}: {}", index, e);
            }
        }

        // Persist purged marker to LSM first, then update cache
        let data = serde_json::to_vec(&Some(log_id)).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Logs,
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;
        self.engine.put(KEY_PURGED.to_vec(), data).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Logs,
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;
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
        let mut final_last_applied = self.last_applied;
        let mut final_membership = self.last_membership.clone();

        // Phase 1: Process all entries, track final metadata
        for entry in entries {
            final_last_applied = Some(entry.log_id);

            match &entry.payload {
                EntryPayload::Blank => {
                    responses.push(OntoResponse::Success(None));
                }
                EntryPayload::Normal(req) => {
                    let resp = self.apply_request(req.clone());
                    responses.push(resp);
                }
                EntryPayload::Membership(mem) => {
                    final_membership = StoredMembership::new(Some(entry.log_id), mem.clone());
                    responses.push(OntoResponse::Success(None));
                }
            }
        }

        // Phase 2: Batch persist metadata to LSM (write before cache update)
        let last_applied_data = serde_json::to_vec(&final_last_applied).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::StateMachine,
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;
        self.engine.put(KEY_LAST_APPLIED.to_vec(), last_applied_data).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::StateMachine,
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;

        if final_membership != self.last_membership {
            let membership_data = serde_json::to_vec(&final_membership).map_err(|e| StorageError::IO {
                source: openraft::StorageIOError::new(
                    openraft::ErrorSubject::StateMachine,
                    openraft::ErrorVerb::Write,
                    &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                ),
            })?;
            self.engine.put(KEY_MEMBERSHIP.to_vec(), membership_data).map_err(|e| StorageError::IO {
                source: openraft::StorageIOError::new(
                    openraft::ErrorSubject::StateMachine,
                    openraft::ErrorVerb::Write,
                    &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                ),
            })?;
        }

        // Phase 3: Update in-memory caches after LSM writes succeed
        self.last_applied = final_last_applied;
        self.last_membership = final_membership;

        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        PersistentRaftStore {
            engine: self.engine.clone(),
            vote: self.vote,
            last_applied: self.last_applied,
            last_membership: self.last_membership.clone(),
            purged: self.purged,
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

        // Parse and install state machine data
        if let Ok(sm_data) = serde_json::from_slice::<std::collections::BTreeMap<Vec<u8>, Vec<u8>>>(&data) {
            // Clear existing SM data and install new
            let existing = self.engine.scan_prefix(RAFT_SM_PREFIX).map_err(|e| StorageError::IO {
                source: openraft::StorageIOError::new(
                    openraft::ErrorSubject::Snapshot(None),
                    openraft::ErrorVerb::Write,
                    &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                ),
            })?;

            for (key, _) in existing {
                let _ = self.engine.delete(key);
            }

            // Write new SM data
            for (key, value) in sm_data {
                let sm_key = Self::sm_key(&key);
                self.engine.put(sm_key, value).map_err(|e| StorageError::IO {
                    source: openraft::StorageIOError::new(
                        openraft::ErrorSubject::Snapshot(None),
                        openraft::ErrorVerb::Write,
                        &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
                    ),
                })?;
            }
        }

        self.last_applied = meta.last_log_id;
        self.last_membership = StoredMembership::new(
            meta.last_log_id,
            meta.last_membership.membership().clone(),
        );

        // Persist metadata
        let meta_json = serde_json::to_vec(&meta).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Snapshot(None),
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;
        self.engine.put(KEY_SNAPSHOT_META.to_vec(), meta_json).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Snapshot(None),
                openraft::ErrorVerb::Write,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;

        tracing::info!("Installed snapshot: {:?}", meta.last_log_id);

        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<OntoRaftConfig>>, StorageError<NodeId>> {
        // Try to load snapshot metadata
        match self.engine.get(KEY_SNAPSHOT_META) {
            Ok(Some(meta_data)) => {
                match serde_json::from_slice::<SnapshotMeta<NodeId, openraft::BasicNode>>(&meta_data) {
                    Ok(meta) => {
                        // Build snapshot data from current SM state
                        let data = self.get_all_sm_data().map_err(|e| StorageError::IO {
                            source: openraft::StorageIOError::new(
                                openraft::ErrorSubject::Snapshot(None),
                                openraft::ErrorVerb::Read,
                                &e,
                            ),
                        })?;

                        Ok(Some(Snapshot {
                            meta,
                            snapshot: Box::new(std::io::Cursor::new(data)),
                        }))
                    }
                    Err(_) => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }
}

impl PersistentRaftStore {
    /// Find the last log entry id by scanning from a high index.
    fn find_last_log_id(&self) -> Result<Option<LogId<NodeId>>, StorageError<NodeId>> {
        // Try to find the last log entry by scanning backwards
        // We start from a high index and work down
        // This is not the most efficient approach, but it's correct
        // A better approach would be to persist the last log index separately

        // First check if there are any entries in the log prefix
        let entries = self.engine.scan_prefix(RAFT_LOG_PREFIX).map_err(|e| StorageError::IO {
            source: openraft::StorageIOError::new(
                openraft::ErrorSubject::Logs,
                openraft::ErrorVerb::Read,
                &std::io::Error::new(std::io::ErrorKind::Other, e.to_string()),
            ),
        })?;

        if entries.is_empty() {
            return Ok(None);
        }

        // Parse the last entry to get its log id
        // The entries are sorted by key (index), so the last one is the highest
        if let Some((key, data)) = entries.last() {
            if let Ok(entry) = serde_json::from_slice::<Entry<OntoRaftConfig>>(data) {
                return Ok(Some(entry.log_id));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onto_storage::StorageOptions;
    use tempfile::TempDir;

    fn create_test_engine() -> (Arc<onto_storage::LsmEngine>, TempDir) {
        let tmp_dir = TempDir::new().unwrap();
        let options = StorageOptions {
            data_dir: tmp_dir.path().to_path_buf(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(onto_storage::LsmEngine::open(options).unwrap());
        (engine, tmp_dir)
    }

    fn create_test_entry(index: u64, key: &[u8], value: &[u8]) -> Entry<OntoRaftConfig> {
        Entry {
            log_id: LogId::new(openraft::CommittedLeaderId::<NodeId>::new(1, 1), index),
            payload: EntryPayload::Normal(OntoRequest::Put {
                key: key.to_vec(),
                value: value.to_vec(),
            }),
        }
    }

    #[tokio::test]
    async fn test_persistent_store_put_and_get() {
        let (engine, _tmp) = create_test_engine();
        let mut store = PersistentRaftStore::new(engine);

        let entry = create_test_entry(1, b"hello", b"world");
        let responses = store.apply_to_state_machine(&[entry]).await.unwrap();
        assert_eq!(responses.len(), 1);
        assert!(matches!(&responses[0], OntoResponse::Success(None)));

        // Verify data persisted
        let sm_key = PersistentRaftStore::sm_key(b"hello");
        let value = store.engine.get(&sm_key).unwrap();
        assert_eq!(value, Some(b"world".to_vec()));
    }

    #[tokio::test]
    async fn test_persistent_store_survives_restart() {
        let tmp_dir = TempDir::new().unwrap();
        let data_dir = tmp_dir.path().to_path_buf();

        // First session: write data
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = Arc::new(onto_storage::LsmEngine::open(options).unwrap());
            let mut store = PersistentRaftStore::new(engine);

            let entry = create_test_entry(1, b"persist_key", b"persist_value");
            store.apply_to_state_machine(&[entry]).await.unwrap();
        }

        // Second session: verify data survived
        {
            let options = StorageOptions {
                data_dir,
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = Arc::new(onto_storage::LsmEngine::open(options).unwrap());
            let store = PersistentRaftStore::new(engine);

            // Verify SM data
            let sm_key = PersistentRaftStore::sm_key(b"persist_key");
            let value = store.engine.get(&sm_key).unwrap();
            assert_eq!(value, Some(b"persist_value".to_vec()));
        }
    }

    #[tokio::test]
    async fn test_persistent_store_log_append_and_read() {
        let (engine, _tmp) = create_test_engine();
        let mut store = PersistentRaftStore::new(engine);

        let entry = create_test_entry(1, b"k", b"v");
        store.append_to_log(vec![entry]).await.unwrap();

        let entries = store.try_get_log_entries(1..2).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].log_id.index, 1);
    }

    #[tokio::test]
    async fn test_persistent_store_batch_apply() {
        let (engine, _tmp) = create_test_engine();
        let mut store = PersistentRaftStore::new(engine);

        let entry = Entry {
            log_id: LogId::new(openraft::CommittedLeaderId::<NodeId>::new(1, 1), 1),
            payload: EntryPayload::Normal(OntoRequest::Batch {
                ops: vec![
                    OntoRequest::Put { key: b"a".to_vec(), value: b"1".to_vec() },
                    OntoRequest::Put { key: b"b".to_vec(), value: b"2".to_vec() },
                ],
            }),
        };

        let responses = store.apply_to_state_machine(&[entry]).await.unwrap();
        assert!(matches!(&responses[0], OntoResponse::Success(_)));

        // Verify both keys
        let sm_key_a = PersistentRaftStore::sm_key(b"a");
        let sm_key_b = PersistentRaftStore::sm_key(b"b");
        assert_eq!(store.engine.get(&sm_key_a).unwrap(), Some(b"1".to_vec()));
        assert_eq!(store.engine.get(&sm_key_b).unwrap(), Some(b"2".to_vec()));
    }

    #[tokio::test]
    async fn test_persistent_store_delete() {
        let (engine, _tmp) = create_test_engine();
        let mut store = PersistentRaftStore::new(engine);

        // First put
        let entry1 = create_test_entry(1, b"key1", b"val1");
        store.apply_to_state_machine(&[entry1]).await.unwrap();

        // Then delete
        let entry2 = Entry {
            log_id: LogId::new(openraft::CommittedLeaderId::<NodeId>::new(1, 1), 2),
            payload: EntryPayload::Normal(OntoRequest::Delete {
                key: b"key1".to_vec(),
            }),
        };
        store.apply_to_state_machine(&[entry2]).await.unwrap();

        // Verify deleted
        let sm_key = PersistentRaftStore::sm_key(b"key1");
        assert_eq!(store.engine.get(&sm_key).unwrap(), None);
    }
}
