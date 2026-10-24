//! One-off: restore a `__vec_meta__` row so an existing HNSW snapshot file
//! (`indexes/{class}_{column}.hnsw`) is picked up at startup.
//!
//! Usage: cargo run -p onto-storage --example restore_vec_meta -- <data_dir> <class> <column> <dim> <metric>
//! e.g.  cargo run -p onto-storage --example restore_vec_meta -- \
//!           E:/sembio/data/ontodb_clean sembio.Compound fp 1024 Tanimoto

use onto_storage::engine::LsmEngine;
use onto_storage::options::StorageOptions;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!("usage: restore_vec_meta <data_dir> <class> <column> <dim> <metric>");
        std::process::exit(2);
    }
    let data_dir = std::path::PathBuf::from(&args[1]);
    let class = &args[2];
    let column = &args[3];
    let dim: usize = args[4].parse().expect("dim");
    let metric = &args[5];

    let options = StorageOptions {
        data_dir: data_dir.clone(),
        ..Default::default()
    };
    let engine = LsmEngine::open(options).expect("open engine");

    let meta = serde_json::json!({
        "class": class,
        "column": column,
        "dimension": dim,
        "metric": metric,
        "m": 8,
        "ef_construction": 32,
        "ef_search": 64,
    });
    let mut key = Vec::new();
    key.extend_from_slice(b"__vec_meta__");
    key.extend_from_slice(class.as_bytes());
    key.extend_from_slice(b"__");
    key.extend_from_slice(column.as_bytes());
    let val = serde_json::to_vec(&meta).expect("serialize meta");

    let snap = data_dir
        .join("indexes")
        .join(format!("{class}_{column}.hnsw"));
    if !snap.exists() {
        eprintln!("snapshot NOT found: {}", snap.display());
        std::process::exit(1);
    }
    println!("snapshot ok: {} ({} bytes)", snap.display(), {
        std::fs::metadata(&snap).unwrap().len()
    });

    engine.put(key.clone(), val).expect("put meta");
    engine.flush().expect("flush");
    let _ = engine.flush_compaction();
    println!(
        "wrote __vec_meta__ for {}.{} dim={dim} metric={metric}",
        class, column
    );

    // Verify readback.
    match engine.get(&key).expect("get meta") {
        Some(v) => println!("readback ok: {}", String::from_utf8_lossy(&v)),
        None => {
            eprintln!("readback FAILED");
            std::process::exit(1);
        }
    }
}
