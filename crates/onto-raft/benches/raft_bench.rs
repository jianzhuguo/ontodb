//! Raft Storage Micro-Benchmarks
//!
//! Tests:
//! 1. Log append throughput (sequential writes)
//! 2. Log read throughput (sequential reads)
//! 3. State machine apply throughput
//! 4. Snapshot build/restore performance
//! 5. Persistent store restart recovery
//! 6. Chain hash computation overhead
//!
//! Run: cargo bench --bench raft_bench

use onto_raft::PersistentRaftStore;
use onto_storage::{LsmEngine, StorageOptions};
use openraft::storage::{RaftLogReader, RaftSnapshotBuilder, RaftStorage};
use openraft::{CommittedLeaderId, Entry, EntryPayload, LogId};
use std::sync::Arc;
use std::time::{Duration, Instant};

// Type aliases for convenience
type NodeId = u64;
type RaftConfig = onto_raft::types::OntoRaftConfig;

/// Create a test engine with temporary directory.
fn setup_engine() -> (Arc<LsmEngine>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 64 * 1024 * 1024, // 64MB
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());
    (engine, dir)
}

/// Create a PUT entry for testing.
fn create_put_entry(index: u64, key_suffix: &str) -> Entry<RaftConfig> {
    Entry {
        log_id: LogId::new(CommittedLeaderId::<NodeId>::new(1, 1), index),
        payload: EntryPayload::Normal(onto_raft::types::OntoRequest::Put {
            key: format!("bench_key_{}", key_suffix).into_bytes(),
            value: format!("bench_value_{}", key_suffix).into_bytes(),
        }),
    }
}

/// Create a batch entry for testing.
fn create_batch_entry(index: u64, batch_size: usize) -> Entry<RaftConfig> {
    let ops: Vec<_> = (0..batch_size)
        .map(|i| onto_raft::types::OntoRequest::Put {
            key: format!("batch_key_{}_{}", index, i).into_bytes(),
            value: format!("batch_value_{}_{}", index, i).into_bytes(),
        })
        .collect();

    Entry {
        log_id: LogId::new(CommittedLeaderId::<NodeId>::new(1, 1), index),
        payload: EntryPayload::Normal(onto_raft::types::OntoRequest::Batch { ops }),
    }
}

// ─── Benchmark 1: Log Append Throughput ──────────────────────────────────────

fn bench_log_append(count: usize) -> (Duration, f64) {
    let (engine, _dir) = setup_engine();
    let mut store = PersistentRaftStore::new(engine);

    let entries: Vec<_> = (1..=count as u64)
        .map(|i| create_put_entry(i, &i.to_string()))
        .collect();

    let start = Instant::now();
    for chunk in entries.chunks(1000) {
        futures::executor::block_on(store.append_to_log(chunk.to_vec())).unwrap();
    }
    let elapsed = start.elapsed();

    let ops_per_sec = count as f64 / elapsed.as_secs_f64();
    (elapsed, ops_per_sec)
}

// ─── Benchmark 2: Log Read Throughput ────────────────────────────────────────

fn bench_log_read(count: usize) -> (Duration, f64) {
    let (engine, _dir) = setup_engine();
    let mut store = PersistentRaftStore::new(engine);

    // Prepare: write entries
    let entries: Vec<_> = (1..=count as u64)
        .map(|i| create_put_entry(i, &i.to_string()))
        .collect();
    for chunk in entries.chunks(1000) {
        futures::executor::block_on(store.append_to_log(chunk.to_vec())).unwrap();
    }

    // Benchmark: read entries in batches
    let start = Instant::now();
    let batch_size = 100;
    for start_idx in (1..=count as u64).step_by(batch_size) {
        let end_idx = (start_idx + batch_size as u64).min(count as u64 + 1);
        let _ = futures::executor::block_on(store.try_get_log_entries(start_idx..end_idx)).unwrap();
    }
    let elapsed = start.elapsed();

    let ops_per_sec = count as f64 / elapsed.as_secs_f64();
    (elapsed, ops_per_sec)
}

// ─── Benchmark 3: State Machine Apply Throughput ─────────────────────────────

fn bench_state_machine_apply(count: usize) -> (Duration, f64) {
    let (engine, _dir) = setup_engine();
    let mut store = PersistentRaftStore::new(engine);

    let entries: Vec<_> = (1..=count as u64)
        .map(|i| create_put_entry(i, &i.to_string()))
        .collect();

    let start = Instant::now();
    for chunk in entries.chunks(100) {
        let _ = futures::executor::block_on(store.apply_to_state_machine(chunk)).unwrap();
    }
    let elapsed = start.elapsed();

    let ops_per_sec = count as f64 / elapsed.as_secs_f64();
    (elapsed, ops_per_sec)
}

// ─── Benchmark 4: Batch Apply Performance ────────────────────────────────────

fn bench_batch_apply(batch_size: usize, num_batches: usize) -> (Duration, f64) {
    let (engine, _dir) = setup_engine();
    let mut store = PersistentRaftStore::new(engine);

    let entries: Vec<_> = (1..=num_batches as u64)
        .map(|i| create_batch_entry(i, batch_size))
        .collect();

    let total_ops = batch_size * num_batches;
    let start = Instant::now();
    for entry in &entries {
        let _ =
            futures::executor::block_on(store.apply_to_state_machine(&[entry.clone()])).unwrap();
    }
    let elapsed = start.elapsed();

    let ops_per_sec = total_ops as f64 / elapsed.as_secs_f64();
    (elapsed, ops_per_sec)
}

// ─── Benchmark 5: Snapshot Build Performance ─────────────────────────────────

fn bench_snapshot_build(entry_count: usize) -> (Duration, usize) {
    let (engine, _dir) = setup_engine();
    let mut store = PersistentRaftStore::new(engine);

    // Populate state machine
    let entries: Vec<_> = (1..=entry_count as u64)
        .map(|i| create_put_entry(i, &i.to_string()))
        .collect();
    for chunk in entries.chunks(100) {
        let _ = futures::executor::block_on(store.apply_to_state_machine(chunk)).unwrap();
    }

    // Benchmark snapshot build (use get_current_snapshot which handles serialization)
    let start = Instant::now();
    let snapshot = futures::executor::block_on(store.get_current_snapshot()).unwrap();
    let elapsed = start.elapsed();

    let snapshot_size = snapshot.map(|s| s.snapshot.get_ref().len()).unwrap_or(0);
    (elapsed, snapshot_size)
}

// ─── Benchmark 6: Restart Recovery ───────────────────────────────────────────

fn bench_restart_recovery(entry_count: usize) -> (Duration, Duration) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Phase 1: Write data
    let write_time = {
        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 64 * 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let mut store = PersistentRaftStore::new(engine);

        let entries: Vec<_> = (1..=entry_count as u64)
            .map(|i| create_put_entry(i, &i.to_string()))
            .collect();

        let start = Instant::now();
        for chunk in entries.chunks(100) {
            let _ = futures::executor::block_on(store.apply_to_state_machine(chunk)).unwrap();
        }
        start.elapsed()
    };

    // Phase 2: Restart and recover
    let recover_time = {
        let options = StorageOptions {
            data_dir,
            memtable_size_limit: 64 * 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());

        let start = Instant::now();
        let _store = PersistentRaftStore::new(engine);
        start.elapsed()
    };

    (write_time, recover_time)
}

// ─── Benchmark 7: Concurrent Log Writes ──────────────────────────────────────

fn bench_concurrent_writes(count: usize, num_threads: usize) -> (Duration, f64) {
    let (engine, _dir) = setup_engine();
    let store = Arc::new(parking_lot::Mutex::new(PersistentRaftStore::new(engine)));

    let entries_per_thread = count / num_threads;
    let start = Instant::now();

    let handles: Vec<_> = (0..num_threads)
        .map(|t| {
            let store = store.clone();
            std::thread::spawn(move || {
                let entries: Vec<_> = (0..entries_per_thread as u64)
                    .map(|i| {
                        let idx = t as u64 * entries_per_thread as u64 + i;
                        create_put_entry(idx, &format!("{}_{}", t, i))
                    })
                    .collect();

                let mut store = store.lock();
                for chunk in entries.chunks(100) {
                    let _ =
                        futures::executor::block_on(store.append_to_log(chunk.to_vec())).unwrap();
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }
    let elapsed = start.elapsed();

    let ops_per_sec = count as f64 / elapsed.as_secs_f64();
    (elapsed, ops_per_sec)
}

// ─── Main: Run all benchmarks ────────────────────────────────────────────────

fn main() {
    println!("═══════════════════════════════════════════════════════════════");
    println!("           OntoDB Raft Storage Micro-Benchmarks");
    println!("═══════════════════════════════════════════════════════════════");
    println!();

    // Benchmark 1: Log Append
    println!("── Benchmark 1: Log Append Throughput ──────────────────────");
    for &count in &[1000, 10000, 50000] {
        let (elapsed, ops) = bench_log_append(count);
        println!(
            "  {:>6} entries: {:>8.2?} ({:>10.0} ops/sec)",
            count, elapsed, ops
        );
    }
    println!();

    // Benchmark 2: Log Read
    println!("── Benchmark 2: Log Read Throughput ────────────────────────");
    for &count in &[1000, 10000, 50000] {
        let (elapsed, ops) = bench_log_read(count);
        println!(
            "  {:>6} entries: {:>8.2?} ({:>10.0} ops/sec)",
            count, elapsed, ops
        );
    }
    println!();

    // Benchmark 3: State Machine Apply
    println!("── Benchmark 3: State Machine Apply Throughput ─────────────");
    for &count in &[1000, 10000, 50000] {
        let (elapsed, ops) = bench_state_machine_apply(count);
        println!(
            "  {:>6} entries: {:>8.2?} ({:>10.0} ops/sec)",
            count, elapsed, ops
        );
    }
    println!();

    // Benchmark 4: Batch Apply
    println!("── Benchmark 4: Batch Apply Performance ────────────────────");
    for &batch_size in &[10, 50, 100] {
        let num_batches = 1000;
        let (elapsed, ops) = bench_batch_apply(batch_size, num_batches);
        println!(
            "  batch_size={:>3}, {} batches: {:>8.2?} ({:>10.0} ops/sec)",
            batch_size, num_batches, elapsed, ops
        );
    }
    println!();

    // Benchmark 5: Snapshot Build
    println!("── Benchmark 5: Snapshot Build Performance ─────────────────");
    for &count in &[1000, 10000, 50000] {
        let (elapsed, size) = bench_snapshot_build(count);
        println!(
            "  {:>6} entries: {:>8.2?} (snapshot size: {} KB)",
            count,
            elapsed,
            size / 1024
        );
    }
    println!();

    // Benchmark 6: Restart Recovery
    println!("── Benchmark 6: Restart Recovery ───────────────────────────");
    for &count in &[1000, 10000, 50000] {
        let (write_time, recover_time) = bench_restart_recovery(count);
        println!(
            "  {:>6} entries: write={:>8.2?}, recover={:>8.2?}",
            count, write_time, recover_time
        );
    }
    println!();

    // Benchmark 7: Concurrent Writes
    println!("── Benchmark 7: Concurrent Log Writes ──────────────────────");
    for &threads in &[1, 2, 4] {
        let count = 10000;
        let (elapsed, ops) = bench_concurrent_writes(count, threads);
        println!(
            "  {} threads, {:>6} entries: {:>8.2?} ({:>10.0} ops/sec)",
            threads, count, elapsed, ops
        );
    }
    println!();

    println!("═══════════════════════════════════════════════════════════════");
    println!("  Benchmark complete.");
    println!("═══════════════════════════════════════════════════════════════");
}
