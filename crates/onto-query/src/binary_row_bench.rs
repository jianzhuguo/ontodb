//! BinaryRow integration performance benchmark.
//!
//! Measures latency and throughput for all scan paths that use BinaryRow,
//! comparing binary fast-path against JSON fallback at the parse level.

use crate::executor::QueryExecutor;
use crate::parser::QueryParser;
use crate::QueryAst;
use onto_core::binary_row::BinaryRow;
use onto_storage::StorageOptions;
use serde_json::{Map, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ROW_COUNT: usize = 5_000;
const WARMUP_ITERS: usize = 2;
const BENCH_ITERS: usize = 20;
const PARSE_ITERS: usize = 20_000;

/// Insert ROW_COUNT rows into the executor, returning (executor, tmpdir).
fn setup() -> (Arc<QueryExecutor>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 64 * 1024 * 1024,
        ..Default::default()
    };
    let engine = Arc::new(std::sync::RwLock::new(
        onto_storage::LsmEngine::open(options).unwrap(),
    ));
    let ontology_store = onto_ontology::OntologyStore::new(engine.clone());
    let executor = Arc::new(QueryExecutor::new(engine, ontology_store));

    for i in 0..ROW_COUNT {
        let ast = QueryAst::Insert {
            class: "Product".to_string(),
            columns: vec![
                "name".to_string(),
                "price".to_string(),
                "category".to_string(),
                "rating".to_string(),
            ],
            values: vec![
                crate::parser::LiteralValue::String(format!("item_{:06}", i)),
                crate::parser::LiteralValue::Int((i as i64 * 7) % 10000),
                crate::parser::LiteralValue::String(format!("cat_{}", i % 20)),
                crate::parser::LiteralValue::Float(1.0 + (i as f64 % 5.0)),
            ],
        };
        executor.execute(&ast).unwrap();
    }

    (executor, dir)
}

/// Run a query N times (write lock), return total duration (after warmup).
fn bench_query(executor: &QueryExecutor, query: &str, iters: usize) -> Duration {
    let ast = QueryParser::parse(query).unwrap();
    for _ in 0..WARMUP_ITERS {
        let _ = executor.execute(&ast).unwrap();
    }
    let start = Instant::now();
    for _ in 0..iters {
        let _ = executor.execute(&ast).unwrap();
    }
    start.elapsed()
}

/// Run a query N times (read lock), return total duration (after warmup).
fn bench_query_read(executor: &QueryExecutor, query: &str, iters: usize) -> Duration {
    let ast = QueryParser::parse(query).unwrap();
    for _ in 0..WARMUP_ITERS {
        let _ = executor.execute_read(&ast).unwrap();
    }
    let start = Instant::now();
    for _ in 0..iters {
        let _ = executor.execute_read(&ast).unwrap();
    }
    start.elapsed()
}

/// Micro-bench: BinaryRow::parse + to_map vs serde_json::from_slice
/// on the same binary-encoded row bytes (written by doc_to_storage_bytes).
fn bench_parse_comparison(sample_bytes: &[u8], iters: usize) -> (Duration, Duration) {
    // BinaryRow path: parse binary → Map
    let start = Instant::now();
    for _ in 0..iters {
        let row = BinaryRow::parse(sample_bytes).unwrap();
        let _map = row.to_map().unwrap();
    }
    let binary_dur = start.elapsed();

    // JSON path: encode to JSON bytes once, then bench serde_json decode
    let row = BinaryRow::parse(sample_bytes).unwrap();
    let map = row.to_map().unwrap();
    let json_bytes = serde_json::to_vec(&Value::Object(map)).unwrap();

    let start = Instant::now();
    for _ in 0..iters {
        let _: Map<String, Value> = match serde_json::from_slice::<Value>(&json_bytes) {
            Ok(Value::Object(m)) => m,
            _ => panic!("expected object"),
        };
    }
    let json_dur = start.elapsed();

    (binary_dur, json_dur)
}

/// Micro-bench: BinaryRow field lookup vs serde_json Map::get
fn bench_field_lookup(sample_bytes: &[u8], json_bytes: &[u8], iters: usize) -> (Duration, Duration) {
    // BinaryRow: find_field + field_value_raw (zero-copy)
    let start = Instant::now();
    for _ in 0..iters {
        let row = BinaryRow::parse(sample_bytes).unwrap();
        if let Some(idx) = row.find_field("price") {
            let (_tag, _raw) = row.field_value_raw(idx);
            // Also test string field
            let _s = row.get_str("name");
        }
    }
    let binary_dur = start.elapsed();

    // JSON: parse entire document then get fields
    let start = Instant::now();
    for _ in 0..iters {
        let val: Value = serde_json::from_slice(json_bytes).unwrap();
        if let Value::Object(ref doc) = val {
            let _price = doc.get("price");
            let _name = doc.get("name").and_then(|v| v.as_str());
        }
    }
    let json_dur = start.elapsed();

    (binary_dur, json_dur)
}

/// Micro-bench: BinaryRow filter eval (price > 5000) vs JSON filter eval
fn bench_filter_eval(sample_bytes: &[u8], json_bytes: &[u8], iters: usize) -> (Duration, Duration) {
    // BinaryRow: parse + inline field compare (no full deserialization)
    let start = Instant::now();
    for _ in 0..iters {
        let row = BinaryRow::parse(sample_bytes).unwrap();
        // Inline filter eval: find_field + raw compare
        if let Some(idx) = row.find_field("price") {
            let (tag, raw) = row.field_value_raw(idx);
            if tag == onto_core::binary_row::TAG_INT {
                if let Ok(arr) = <[u8; 8]>::try_from(raw) {
                    let _val = i64::from_be_bytes(arr);
                    let _cmp = _val > 5000;
                }
            }
        }
    }
    let binary_dur = start.elapsed();

    // JSON: parse full document then compare
    let start = Instant::now();
    for _ in 0..iters {
        let val: Value = serde_json::from_slice(json_bytes).unwrap();
        if let Value::Object(ref doc) = val {
            let _cmp = doc.get("price").and_then(|v| v.as_i64()).map_or(false, |n| n > 5000);
        }
    }
    let json_dur = start.elapsed();

    (binary_dur, json_dur)
}

// ─── Output helpers ─────────────────────────────────────────────────────────

fn print_header() {
    println!();
    println!("{}", "=".repeat(78));
    println!("  BinaryRow Integration Performance Benchmark");
    println!(
        "  Data: {} rows, {} bench iters (+ {} warmup), {} parse iters",
        ROW_COUNT, BENCH_ITERS, WARMUP_ITERS, PARSE_ITERS
    );
    println!("{}", "=".repeat(78));
    println!();
}

fn print_result(label: &str, dur: Duration, iters: usize) {
    let avg_us = dur.as_micros() as f64 / iters as f64;
    let qps = iters as f64 / dur.as_secs_f64();
    println!(
        "  {:<50} avg={:>8.1} us  throughput={:>8.0} qps",
        label, avg_us, qps
    );
}

fn print_comparison(label: &str, binary_dur: Duration, json_dur: Duration, iters: usize) {
    let bin_avg = binary_dur.as_micros() as f64 / iters as f64;
    let json_avg = json_dur.as_micros() as f64 / iters as f64;
    let speedup = if bin_avg > 0.0 { json_avg / bin_avg } else { f64::INFINITY };
    println!(
        "  {:<35} binary={:>7.1} us  json={:>7.1} us  speedup={:.2}x",
        label, bin_avg, json_avg, speedup
    );
}

// ─── Test entry point ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn bench_binary_row_integration() {
        let (executor, _dir) = setup();
        print_header();

        // ─────────────────────────────────────────────────────────────────────
        // 1. End-to-end: SeqScan write path
        // ─────────────────────────────────────────────────────────────────────
        println!("── SeqScan — write path (execute) ──");
        let cases: &[(&str, &str)] = &[
            ("full scan, no filter", "SELECT * FROM Product"),
            ("simple filter (price > 5000)", "SELECT * FROM Product WHERE price > 5000"),
            ("compound filter (category + price)", "SELECT name, price FROM Product WHERE category = 'cat_3' AND price > 1000"),
        ];
        for (label, q) in cases {
            let d = bench_query(&executor, q, BENCH_ITERS);
            print_result(label, d, BENCH_ITERS);
        }
        println!();

        // ─────────────────────────────────────────────────────────────────────
        // 2. End-to-end: SeqScan read path
        // ─────────────────────────────────────────────────────────────────────
        println!("── SeqScan — read path (execute_read) ──");
        for (label, q) in cases {
            let d = bench_query_read(&executor, q, BENCH_ITERS);
            print_result(label, d, BENCH_ITERS);
        }
        println!();

        // ─────────────────────────────────────────────────────────────────────
        // 3. End-to-end: MATCH semantic query (execute_match_read)
        // ─────────────────────────────────────────────────────────────────────
        println!("── MATCH — execute_match_read ──");
        let d = bench_query_read(&executor, "MATCH (p:Product) RETURN p.name, p.price", BENCH_ITERS);
        print_result("MATCH (p:Product) RETURN p.name, p.price", d, BENCH_ITERS);
        let d = bench_query_read(&executor, "MATCH (p:Product) WHERE p.price > 5000 RETURN p.name", BENCH_ITERS);
        print_result("MATCH WHERE price > 5000 RETURN name", d, BENCH_ITERS);
        println!();

        // ─────────────────────────────────────────────────────────────────────
        // 4. End-to-end: Post-scan operations
        // ─────────────────────────────────────────────────────────────────────
        println!("── Post-scan operations ──");
        let d = bench_query(&executor, "SELECT name, price FROM Product WHERE price > 2000 ORDER BY price DESC LIMIT 10", BENCH_ITERS);
        print_result("ORDER BY + LIMIT (price > 2000)", d, BENCH_ITERS);
        let d = bench_query(&executor, "SELECT COUNT(*) FROM Product WHERE price > 3000", BENCH_ITERS);
        print_result("COUNT(*) WHERE price > 3000", d, BENCH_ITERS);
        let d = bench_query(&executor, "SELECT category, AVG(price) FROM Product GROUP BY category", BENCH_ITERS);
        print_result("GROUP BY + AVG", d, BENCH_ITERS);
        println!();

        // ─────────────────────────────────────────────────────────────────────
        // 5. End-to-end: Index scan paths
        // ─────────────────────────────────────────────────────────────────────
        println!("── Index scan paths ──");
        let _ = executor.execute(&QueryAst::CreateIndex {
            class: "Product".to_string(),
            column: "price".to_string(),
        });
        let d = bench_query(&executor, "SELECT * FROM Product WHERE price = 500", BENCH_ITERS);
        print_result("Index lookup (price = 500)", d, BENCH_ITERS);
        let d = bench_query(&executor, "SELECT * FROM Product WHERE price > 5000", BENCH_ITERS);
        print_result("Index scan (price > 5000)", d, BENCH_ITERS);
        println!();

        // ─────────────────────────────────────────────────────────────────────
        // 6. Micro-bench: parse + to_map
        // ─────────────────────────────────────────────────────────────────────
        println!("── Micro: BinaryRow::parse+to_map vs serde_json::from_slice ──");
        let engine_guard = executor.engine();
        let entries = engine_guard.scan_prefix(b"Product::").unwrap();
        let sample_bytes = entries.first().map(|(_, v)| v.clone()).unwrap();
        drop(engine_guard);

        let (bin_parse, json_parse) = bench_parse_comparison(&sample_bytes, PARSE_ITERS);
        print_comparison("parse + to_map", bin_parse, json_parse, PARSE_ITERS);
        println!();

        // ─────────────────────────────────────────────────────────────────────
        // 7. Micro-bench: field lookup
        // ─────────────────────────────────────────────────────────────────────
        println!("── Micro: field lookup (find_field vs Map::get) ──");
        let row = BinaryRow::parse(&sample_bytes).unwrap();
        let map = row.to_map().unwrap();
        let json_bytes = serde_json::to_vec(&Value::Object(map)).unwrap();
        let (bin_lookup, json_lookup) = bench_field_lookup(&sample_bytes, &json_bytes, PARSE_ITERS);
        print_comparison("field lookup (price + name)", bin_lookup, json_lookup, PARSE_ITERS);
        println!();

        // ─────────────────────────────────────────────────────────────────────
        // 8. Micro-bench: filter eval (price > 5000)
        // ─────────────────────────────────────────────────────────────────────
        println!("── Micro: filter eval — price > 5000 ──");
        let (bin_filt, json_filt) = bench_filter_eval(&sample_bytes, &json_bytes, PARSE_ITERS);
        print_comparison("price > 5000", bin_filt, json_filt, PARSE_ITERS);
        println!();

        println!("{}", "=".repeat(78));
        println!("  Benchmark complete.");
        println!("{}", "=".repeat(78));
        println!();
    }
}
