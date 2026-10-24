//! Large-scale delete + compaction regression for the static-workspace zstd fix.
//!
//! Isolated: data lives in a fresh temp dir. Never touches `./ontodb_data`
//! or any running server.
//!
//! Run: `cargo run -p onto-storage --release --example stress_delete_compaction`
//!
//! Workload mirrors the production crash: small memtable → frequent SST
//! `flush_block` (zstd), then mass delete → tombstones → `flush_compaction`.
//! Several worker threads give one thread_local static CCtx each.

use std::sync::Arc;
use std::time::Instant;

use onto_storage::engine::LsmEngine;
use onto_storage::options::StorageOptions;

const KEYS_PER_WORKER: usize = 60_000;
const WORKERS: usize = 4;
const VALUE_LEN: usize = 300;
const DELETE_RATIO: usize = 70; // percent
const WAVES: usize = 4; // insert → delete → compact, repeated

fn compressible_value(i: usize) -> Vec<u8> {
    // Highly compressible payload so zstd actually runs its sequence path.
    let mut v = Vec::with_capacity(VALUE_LEN);
    v.extend_from_slice(format!("rec-{i:08}|").as_bytes());
    while v.len() < VALUE_LEN {
        v.extend_from_slice(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789;");
    }
    v.truncate(VALUE_LEN);
    v
}

fn main() {
    let dir = tempfile::tempdir().expect("tempdir");
    println!("[stress] data_dir = {}", dir.path().display());
    assert!(
        !dir.path().ends_with("ontodb_data"),
        "refusing to run against production data dir"
    );

    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        // Small memtable → many SST flushes → many zstd compress calls.
        memtable_size_limit: 512 * 1024,
        block_size: 4096,
        size_ratio: 2,
        compression_level: 2,
        sync_wal_on_commit: false,
        ..Default::default()
    };

    let engine = Arc::new(LsmEngine::open(options).expect("open engine"));
    let t0 = Instant::now();

    let mut total_inserted = 0usize;
    let mut total_deleted = 0usize;

    for wave in 0..WAVES {
        let prefix = format!("w{wave:02}");
        let keys_per_worker = KEYS_PER_WORKER;

        // Insert wave (each worker hits its own static CCtx).
        let mut handles = Vec::new();
        for w in 0..WORKERS {
            let engine = Arc::clone(&engine);
            let prefix = prefix.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..keys_per_worker {
                    let key = format!("{prefix}:w{w:02}:k{i:07}");
                    engine
                        .put(key.into_bytes(), compressible_value(i))
                        .expect("put");
                }
            }));
        }
        for h in handles {
            h.join().expect("insert worker");
        }
        total_inserted += WORKERS * keys_per_worker;

        // Mass deletes (tombstones) + concurrent flush.
        let mut handles = Vec::new();
        for w in 0..WORKERS {
            let engine = Arc::clone(&engine);
            let prefix = prefix.clone();
            handles.push(std::thread::spawn(move || {
                let n = keys_per_worker * DELETE_RATIO / 100;
                for i in 0..n {
                    let key = format!("{prefix}:w{w:02}:k{i:07}");
                    engine.delete(key.into_bytes()).expect("delete");
                }
            }));
        }
        let engine_flush = Arc::clone(&engine);
        let flusher = std::thread::spawn(move || {
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(20));
                let _ = engine_flush.flush();
            }
        });
        for h in handles {
            h.join().expect("delete worker");
        }
        flusher.join().expect("flusher");
        total_deleted += WORKERS * keys_per_worker * DELETE_RATIO / 100;

        // Force compaction — crash site: flush_block → zstd under tombstone merge.
        engine.flush().expect("flush");
        let rt = Instant::now();
        engine.flush_compaction().expect("compaction");
        println!(
            "[stress] wave {}/{WAVES} insert+delete done, compaction in {:.1?} (total ins={} del={})",
            wave + 1,
            rt.elapsed(),
            total_inserted,
            total_deleted
        );
    }

    // Quiesce: drain background compaction + deferred deletes so verify
    // is not racing a file removal. (Separate from the zstd crash we are
    // regressing — this just lets us read a stable LSM.)
    for _ in 0..3 {
        engine.flush().expect("flush");
        let _ = engine.flush_compaction();
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // Final verify across all waves: keys never deleted must live,
    // deleted keys must stay gone (no resurrection).
    let mut live = 0usize;
    let mut missing = 0usize;
    let mut resurrected = 0usize;
    let mut io_errors = 0usize;
    let n_del = KEYS_PER_WORKER * DELETE_RATIO / 100;
    for wave in 0..WAVES {
        for w in 0..WORKERS {
            for i in 0..KEYS_PER_WORKER {
                let key = format!("w{wave:02}:w{w:02}:k{i:07}");
                match engine.get(key.as_bytes()) {
                    Ok(got) => {
                        if i < n_del {
                            if got.is_some() {
                                resurrected += 1;
                            }
                        } else if got.is_some() {
                            live += 1;
                        } else {
                            missing += 1;
                        }
                    }
                    Err(e) => {
                        io_errors += 1;
                        if io_errors <= 3 {
                            eprintln!("[stress] get {key} err: {e}");
                        }
                    }
                }
            }
        }
    }

    let expect_live = total_inserted - total_deleted;
    println!(
        "[stress] verify live={live} (expect {expect_live}) missing={missing} resurrected={resurrected} io_err={io_errors}"
    );
    assert_eq!(io_errors, 0, "SST NotFound during read — file lifetime race");
    assert_eq!(resurrected, 0, "tombstones resurrected — delete/compaction bug");
    assert_eq!(missing, 0, "live keys lost during compaction");
    assert_eq!(live, expect_live, "live count mismatch");

    println!(
        "[stress] PASS inserted={total_inserted} deleted={total_deleted} elapsed={:.1?} dir={}",
        t0.elapsed(),
        dir.path().display()
    );
}
