//! High-concurrency group commit benchmark.
//!
//! Tests WAL group commit performance under heavy write contention
//! with multiple concurrent writer threads.

use std::sync::Arc;
use std::time::Instant;
use onto_storage::{LsmEngine, StorageOptions};

fn run_bench(label: &str, engine: Arc<LsmEngine>, num_threads: usize, writes_per_thread: usize) {
    let total = num_threads * writes_per_thread;
    let start = Instant::now();
    let mut handles = vec![];

    for t in 0..num_threads {
        let engine = engine.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..writes_per_thread {
                let key = format!("t{:02}::{:010}", t, i);
                engine.put(key.into_bytes(), vec![b'x'; 100]).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed();
    let qps = total as f64 / elapsed.as_secs_f64();
    println!("  {:>30}: {:>8} writes in {:.1}ms  ({:.0} writes/sec)", 
        label, total, elapsed.as_secs_f64() * 1000.0, qps);
}

fn main() {
    println!("========================================================================");
    println!("  OntoDB Group Commit High-Concurrency Benchmark");
    println!("========================================================================");
    println!();

    // Test 1: With sync (group commit active)
    println!("─── sync_wal_on_commit=true (Group Commit) ───");
    {
        let dir = tempfile::tempdir().unwrap();
        let opts = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 64 * 1024 * 1024,
            sync_wal_on_commit: true,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(opts).unwrap());

        run_bench("1 thread × 10000", engine.clone(), 1, 10_000);
        run_bench("2 threads × 10000", engine.clone(), 2, 10_000);
        run_bench("4 threads × 10000", engine.clone(), 4, 10_000);
        run_bench("8 threads × 10000", engine.clone(), 8, 10_000);
        run_bench("16 threads × 10000", engine.clone(), 16, 10_000);
    }
    println!();

    // Test 2: Without sync (baseline)
    println!("─── sync_wal_on_commit=false (No Sync) ───");
    {
        let dir = tempfile::tempdir().unwrap();
        let opts = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 64 * 1024 * 1024,
            sync_wal_on_commit: false,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(opts).unwrap());

        run_bench("1 thread × 50000", engine.clone(), 1, 50_000);
        run_bench("2 threads × 50000", engine.clone(), 2, 50_000);
        run_bench("4 threads × 50000", engine.clone(), 4, 50_000);
        run_bench("8 threads × 50000", engine.clone(), 8, 50_000);
    }
    println!();

    println!("========================================================================");
    println!("  Benchmark complete.");
    println!("========================================================================");
}
