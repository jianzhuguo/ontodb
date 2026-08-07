//! Benchmark: HNSW vector search recall rate.
//!
//! Measures how many true nearest neighbors the HNSW index finds
//! compared to brute-force search.

use onto_storage::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorEntry};
use rand::Rng;
use std::collections::HashSet;

/// Brute-force k-nearest-neighbor search (ground truth).
fn brute_force_search(
    vectors: &[Vec<f32>],
    query: &[f32],
    k: usize,
    metric: DistanceMetric,
) -> Vec<(usize, f32)> {
    let mut distances: Vec<(usize, f32)> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| (i, onto_storage::vector::distance::distance(query, v, metric)))
        .collect();
    distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    distances.truncate(k);
    distances
}

/// Calculate recall@k: fraction of true top-k neighbors found by HNSW.
fn recall_at_k(
    hnsw_results: &[Vec<u8>],  // HNSW result IDs
    brute_force_ids: &[usize], // Brute-force result indices
    id_to_index: &[Vec<u8>],   // Map from index to ID
) -> f64 {
    let hnsw_ids: HashSet<&Vec<u8>> = hnsw_results.iter().collect();
    let mut hits = 0;
    for &bf_idx in brute_force_ids {
        if hnsw_ids.contains(&id_to_index[bf_idx]) {
            hits += 1;
        }
    }
    hits as f64 / brute_force_ids.len() as f64
}

fn main() {
    let mut rng = rand::thread_rng();

    // Test configurations
    let configs = [
        (1000, 64, 10, "1K vectors, 64D"),
        (5000, 128, 10, "5K vectors, 128D"),
        (10000, 256, 10, "10K vectors, 256D"),
    ];

    let ef_search_values = [200, 400, 800, 1600];
    let num_queries = 30;
    let k = 10;

    println!("{}", "=".repeat(72));
    println!("  OntoDB HNSW Vector Search Recall Rate Benchmark");
    println!("{}", "=".repeat(72));
    println!();

    for &(n_vectors, dim, _, label) in &configs {
        println!("--- {} ---", label);
        println!();

        // Generate random vectors
        let vectors: Vec<Vec<f32>> = (0..n_vectors)
            .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
            .collect();

        // Generate query vectors
        let queries: Vec<Vec<f32>> = (0..num_queries)
            .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
            .collect();

        // Compute brute-force ground truth
        let ground_truth: Vec<Vec<(usize, f32)>> = queries
            .iter()
            .map(|q| brute_force_search(&vectors, q, k, DistanceMetric::L2))
            .collect();

        // Create ID map
        let id_map: Vec<Vec<u8>> = (0..n_vectors)
            .map(|i| format!("vec_{}", i).into_bytes())
            .collect();

        for &ef_search in &ef_search_values {
            // Build HNSW index - use higher params for higher dimensions
            let m = if dim >= 256 { 64 } else { 32 };
            let ef_construction = if dim >= 256 { 1000 } else { 500 };
            let config = HnswConfig::new(dim, DistanceMetric::L2)
                .with_m(m)
                .with_ef_construction(ef_construction)
                .with_ef_search(ef_search);

            let mut index = HnswIndex::new(config);
            let entries: Vec<VectorEntry> = vectors
                .iter()
                .enumerate()
                .map(|(i, v)| VectorEntry {
                    id: format!("vec_{}", i).into_bytes(),
                    vector: v.clone(),
                })
                .collect();
            index.insert_batch(entries);

            // Search and measure recall
            let mut total_recall = 0.0;
            let mut total_latency = std::time::Duration::ZERO;

            for (qi, query) in queries.iter().enumerate() {
                let start = std::time::Instant::now();
                let results = index.search(query, k);
                total_latency += start.elapsed();

                let hnsw_ids: Vec<Vec<u8>> = results.iter().map(|r| r.entry.id.clone()).collect();
                let bf_indices: Vec<usize> = ground_truth[qi].iter().map(|&(i, _)| i).collect();

                total_recall += recall_at_k(&hnsw_ids, &bf_indices, &id_map);
            }

            let avg_recall = total_recall / num_queries as f64;
            let avg_latency = total_latency / num_queries as u32;

            println!(
                "  ef_search={:>3}: recall@{} = {:.4}  ({:.1}%)  avg_latency = {:?}",
                ef_search, k, avg_recall, avg_recall * 100.0, avg_latency
            );
        }
        println!();
    }

    // Test with different metrics
    println!("--- Metric Comparison (5K vectors, 128D) ---");
    println!();

    let n_vectors = 5000;
    let dim = 128;
    let vectors: Vec<Vec<f32>> = (0..n_vectors)
        .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
        .collect();
    let queries: Vec<Vec<f32>> = (0..num_queries)
        .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
        .collect();

    let id_map: Vec<Vec<u8>> = (0..n_vectors)
        .map(|i| format!("vec_{}", i).into_bytes())
        .collect();

    for (metric_name, metric) in &[("L2", DistanceMetric::L2), ("Cosine", DistanceMetric::Cosine)] {
        let ground_truth: Vec<Vec<(usize, f32)>> = queries
            .iter()
            .map(|q| brute_force_search(&vectors, q, k, *metric))
            .collect();

        let config = HnswConfig::new(dim, *metric)
            .with_m(32)
            .with_ef_construction(500)
            .with_ef_search(400);

        let mut index = HnswIndex::new(config);
        let entries: Vec<VectorEntry> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| VectorEntry {
                id: format!("vec_{}", i).into_bytes(),
                vector: v.clone(),
            })
            .collect();
        index.insert_batch(entries);

        let mut total_recall = 0.0;
        let mut total_latency = std::time::Duration::ZERO;

        for (qi, query) in queries.iter().enumerate() {
            let start = std::time::Instant::now();
            let results = index.search(query, k);
            total_latency += start.elapsed();

            let hnsw_ids: Vec<Vec<u8>> = results.iter().map(|r| r.entry.id.clone()).collect();
            let bf_indices: Vec<usize> = ground_truth[qi].iter().map(|&(i, _)| i).collect();

            total_recall += recall_at_k(&hnsw_ids, &bf_indices, &id_map);
        }

        let avg_recall = total_recall / num_queries as f64;
        let avg_latency = total_latency / num_queries as u32;

        println!(
            "  {}: recall@{} = {:.4}  ({:.1}%)  avg_latency = {:?}",
            metric_name, k, avg_recall, avg_recall * 100.0, avg_latency
        );
    }

    println!();
    println!("{}", "=".repeat(72));
    println!("  Benchmark complete.");
    println!("{}", "=".repeat(72));
}
