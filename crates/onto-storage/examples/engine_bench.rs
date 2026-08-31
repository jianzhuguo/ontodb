//! Micro-benchmark for engine-like get() with lock

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use parking_lot::RwLock;

struct MemTable {
    data: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl MemTable {
    fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.data.get(key).map(|v| v.as_slice())
    }
}

fn main() {
    println!("Engine-like Get Benchmark");
    println!("========================\n");

    let row_count = 50_000;
    let mut mt = MemTable { data: BTreeMap::new() };
    
    for i in 0..row_count {
        let key = format!("K::{:012}", i).into_bytes();
        let val = format!(r#"{{"v":{}}}"#, i).into_bytes();
        mt.data.insert(key, val);
    }
    
    let memtable = Arc::new(RwLock::new(mt));
    println!("Inserted {} entries", row_count);
    
    // Benchmark with lock
    let lookup_count = 50_000;
    let start = Instant::now();
    for i in 0..lookup_count {
        let key = format!("K::{:012}", i).into_bytes();
        let ws = memtable.read();
        let _ = ws.get(&key).map(|v| v.to_vec());
    }
    let elapsed = start.elapsed();
    println!("{} lookups with lock: {:?} ({:.0} lookups/sec)", 
        lookup_count, elapsed, lookup_count as f64 / elapsed.as_secs_f64());
    
    // Benchmark without lock (direct access)
    let start = Instant::now();
    for i in 0..lookup_count {
        let key = format!("K::{:012}", i).into_bytes();
        let _ = memtable.read().get(&key).map(|v| v.to_vec());
    }
    let elapsed = start.elapsed();
    println!("{} lookups without lock: {:?} ({:.0} lookups/sec)", 
        lookup_count, elapsed, lookup_count as f64 / elapsed.as_secs_f64());
}
