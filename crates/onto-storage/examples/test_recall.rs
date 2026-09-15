//! Simple recall rate test for HNSW.

use onto_storage::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorEntry};
use rand::Rng;
use std::collections::HashSet;

fn main() {
    let mut rng = rand::thread_rng();
    let dim = 64;
    let n = 1000;
    let k = 10;
    let num_queries = 50;

    // Generate random vectors
    let vectors: Vec<Vec<f32>> = (0..n)
        .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
        .collect();

    // Build HNSW index
    let config = HnswConfig::new(dim, DistanceMetric::L2)
        .with_m(64)
        .with_ef_construction(800)
        .with_ef_search(500);

    let mut index = HnswIndex::new(config);
    for (i, v) in vectors.iter().enumerate() {
        index.insert(VectorEntry {
            id: format!("vec_{}", i).into_bytes(),
            vector: v.clone(),
        });
    }

    // Test queries
    let mut total_recall = 0.0;
    for qi in 0..num_queries {
        let query: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();

        // Brute force ground truth
        let mut bf_distances: Vec<(usize, f32)> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let dist: f32 = query
                    .iter()
                    .zip(v.iter())
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f32>()
                    .sqrt();
                (i, dist)
            })
            .collect();
        bf_distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let bf_ids: HashSet<usize> = bf_distances.iter().take(k).map(|(i, _)| *i).collect();

        // HNSW search
        let results = index.search(&query, k);
        let hnsw_ids: HashSet<usize> = results
            .iter()
            .map(|r| {
                let id_str = String::from_utf8_lossy(&r.entry.id);
                id_str
                    .strip_prefix("vec_")
                    .unwrap()
                    .parse::<usize>()
                    .unwrap()
            })
            .collect();

        // Calculate recall
        let hits = bf_ids.intersection(&hnsw_ids).count();
        let recall = hits as f64 / k as f64;
        total_recall += recall;

        if qi < 3 {
            println!(
                "Query {}: recall = {:.2} ({}/{} found)",
                qi, recall, hits, k
            );
            // Print some debug info
            let bf_top3: Vec<(usize, f32)> = bf_distances.iter().take(3).cloned().collect();
            println!("  BF top-3: {:?}", bf_top3);
            let hnsw_top3: Vec<(usize, f32)> = results
                .iter()
                .take(3)
                .map(|r| {
                    let id_str = String::from_utf8_lossy(&r.entry.id);
                    (
                        id_str
                            .strip_prefix("vec_")
                            .unwrap()
                            .parse::<usize>()
                            .unwrap(),
                        r.distance,
                    )
                })
                .collect();
            println!("  HNSW top-3: {:?}", hnsw_top3);
        }
    }

    let avg_recall = total_recall / num_queries as f64;
    println!(
        "\nAverage recall@{}: {:.4} ({:.1}%)",
        k,
        avg_recall,
        avg_recall * 100.0
    );
}
