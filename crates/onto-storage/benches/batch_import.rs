//! Benchmark: Batch import vs individual inserts

use onto_storage::{LsmEngine, StorageOptions};
use std::time::Instant;

fn main() {
    let row_count = 100_000;
    println!("=== Batch Import Benchmark ({} rows) ===\n", row_count);

    // ── Test 1: Individual puts ──
    {
        let dir = tempfile::tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 64 * 1024 * 1024,
            sync_wal_on_commit: false,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        let start = Instant::now();
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
        let elapsed = start.elapsed();
        let rate = row_count as f64 / elapsed.as_secs_f64();
        println!(
            "Individual put():  {:>8.2}s  ({:.0} rows/sec)",
            elapsed.as_secs_f64(),
            rate
        );
    }

    // ── Test 2: Batch put_batch ──
    {
        let dir = tempfile::tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 64 * 1024 * 1024,
            sync_wal_on_commit: false,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        // Build all entries
        let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..row_count)
            .map(|i| {
                let key = format!("Product::{:020}", i).into_bytes();
                let val = format!(
                    r#"{{"__class__":"Product","name":"item_{}","price":{}}}"#,
                    i,
                    (i * 10) % 10000
                )
                .into_bytes();
                (key, val)
            })
            .collect();

        let start = Instant::now();
        let imported = engine.put_batch(entries).unwrap();
        let elapsed = start.elapsed();
        let rate = imported as f64 / elapsed.as_secs_f64();
        println!(
            "put_batch():       {:>8.2}s  ({:.0} rows/sec)",
            elapsed.as_secs_f64(),
            rate
        );
    }

    // ── Test 3: Batch put_batch in chunks of 1000 ──
    {
        let dir = tempfile::tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 64 * 1024 * 1024,
            sync_wal_on_commit: false,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        let chunk_size = 1000;
        let start = Instant::now();
        let mut total = 0;
        for chunk_start in (0..row_count).step_by(chunk_size) {
            let chunk_end = (chunk_start + chunk_size).min(row_count);
            let entries: Vec<(Vec<u8>, Vec<u8>)> = (chunk_start..chunk_end)
                .map(|i| {
                    let key = format!("Product::{:020}", i).into_bytes();
                    let val = format!(
                        r#"{{"__class__":"Product","name":"item_{}","price":{}}}"#,
                        i,
                        (i * 10) % 10000
                    )
                    .into_bytes();
                    (key, val)
                })
                .collect();
            total += engine.put_batch(entries).unwrap();
        }
        let elapsed = start.elapsed();
        let rate = total as f64 / elapsed.as_secs_f64();
        println!(
            "put_batch(1000):   {:>8.2}s  ({:.0} rows/sec)",
            elapsed.as_secs_f64(),
            rate
        );
    }

    println!("\n=== Benchmark complete ===");
}
