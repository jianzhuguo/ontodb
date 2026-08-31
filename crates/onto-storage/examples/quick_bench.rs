//! Quick storage engine benchmark - 8 readers + 1 writer

use onto_storage::{LsmEngine, StorageOptions};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    println!("OntoDB Storage Engine - Quick Benchmark (8R/1W)");
    println!("================================================\n");

    // Setup
    let row_count = 50_000;
    let iterations = 100;
    
    println!("Setting up engine with {} rows...", row_count);
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 64 * 1024 * 1024, // 64MB - same as official benchmark
        sync_wal_on_commit: false,
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());

    for i in 0..row_count {
        let key = format!("K::{:012}", i).into_bytes();
        let val = format!(r#"{{"v":{}}}"#, i).into_bytes();
        engine.put(key, val).unwrap();
    }
    println!("Setup complete.\n");

    // Test 1: Sequential scan baseline
    println!("─── 1. Sequential Scan Baseline ───");
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = engine.scan_prefix(b"K::").unwrap();
    }
    let seq_time = start.elapsed();
    println!("  {} scans: {:?} ({:.0} scans/sec)", 
        iterations, seq_time, iterations as f64 / seq_time.as_secs_f64());
    println!();

    // Test 2: Mixed 8 readers + 1 writer
    println!("─── 2. Mixed: 8 Readers + 1 Writer ───");
    let read_iters = iterations * 4;
    let reads_per = read_iters / 8;
    
    let start = Instant::now();
    let mut handles = Vec::new();

    // 8 reader threads
    for _ in 0..8 {
        let eng = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for _ in 0..reads_per {
                let _ = eng.scan_prefix(b"K::").unwrap();
            }
        }));
    }

    // 1 writer thread with flush
    {
        let eng = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for i in 0..50 {
                let key = format!("New::{:020}", i).into_bytes();
                let val = b"{}".to_vec();
                let _ = eng.put(key, val);
                let _ = eng.flush();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    let mixed_time = start.elapsed();
    println!("  {} reads + 50 writes: {:?}", read_iters, mixed_time);
    println!("  Read throughput: {:.0} reads/sec", read_iters as f64 / mixed_time.as_secs_f64());
    println!();

    // Test 3: Write throughput
    println!("─── 3. Write Throughput ───");
    let write_count = 5_000;
    let start = Instant::now();
    for i in 0..write_count {
        let key = format!("Write::{:020}", i).into_bytes();
        let val = format!(r#"{{"id":{}}}"#, i).into_bytes();
        let _ = engine.put(key, val);
    }
    let write_time = start.elapsed();
    println!("  {} writes: {:?} ({:.0} writes/sec)", 
        write_count, write_time, write_count as f64 / write_time.as_secs_f64());
    println!();

    // Test 4: Transaction throughput
    println!("─── 4. Transaction Throughput ───");
    let txn_count = 1_000;
    let start = Instant::now();
    for i in 0..txn_count {
        let txn_id = engine.begin_txn();
        let key = format!("Txn::{:020}", i).into_bytes();
        let val = format!(r#"{{"txn":{}}}"#, i).into_bytes();
        let _ = engine.txn_put(txn_id, key, val);
        let _ = engine.commit_txn(txn_id);
    }
    let txn_time = start.elapsed();
    println!("  {} txn commits: {:?} ({:.0} txn/sec)", 
        txn_count, txn_time, txn_count as f64 / txn_time.as_secs_f64());
    println!();

    // Test 5: Concurrent read scalability
    println!("─── 5. Concurrent Read Scalability ───");
    for &nt in &[1, 2, 4, 8] {
        let per_thread = iterations / nt;
        let start = Instant::now();
        let mut handles = Vec::new();
        
        for _ in 0..nt {
            let eng = Arc::clone(&engine);
            handles.push(std::thread::spawn(move || {
                for _ in 0..per_thread {
                    let _ = eng.scan_prefix(b"K::").unwrap();
                }
            }));
        }
        
        for h in handles {
            h.join().unwrap();
        }
        let time = start.elapsed();
        let total = per_thread * nt;
        println!("  {} threads: {:?} ({:.0} reads/sec, speedup {:.2}x)", 
            nt, time, total as f64 / time.as_secs_f64(),
            seq_time.as_secs_f64() / time.as_secs_f64() * nt as f64);
    }
    println!();

    // Test 6: Point Lookup (comparable to previous benchmark)
    println!("─── 6. Point Lookup (get by key) ───");
    
    // Check if data is in MemTable
    let first_key = format!("K::{:012}", 0).into_bytes();
    let in_memtable = engine.get(&first_key).is_ok();
    println!("  Data location: {}", if in_memtable { "MemTable (in-memory)" } else { "SSTable (on-disk)" });
    
    let lookup_count = 50_000; // Match official benchmark
    let start = Instant::now();
    for i in 0..lookup_count {
        let key = format!("K::{:012}", i % row_count).into_bytes();
        let _ = engine.get(&key);
    }
    let lookup_time = start.elapsed();
    println!("  {} lookups: {:?} ({:.0} lookups/sec)", 
        lookup_count, lookup_time, lookup_count as f64 / lookup_time.as_secs_f64());
    println!();

    // Summary
    println!("================================================");
    println!("Benchmark Complete");
    println!("================================================");
}
