//! Micro-benchmark for lock overhead

use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    println!("Lock Overhead Benchmark");
    println!("=======================\n");

    let lock = Arc::new(RwLock::new(42u64));
    let iterations = 1_000_000;

    // Benchmark read lock acquisition
    let start = Instant::now();
    for _ in 0..iterations {
        let _val = lock.read();
    }
    let elapsed = start.elapsed();
    println!(
        "{} read locks: {:?} ({:.0} locks/sec)",
        iterations,
        elapsed,
        iterations as f64 / elapsed.as_secs_f64()
    );

    // Benchmark write lock acquisition
    let start = Instant::now();
    for _ in 0..iterations {
        let _val = lock.write();
    }
    let elapsed = start.elapsed();
    println!(
        "{} write locks: {:?} ({:.0} locks/sec)",
        iterations,
        elapsed,
        iterations as f64 / elapsed.as_secs_f64()
    );
}
