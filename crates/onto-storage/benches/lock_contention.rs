//! Benchmark: OntoDB storage engine performance.
//!
//! Tests:
//! 1. Concurrent scan_prefix throughput (internal Mutex contention)
//! 2. WAL write throughput (put operations)
//! 3. Mixed read/write throughput
//! 4. HNSW batch construction vs individual inserts

use onto_storage::vector::DistanceMetric;
use onto_storage::{LsmEngine, StorageOptions};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn setup_engine(row_count: usize) -> (Arc<LsmEngine>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 4 * 1024 * 1024,
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());

    for i in 0..row_count {
        let key = format!("Product::{:020}", i).into_bytes();
        let val = format!(
            r#"{{"__class__":"Product","name":"item_{}","price":{}}}"#,
            i,
            (i * 10) % 10000
        )
        .into_bytes();
        engine.put(key, val).unwrap();
    }

    (engine, dir)
}

/// Sequential scan_prefix baseline (single-threaded).
fn bench_sequential_scan(engine: &LsmEngine, iterations: usize) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = engine.scan_prefix(b"Product::").unwrap();
    }
    start.elapsed()
}

/// Concurrent scan_prefix — tests internal Mutex contention.
fn bench_concurrent_scan(
    engine: Arc<LsmEngine>,
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
                let _ = eng.scan_prefix(b"Product::").unwrap();
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
    engine: Arc<LsmEngine>,
    read_iters: usize,
    num_readers: usize,
) -> Duration {
    let reads_per = read_iters / num_readers;
    let start = Instant::now();
    let mut handles = Vec::new();

    for _ in 0..num_readers {
        let eng = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || {
            for _ in 0..reads_per {
                let _ = eng.scan_prefix(b"Product::").unwrap();
            }
        }));
    }

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
    start.elapsed()
}

/// Benchmark: sequential put throughput (WAL batch sync optimization).
fn bench_write_throughput(num_writes: usize) -> Duration {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 64 * 1024 * 1024, // 64MB — avoid mid-bench flushes
        sync_wal_on_commit: false,
        ..Default::default()
    };
    let engine = LsmEngine::open(options).unwrap();

    let start = Instant::now();
    for i in 0..num_writes {
        let key = format!("K::{:012}", i).into_bytes();
        let val = format!(r#"{{"v":{}}}"#, i).into_bytes();
        engine.put(key, val).unwrap();
    }
    start.elapsed()
}

/// Benchmark: sequential get throughput.
fn bench_read_throughput(engine: &LsmEngine, num_reads: usize) -> Duration {
    let start = Instant::now();
    for i in 0..num_reads {
        let key = format!("K::{:012}", i).into_bytes();
        let _ = engine.get(&key).unwrap();
    }
    start.elapsed()
}

/// Benchmark: HNSW individual vector inserts.
fn bench_hnsw_individual(count: usize, dim: usize) -> Duration {
    use onto_storage::vector::{HnswConfig, HnswIndex, VectorEntry};
    use rand::Rng;

    let config = HnswConfig::new(dim, DistanceMetric::L2)
        .with_m(16)
        .with_ef_construction(200)
        .with_ef_search(100);
    let mut index = HnswIndex::new(config);
    let mut rng = rand::thread_rng();

    let vectors: Vec<Vec<f32>> = (0..count)
        .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
        .collect();

    let start = Instant::now();
    for (i, vec) in vectors.into_iter().enumerate() {
        let entry = VectorEntry {
            id: format!("vec_{}", i).into_bytes(),
            vector: vec,
        };
        index.insert(entry);
    }
    start.elapsed()
}

/// Benchmark: HNSW batch vector inserts.
fn bench_hnsw_batch(count: usize, dim: usize) -> Duration {
    use onto_storage::vector::{HnswConfig, HnswIndex, VectorEntry};
    use rand::Rng;

    let config = HnswConfig::new(dim, DistanceMetric::L2)
        .with_m(16)
        .with_ef_construction(200)
        .with_ef_search(100);
    let mut index = HnswIndex::new(config);
    let mut rng = rand::thread_rng();

    let entries: Vec<VectorEntry> = (0..count)
        .map(|i| VectorEntry {
            id: format!("vec_{}", i).into_bytes(),
            vector: (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect(),
        })
        .collect();

    let start = Instant::now();
    index.insert_batch(entries);
    start.elapsed()
}

fn main() {
    let row_count = 100_000;
    let iterations = 200;

    println!("Setting up engine with {} rows...", row_count);
    let (engine, _dir) = setup_engine(row_count);
    println!("Setup complete.\n");

    println!("{}", "=".repeat(72));
    println!("  OntoDB Storage Engine Benchmark");
    println!("  Data: {} rows, {} iterations/test", row_count, iterations);
    println!("{}", "=".repeat(72));
    println!();

    // ── Section 1: Concurrent Scan Throughput ──
    println!("─── 1. Concurrent Scan Throughput ───");
    let seq = bench_sequential_scan(&engine, iterations);
    println!("  Sequential scan (1 thread):  {:>8?}", seq);
    println!();

    println!("  Concurrent scan_prefix (multi-threaded):");
    for &nt in &[2, 4, 8] {
        let ct = bench_concurrent_scan(Arc::clone(&engine), iterations, nt);
        let speedup = seq.as_secs_f64() / ct.as_secs_f64();
        println!(
            "    {} threads:  {:>8?}  speedup={:.2}x",
            nt, ct, speedup
        );
    }
    println!();

    println!("  Mixed: 8 readers + 1 writer (flush under write lock):");
    let mixed = bench_mixed_read_write(Arc::clone(&engine), iterations * 4, 8);
    println!("    Total: {:?}", mixed);
    println!();

    // ── Section 2: Write Throughput (WAL optimization) ──
    println!("─── 2. Write Throughput (WAL batch sync) ───");
    let write_count = 50_000;
    let wt = bench_write_throughput(write_count);
    let wps = write_count as f64 / wt.as_secs_f64();
    println!(
        "  {} writes: {:>8?}  ({:.0} writes/sec)",
        write_count, wt, wps
    );
    println!();

    // Read throughput on the same data
    {
        let dir = tempfile::tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 64 * 1024 * 1024,
            ..Default::default()
        };
        let eng = LsmEngine::open(options).unwrap();
        for i in 0..write_count {
            let key = format!("K::{:012}", i).into_bytes();
            let val = format!(r#"{{"v":{}}}"#, i).into_bytes();
            eng.put(key, val).unwrap();
        }

        let read_count = 50_000;
        let rt = bench_read_throughput(&eng, read_count);
        let rps = read_count as f64 / rt.as_secs_f64();
        println!(
            "  {} reads:  {:>8?}  ({:.0} reads/sec)",
            read_count, rt, rps
        );
    }
    println!();

    // ── Section 3: HNSW Batch Construction ──
    println!("─── 3. HNSW Batch Construction ───");
    let vec_count = 5_000;
    let dim = 128;

    let ht_ind = bench_hnsw_individual(vec_count, dim);
    let ht_batch = bench_hnsw_batch(vec_count, dim);
    let hnsw_speedup = ht_ind.as_secs_f64() / ht_batch.as_secs_f64();

    println!("  {} vectors, {} dimensions:", vec_count, dim);
    println!("    Individual insert: {:>8?}", ht_ind);
    println!("    Batch insert:      {:>8?}", ht_batch);
    println!("    Speedup:           {:.2}x", hnsw_speedup);
    println!();

    println!("{}", "=".repeat(72));
    println!("  Benchmark complete.");
    println!("{}", "=".repeat(72));
}
