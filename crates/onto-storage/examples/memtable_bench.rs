//! Micro-benchmark for MemTable lookup performance

use std::collections::BTreeMap;
use std::time::Instant;

fn main() {
    println!("MemTable BTreeMap Lookup Benchmark");
    println!("==================================\n");

    // Create a BTreeMap with 50K entries
    let mut map: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let row_count = 50_000;

    for i in 0..row_count {
        let key = format!("K::{:012}", i).into_bytes();
        let val = format!(r#"{{"v":{}}}"#, i).into_bytes();
        map.insert(key, val);
    }

    println!("Inserted {} entries", row_count);

    // Benchmark point lookups
    let lookup_count = 50_000;
    let start = Instant::now();
    for i in 0..lookup_count {
        let key = format!("K::{:012}", i).into_bytes();
        let _ = map.get(&key);
    }
    let elapsed = start.elapsed();
    println!(
        "{} lookups: {:?} ({:.0} lookups/sec)",
        lookup_count,
        elapsed,
        lookup_count as f64 / elapsed.as_secs_f64()
    );

    // Benchmark with range query (like MemTable)
    let start = Instant::now();
    for i in 0..lookup_count {
        let key = format!("K::{:012}", i).into_bytes();
        let mut lower = Vec::with_capacity(key.len() + 8);
        lower.extend_from_slice(&key);
        lower.extend_from_slice(&0u64.to_be_bytes());

        let mut upper = Vec::with_capacity(key.len() + 8);
        upper.extend_from_slice(&key);
        upper.extend_from_slice(&u64::MAX.to_be_bytes());

        let _ = map.range(lower..=upper).next();
    }
    let elapsed = start.elapsed();
    println!(
        "{} range lookups: {:?} ({:.0} lookups/sec)",
        lookup_count,
        elapsed,
        lookup_count as f64 / elapsed.as_secs_f64()
    );
}
