//! Phase 5 Multi-Modal Expansion Benchmark
//!
//! Tests all new modules added in Phase 5:
//! - GIS (WKB encode/decode, spatial predicates)
//! - R*tree (insert, search, KNN)
//! - Geohash index (insert, search)
//! - Spatio-temporal index (insert, query)
//! - STTRL rules (rule evaluation)
//! - Time series (DTW, anomaly detection)

use onto_core::geo::{self, Coord, Geometry};
use onto_core::geohash_index::GeohashIndex;
use onto_core::rtree::{BBox, RTree};
use onto_core::spatiotemporal::{STIndex, STPoint, STQuery};
use onto_core::sttrl::{Region, Rule, RuleEngine, RuleType, SpatioTemporalEvent};
use onto_core::time_series::{self, TimeSeries, DataPoint, Timestamp};
use std::collections::HashMap;
use std::time::Instant;

fn main() {
    println!("{}", "=".repeat(70));
    println!("  OntoDB Phase 5 - Multi-Modal Expansion Benchmark");
    println!("{}", "=".repeat(70));
    println!();

    bench_gis();
    bench_rtree();
    bench_geohash();
    bench_spatiotemporal();
    bench_sttrl();
    bench_timeseries();

    println!("{}", "=".repeat(70));
    println!("  Benchmark complete.");
    println!("{}", "=".repeat(70));
}

// ── GIS Benchmark ──

fn bench_gis() {
    println!("─── 1. GIS (Geometry + Spatial Predicates) ───");

    // WKB encode/decode
    let poly = Geometry::Polygon(vec![vec![
        Coord::new(0.0, 0.0),
        Coord::new(10.0, 0.0),
        Coord::new(10.0, 10.0),
        Coord::new(0.0, 10.0),
        Coord::new(0.0, 0.0),
    ]]);

    let n = 100_000;
    let start = Instant::now();
    for _ in 0..n {
        let wkb = poly.to_wkb();
        let _ = Geometry::from_wkb(&wkb);
    }
    let elapsed = start.elapsed();
    println!("  WKB roundtrip ({}x): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    // Spatial predicates
    let a = Geometry::Polygon(vec![vec![
        Coord::new(0.0, 0.0), Coord::new(10.0, 0.0),
        Coord::new(10.0, 10.0), Coord::new(0.0, 10.0), Coord::new(0.0, 0.0),
    ]]);
    let b = Geometry::Point(Coord::new(5.0, 5.0));

    let start = Instant::now();
    for _ in 0..n {
        let _ = geo::contains(&a, &b);
    }
    let elapsed = start.elapsed();
    println!("  ST_Contains ({}x): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    let start = Instant::now();
    for _ in 0..n {
        let _ = geo::intersects(&a, &a);
    }
    let elapsed = start.elapsed();
    println!("  ST_Intersects ({}x): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    // Distance calculation
    let p1 = Geometry::Point(Coord::new(116.4, 39.9));
    let p2 = Geometry::Point(Coord::new(121.5, 31.2));

    let start = Instant::now();
    for _ in 0..n {
        let _ = geo::distance(&p1, &p2);
    }
    let elapsed = start.elapsed();
    println!("  ST_Distance ({}x): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    // Geohash encode
    let start = Instant::now();
    for _ in 0..n {
        let _ = geo::geohash_encode(39.9, 116.4, 8);
    }
    let elapsed = start.elapsed();
    println!("  Geohash encode ({}x): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    println!();
}

// ── R*tree Benchmark ──

fn bench_rtree() {
    println!("─── 2. R*tree Spatial Index ───");

    let n = 100_000;
    let mut tree = RTree::new();

    // Insert
    let start = Instant::now();
    for i in 0..n {
        let x = (i % 1000) as f64;
        let y = (i / 1000) as f64;
        tree.insert(BBox::new(x, y, x + 1.0, y + 1.0), format!("item_{}", i));
    }
    let elapsed = start.elapsed();
    println!("  Insert ({} items): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    // Range search
    let start = Instant::now();
    let mut total = 0;
    for _ in 0..1000 {
        let results = tree.search(&BBox::new(0.0, 0.0, 100.0, 100.0));
        total += results.len();
    }
    let elapsed = start.elapsed();
    println!("  Range search (1000x): {:.2}s ({:.0}/sec)", elapsed.as_secs_f64(), 1000.0 / elapsed.as_secs_f64());

    // KNN search
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = tree.knn(50.0, 50.0, 10);
    }
    let elapsed = start.elapsed();
    println!("  KNN search (1000x): {:.2}s ({:.0}/sec)", elapsed.as_secs_f64(), 1000.0 / elapsed.as_secs_f64());

    println!();
}

// ── Geohash Index Benchmark ──

fn bench_geohash() {
    println!("─── 3. Geohash Spatial Index ───");

    let n = 100_000;
    let mut idx = GeohashIndex::with_precision(8);

    // Insert
    let start = Instant::now();
    for i in 0..n {
        let lat = 39.0 + (i % 1000) as f64 * 0.001;
        let lon = 116.0 + (i / 1000) as f64 * 0.001;
        idx.insert(&format!("entity_{}", i), lat, lon);
    }
    let elapsed = start.elapsed();
    println!("  Insert ({} items): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    // Exact search
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = idx.exact_search(39.5, 116.5);
    }
    let elapsed = start.elapsed();
    println!("  Exact search (1000x): {:.2}s ({:.0}/sec)", elapsed.as_secs_f64(), 1000.0 / elapsed.as_secs_f64());

    // Prefix search
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = idx.prefix_search("wx4g");
    }
    let elapsed = start.elapsed();
    println!("  Prefix search (1000x): {:.2}s ({:.0}/sec)", elapsed.as_secs_f64(), 1000.0 / elapsed.as_secs_f64());

    println!();
}

// ── Spatio-Temporal Index Benchmark ──

fn bench_spatiotemporal() {
    println!("─── 4. Spatio-Temporal Index ───");

    let n = 100_000;
    let mut idx = STIndex::world();

    // Insert
    let start = Instant::now();
    for i in 0..n {
        idx.insert(STPoint {
            id: format!("event_{}", i),
            lon: -180.0 + (i % 360) as f64,
            lat: -90.0 + (i / 360) as f64,
            timestamp: 1000 + i as i64,
        });
    }
    let elapsed = start.elapsed();
    println!("  Insert ({} points): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    // Range query
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = idx.query(&STQuery {
            min_lon: 0.0, min_lat: 0.0,
            max_lon: 90.0, max_lat: 90.0,
            min_time: 0, max_time: 999999,
        });
    }
    let elapsed = start.elapsed();
    println!("  Range query (1000x): {:.2}s ({:.0}/sec)", elapsed.as_secs_f64(), 1000.0 / elapsed.as_secs_f64());

    // Radius query
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = idx.radius_query(50.0, 50.0, 1000.0, 0, 999999);
    }
    let elapsed = start.elapsed();
    println!("  Radius query (1000x): {:.2}s ({:.0}/sec)", elapsed.as_secs_f64(), 1000.0 / elapsed.as_secs_f64());

    println!();
}

// ── STTRL Rules Benchmark ──

fn bench_sttrl() {
    println!("─── 5. STTRL Rule Engine ───");

    let mut engine = RuleEngine::new();

    // Add rules
    for i in 0..100 {
        engine.add_rule(Rule::new(
            format!("geofence_{}", i),
            RuleType::GeofenceEnter {
                region: Region::Circle {
                    center_lon: 116.0 + i as f64 * 0.01,
                    center_lat: 39.0 + i as f64 * 0.01,
                    radius_m: 1000.0,
                },
            },
        ));
    }

    // Evaluate
    let n = 10_000;
    let start = Instant::now();
    for i in 0..n {
        let event = SpatioTemporalEvent {
            entity_id: format!("vehicle_{}", i % 100),
            lon: 116.0 + (i % 100) as f64 * 0.01,
            lat: 39.0 + (i / 100) as f64 * 0.01,
            timestamp: 1000 + i as i64,
            properties: HashMap::new(),
        };
        let _ = engine.evaluate(&event);
    }
    let elapsed = start.elapsed();
    println!("  Evaluate 100 rules ({} events): {:.2}s ({:.0} events/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    println!();
}

// ── Time Series Benchmark ──

fn bench_timeseries() {
    println!("─── 6. Time Series (DTW + Anomaly Detection) ───");

    let n = 10_000;
    let seq_len = 100;

    // Generate sequences
    let seq_a: Vec<f64> = (0..seq_len).map(|i| (i as f64 * 0.1).sin()).collect();
    let seq_b: Vec<f64> = (0..seq_len).map(|i| (i as f64 * 0.1 + 0.1).sin()).collect();

    // DTW distance
    let start = Instant::now();
    for _ in 0..n {
        let _ = time_series::dtw_distance(&seq_a, &seq_b, None);
    }
    let elapsed = start.elapsed();
    println!("  DTW distance ({}x, len={}): {:.2}s ({:.0}/sec)", n, seq_len, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    // Anomaly detection
    let values: Vec<f64> = (0..1000).map(|i| {
        if i == 500 { 100.0 } else { (i as f64 * 0.01).sin() }
    }).collect();

    let start = Instant::now();
    for _ in 0..n {
        let _ = time_series::detect_anomalies(&values, 2.0);
    }
    let elapsed = start.elapsed();
    println!("  Anomaly detection ({}x, len=1000): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    // TimeSeries aggregation
    let mut ts = TimeSeries::new("sensor");
    for i in 0..10000 {
        ts.push(DataPoint::new(Timestamp::from_secs(i), (i as f64 * 0.01).sin()));
    }

    let start = Instant::now();
    for _ in 0..n {
        let _ = ts.mean();
        let _ = ts.std_dev();
        let _ = ts.min();
        let _ = ts.max();
    }
    let elapsed = start.elapsed();
    println!("  TS aggregations ({}x, 10K points): {:.2}s ({:.0}/sec)", n, elapsed.as_secs_f64(), n as f64 / elapsed.as_secs_f64());

    println!();
}
