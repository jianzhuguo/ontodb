// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Concurrent read-write lock separation benchmark.

use crate::executor::QueryExecutor;
use crate::parser::QueryParser;
use crate::QueryAst;
use onto_storage::StorageOptions;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn setup_bench(row_count: usize) -> (Arc<QueryExecutor>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 4 * 1024 * 1024,
        ..Default::default()
    };
    let engine = Arc::new(onto_storage::LsmEngine::open(options).unwrap());
    let ontology_store = onto_ontology::OntologyStore::new(engine.clone());
    let executor = Arc::new(QueryExecutor::new(engine, ontology_store));

    for i in 0..row_count {
        let ast = QueryAst::Insert {
            class: "Product".to_string(),
            columns: vec![
                "name".to_string(),
                "price".to_string(),
                "category".to_string(),
            ],
            values: vec![
                crate::parser::LiteralValue::String(format!("item_{}", i)),
                crate::parser::LiteralValue::Int((i as i64 * 10) % 10000),
                crate::parser::LiteralValue::String(format!("cat_{}", i % 10)),
            ],
        };
        executor.execute(&ast).unwrap();
    }

    (executor, dir)
}

/// Sequential SELECT baseline (single-threaded, write lock).
fn bench_sequential_select(executor: &QueryExecutor, query: &str, iterations: usize) -> Duration {
    let ast = QueryParser::parse(query).unwrap();
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = executor.execute(&ast).unwrap();
    }
    start.elapsed()
}

/// Concurrent SELECT via execute_read (read lock — allows parallelism).
fn bench_concurrent_read(
    executor: Arc<QueryExecutor>,
    query: &str,
    iterations: usize,
    num_threads: usize,
) -> Duration {
    let queries_per_thread = iterations / num_threads;
    let remainder = iterations % num_threads;

    let start = Instant::now();
    let mut handles = Vec::new();

    for t in 0..num_threads {
        let count = queries_per_thread + if t < remainder { 1 } else { 0 };
        let exec = Arc::clone(&executor);
        let query_owned = query.to_string();
        handles.push(std::thread::spawn(move || {
            let ast = QueryParser::parse(&query_owned).unwrap();
            for _ in 0..count {
                let _ = exec.execute_read(&ast).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    start.elapsed()
}

/// Concurrent SELECT via execute (write lock — serialized, the old behavior).
fn bench_concurrent_write_lock_select(
    executor: Arc<QueryExecutor>,
    query: &str,
    iterations: usize,
    num_threads: usize,
) -> Duration {
    let queries_per_thread = iterations / num_threads;
    let remainder = iterations % num_threads;

    let start = Instant::now();
    let mut handles = Vec::new();

    for t in 0..num_threads {
        let count = queries_per_thread + if t < remainder { 1 } else { 0 };
        let exec = Arc::clone(&executor);
        let query_owned = query.to_string();
        handles.push(std::thread::spawn(move || {
            let ast = QueryParser::parse(&query_owned).unwrap();
            for _ in 0..count {
                let _ = exec.execute(&ast).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    start.elapsed()
}

/// Mixed read + write concurrently.
fn bench_mixed_read_write(
    executor: Arc<QueryExecutor>,
    select_query: &str,
    iterations: usize,
    num_readers: usize,
    num_writers: usize,
) -> Duration {
    let reads_per_thread = iterations / num_readers;
    let writes_per_thread = iterations / num_writers;

    let start = Instant::now();
    let mut handles = Vec::new();

    // Readers (execute_read — read lock)
    for _ in 0..num_readers {
        let exec = Arc::clone(&executor);
        let q = select_query.to_string();
        handles.push(std::thread::spawn(move || {
            let ast = QueryParser::parse(&q).unwrap();
            for _ in 0..reads_per_thread {
                let _ = exec.execute_read(&ast).unwrap();
            }
        }));
    }

    // Writers (execute — write lock)
    for i in 0..num_writers {
        let exec = Arc::clone(&executor);
        handles.push(std::thread::spawn(move || {
            for j in 0..writes_per_thread {
                let ast = QueryAst::Insert {
                    class: "Product".to_string(),
                    columns: vec![
                        "name".to_string(),
                        "price".to_string(),
                        "category".to_string(),
                    ],
                    values: vec![
                        crate::parser::LiteralValue::String(format!("bench_{}_{}", i, j)),
                        crate::parser::LiteralValue::Int(999),
                        crate::parser::LiteralValue::String("benchmark".to_string()),
                    ],
                };
                let _ = exec.execute(&ast);
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    start.elapsed()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW_COUNT: usize = 100_000;
    const ITERATIONS: usize = 50;

    #[test]
    #[ignore]
    fn bench_read_write_lock_separation() {
        let (executor, _dir) = setup_bench(ROW_COUNT);

        let simple_select = "SELECT * FROM Product WHERE price > 5000";
        let filter_select =
            "SELECT name, price FROM Product WHERE category = 'cat_3' AND price > 1000";
        let order_select =
            "SELECT name, price FROM Product WHERE price > 2000 ORDER BY price DESC LIMIT 10";
        let count_select = "SELECT COUNT(*) FROM Product WHERE price > 3000";

        println!();
        println!("{}", "=".repeat(72));
        println!("  OntoDB Concurrent Read-Write Lock Benchmark");
        println!("  Data: {} rows, {} iterations/test", ROW_COUNT, ITERATIONS);
        println!("{}", "=".repeat(72));
        println!();

        let queries = &[
            ("Simple SELECT (price > 5000)", simple_select),
            ("Filtered SELECT (category + price)", filter_select),
            ("ORDER BY + LIMIT", order_select),
            ("COUNT(*) aggregate", count_select),
        ];

        for (label, query) in queries {
            println!("--- {} ---", label);

            // Sequential baseline
            let seq = bench_sequential_select(&executor, query, ITERATIONS);
            println!("  Sequential (1 thread): {:?}", seq);

            for &nt in &[2, 4, 8] {
                let wt = bench_concurrent_write_lock_select(
                    Arc::clone(&executor),
                    query,
                    ITERATIONS,
                    nt,
                );
                let rt = bench_concurrent_read(Arc::clone(&executor), query, ITERATIONS, nt);
                let speedup = if rt.as_micros() > 0 {
                    wt.as_secs_f64() / rt.as_secs_f64()
                } else {
                    f64::INFINITY
                };
                println!(
                    "  {} threads:  write-lock={:>8?}  read-lock={:>8?}  speedup={:.2}x",
                    nt, wt, rt, speedup
                );
            }
            println!();
        }

        // Mixed read + write
        println!(
            "--- Mixed: 4 readers + 1 writer ({} iters each) ---",
            ITERATIONS
        );
        let mixed = bench_mixed_read_write(Arc::clone(&executor), simple_select, ITERATIONS, 4, 1);
        println!("  Total: {:?}", mixed);

        // Write-only baseline for comparison
        println!();
        println!(
            "--- Write-only baseline: 5 threads INSERT ({} iters each) ---",
            ITERATIONS
        );
        let write_only = {
            let start = Instant::now();
            let mut handles = Vec::new();
            for i in 0..5usize {
                let exec = Arc::clone(&executor);
                handles.push(std::thread::spawn(move || {
                    for j in 0..ITERATIONS {
                        let ast = QueryAst::Insert {
                            class: "Product".to_string(),
                            columns: vec![
                                "name".to_string(),
                                "price".to_string(),
                                "category".to_string(),
                            ],
                            values: vec![
                                crate::parser::LiteralValue::String(format!("wo_{}_{}", i, j)),
                                crate::parser::LiteralValue::Int(123),
                                crate::parser::LiteralValue::String("write_only".to_string()),
                            ],
                        };
                        let _ = exec.execute(&ast);
                    }
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
            start.elapsed()
        };
        println!("  Total: {:?}", write_only);

        println!();
        println!("{}", "=".repeat(72));
        println!("  Benchmark complete.");
        println!("{}", "=".repeat(72));
        println!();
    }
}
