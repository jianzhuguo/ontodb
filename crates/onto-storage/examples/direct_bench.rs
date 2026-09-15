//! Micro-benchmark for direct MemTable lookup without engine

use std::collections::BTreeMap;
use std::time::Instant;

fn main() {
    println!("Direct MemTable Lookup Benchmark");
    println!("================================\n");

    let row_count = 50_000;
    let mut map: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();

    // Insert data
    for i in 0..row_count {
        let key = format!("K::{:012}", i).into_bytes();
        let val = format!(r#"{{"v":{}}}"#, i).into_bytes();
        map.insert(key, val);
    }

    println!("Inserted {} entries", row_count);

    // Benchmark direct BTreeMap lookup
    let lookup_count = 50_000;
    let start = Instant::now();
    for i in 0..lookup_count {
        let key = format!("K::{:012}", i).into_bytes();
        let _ = map.get(&key);
    }
    let elapsed = start.elapsed();
    println!(
        "{} direct lookups: {:?} ({:.0} lookups/sec)",
        lookup_count,
        elapsed,
        lookup_count as f64 / elapsed.as_secs_f64()
    );
}
