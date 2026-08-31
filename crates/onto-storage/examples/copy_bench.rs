//! Micro-benchmark for value copy overhead

use std::time::Instant;

fn main() {
    println!("Value Copy Benchmark");
    println!("====================\n");

    let iterations = 1_000_000;
    let value = vec![0u8; 100]; // 100 byte value
    
    // Benchmark to_vec() copy
    let start = Instant::now();
    for _ in 0..iterations {
        let _copy = value.to_vec();
    }
    let elapsed = start.elapsed();
    println!("{} to_vec() copies: {:?} ({:.0} copies/sec)", 
        iterations, elapsed, iterations as f64 / elapsed.as_secs_f64());
    
    // Benchmark clone()
    let start = Instant::now();
    for _ in 0..iterations {
        let _copy = value.clone();
    }
    let elapsed = start.elapsed();
    println!("{} clone() copies: {:?} ({:.0} copies/sec)", 
        iterations, elapsed, iterations as f64 / elapsed.as_secs_f64());
}
