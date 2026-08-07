//! Benchmark: read lock vs write lock contention on LsmEngine.
//!
//! Tests high-contention scenarios where the lock is held for realistic workloads.

use onto_storage::{LsmEngine, StorageOptions};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

fn setup_engine(row_count: usize) -> (Arc<RwLock<LsmEngine>>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 4 * 1024 * 1024,
        ..Default::default()
    };
    let engine = LsmEngine::open(options).unwrap();
    let engine = Arc::new(RwLock::new(engine));

    {
        let eng = engine.write().unwrap();
        for i in 0..row_count {
            let key = format!("Product::{:020}", i).into_bytes();
            let val = format!(
                r#"{{"__class__":"Product","name":"item_{}","price":{}}}"#,
                i,
                (i * 10) % 10000
            )
            .into_bytes();
            eng.put(key, val).unwrap();
        }
        // Don't flush — keep data in memtable for fast reads
    }

    (engine, dir)
}

/// Sequential scan_prefix baseline (single-threaded).
fn bench_sequential_scan(engine: &RwLock<LsmEngine>, iterations: usize) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        let eng = engine.read().unwrap();
        let _ = eng.scan_prefix(b"Product::").unwrap();
    }
    start.elapsed()
}

/// Concurrent scan_prefix via read lock (allows parallelism).
fn bench_concurrent_read_lock(
    engine: Arc<RwLock<LsmEngine>>,
    iterations: usize,
    num_threads: usize,
) -> Duration {
    let per_thread = iterations / num_threads;
    let start = Instant::now();
    let mut handles = Vec::new();

    for t in 0..num_threads {
        let count = per_thread + if t < iterations % num_threads { 1 } else { 0 };
        let eng = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for _ in 0..count {
                let guard = eng.read().unwrap();
                let _ = guard.scan_prefix(b"Product::").unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    start.elapsed()
}

/// Concurrent scan_prefix via write lock (serialized — the old behavior).
fn bench_concurrent_write_lock(
    engine: Arc<RwLock<LsmEngine>>,
    iterations: usize,
    num_threads: usize,
) -> Duration {
    let per_thread = iterations / num_threads;
    let start = Instant::now();
    let mut handles = Vec::new();

    for t in 0..num_threads {
        let count = per_thread + if t < iterations % num_threads { 1 } else { 0 };
        let eng = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for _ in 0..count {
                let guard = eng.write().unwrap();
                let _ = guard.scan_prefix(b"Product::").unwrap();
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    start.elapsed()
}

/// Mixed: concurrent readers + a writer doing flush (heavy I/O).
fn bench_mixed_read_write(
    engine: Arc<RwLock<LsmEngine>>,
    read_iters: usize,
    num_readers: usize,
) -> Duration {
    let reads_per = read_iters / num_readers;
    let start = Instant::now();
    let mut handles = Vec::new();

    // Readers — scan_prefix
    for _ in 0..num_readers {
        let eng = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for _ in 0..reads_per {
                let guard = eng.read().unwrap();
                let _ = guard.scan_prefix(b"Product::").unwrap();
            }
        }));
    }

    // Writer — insert + flush (heavy I/O under lock)
    {
        let eng = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for i in 0..50 {
                let mut guard = eng.write().unwrap();
                let key = format!("New::{:020}", i).into_bytes();
                let val = b"{}".to_vec();
                let _ = guard.put(key, val);
                let _ = guard.flush();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    start.elapsed()
}

fn main() {
    let row_count = 10_000;
    let iterations = 200;

    println!("Setting up engine with {} rows...", row_count);
    let (engine, _dir) = setup_engine(row_count);
    println!("Setup complete.\n");

    println!("{}", "=".repeat(72));
    println!("  OntoDB Lock Contention Benchmark (scan_prefix workload)");
    println!("  Data: {} rows, {} iterations/test", row_count, iterations);
    println!("{}", "=".repeat(72));
    println!();

    // Sequential baseline
    let seq = bench_sequential_scan(&engine, iterations);
    println!("Sequential scan (1 thread):  {:>8?}", seq);
    println!();

    // Concurrent reads — the key test
    println!("--- Concurrent scan_prefix (read-lock vs write-lock) ---");
    for &nt in &[2, 4, 8] {
        let rl = bench_concurrent_read_lock(Arc::clone(&engine), iterations, nt);
        let wl = bench_concurrent_write_lock(Arc::clone(&engine), iterations, nt);
        let speedup = if rl.as_micros() > 0 {
            wl.as_secs_f64() / rl.as_secs_f64()
        } else {
            f64::INFINITY
        };
        println!(
            "  {} threads:  read-lock={:>8?}  write-lock={:>8?}  speedup={:.2}x",
            nt, rl, wl, speedup
        );
    }
    println!();

    // Mixed read + write (the real win: writes don't block reads)
    println!("--- Mixed: 8 readers + 1 writer (flush under write lock) ---");
    let mixed = bench_mixed_read_write(Arc::clone(&engine), iterations * 4, 8);
    println!("  Total: {:?}", mixed);

    println!();
    println!("{}", "=".repeat(72));
    println!("  Benchmark complete.");
    println!("{}", "=".repeat(72));
}
