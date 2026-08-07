//! Graph-enhanced vector recall test.
//!
//! Tests whether using graph relationships to expand vector search results
//! can improve recall rate.

use onto_graph::{GraphStore, Vertex, Edge, PropValue, TraversalEngine, Direction};
use onto_storage::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorEntry};
use rand::Rng;
use std::collections::HashSet;
use std::time::{Duration, Instant};

fn distance_l2(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum::<f32>().sqrt()
}

fn main() {
    let dim = 64;
    let n = 5000;
    let k = 10;
    let avg_degree = 10;
    let num_queries = 30;

    println!("{}", "=".repeat(72));
    println!("  Graph-Enhanced Vector Recall Test");
    println!("  n={}, dim={}, k={}, avg_degree={}", n, dim, k, avg_degree);
    println!("{}", "=".repeat(72));
    println!();

    // Generate vectors
    let mut rng = rand::thread_rng();
    let vectors: Vec<Vec<f32>> = (0..n)
        .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
        .collect();

    // Build graph where connected vertices have similar vectors
    let store = GraphStore::new();
    for i in 0..n {
        store.add_vertex(Vertex::new(format!("v{}", i), vec!["Item".to_string()])).unwrap();
    }

    // Connect each vertex to its nearest neighbors in vector space
    for i in 0..n {
        let mut distances: Vec<(usize, f32)> = (0..n)
            .filter(|&j| j != i)
            .map(|j| (j, distance_l2(&vectors[i], &vectors[j])))
            .collect();
        distances.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

        let num_edges = rng.gen_range(1..=avg_degree * 2).min(distances.len());
        for j in 0..num_edges {
            let (target, _) = distances[j];
            store.add_edge(Edge::new(
                format!("e{}_{}", i, target),
                format!("v{}", i),
                format!("v{}", target),
                "SIMILAR"
            )).unwrap();
        }
    }
    println!("Graph: {} vertices, {} edges", store.vertex_count(), store.edge_count());

    // Build HNSW index with LOWER ef_search to simulate imperfect recall
    println!("Building HNSW index (low ef_search for imperfect recall)...");
    let config = HnswConfig::new(dim, DistanceMetric::L2)
        .with_m(16).with_ef_construction(200).with_ef_search(50);  // Lower ef_search
    let mut index = HnswIndex::new(config);
    for (i, v) in vectors.iter().enumerate() {
        index.insert(VectorEntry {
            id: format!("v{}", i).into_bytes(),
            vector: v.clone(),
        });
    }

    let engine = TraversalEngine::new(&store);

    // Generate queries and ground truth
    let mut queries = Vec::new();
    for _ in 0..num_queries {
        let query: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
        let mut bf: Vec<(usize, f32)> = vectors.iter().enumerate()
            .map(|(i, v)| (i, distance_l2(&query, v)))
            .collect();
        bf.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let bf_ids: HashSet<usize> = bf.iter().take(k).map(|(i, _)| *i).collect();
        queries.push((query, bf_ids));
    }

    // Test 1: Pure vector search
    println!("\n--- Pure Vector Search ---");
    let mut pure_recall = 0.0;
    let mut pure_time = Duration::ZERO;

    for (query, bf_ids) in &queries {
        let start = Instant::now();
        let results = index.search(query, k);
        pure_time += start.elapsed();

        let hnsw_ids: HashSet<usize> = results.iter()
            .map(|r| String::from_utf8_lossy(&r.entry.id).strip_prefix("v").unwrap().parse::<usize>().unwrap())
            .collect();

        let hits = bf_ids.intersection(&hnsw_ids).count();
        pure_recall += hits as f64 / k as f64;
    }
    let pure_recall_pct = pure_recall / num_queries as f64 * 100.0;
    println!("Recall@{}: {:.2}%", k, pure_recall_pct);
    println!("Avg latency: {:?}", pure_time / num_queries as u32);

    // Test 2: Graph-enhanced search (1 hop expansion)
    println!("\n--- Graph-Enhanced (1 hop) ---");
    let mut graph1_recall = 0.0;
    let mut graph1_time = Duration::ZERO;

    for (query, bf_ids) in &queries {
        let start = Instant::now();

        // Step 1: HNSW search with larger k
        let initial = index.search(query, k * 3);

        // Step 2: Expand via graph
        let mut candidate_ids: HashSet<Vec<u8>> = initial.iter().map(|r| r.entry.id.clone()).collect();
        for result in &initial {
            let vid = String::from_utf8_lossy(&result.entry.id).to_string();
            if let Ok(neighbors) = engine.hop(&vid, Direction::Out, Some("SIMILAR"), None, None) {
                for nbr in neighbors {
                    candidate_ids.insert(nbr.id.as_bytes().to_vec());
                }
            }
        }

        // Step 3: Filtered search
        let results = index.search_filtered(query, k, &candidate_ids);
        graph1_time += start.elapsed();

        let hnsw_ids: HashSet<usize> = results.iter()
            .map(|r| String::from_utf8_lossy(&r.entry.id).strip_prefix("v").unwrap().parse::<usize>().unwrap())
            .collect();

        let hits = bf_ids.intersection(&hnsw_ids).count();
        graph1_recall += hits as f64 / k as f64;
    }
    let graph1_recall_pct = graph1_recall / num_queries as f64 * 100.0;
    println!("Recall@{}: {:.2}%", k, graph1_recall_pct);
    println!("Avg latency: {:?}", graph1_time / num_queries as u32);

    // Test 3: Graph-enhanced search (2 hops)
    println!("\n--- Graph-Enhanced (2 hops) ---");
    let mut graph2_recall = 0.0;
    let mut graph2_time = Duration::ZERO;

    for (query, bf_ids) in &queries {
        let start = Instant::now();

        // Step 1: HNSW search
        let initial = index.search(query, k * 2);

        // Step 2: Expand via graph (2 hops)
        let mut candidate_ids: HashSet<Vec<u8>> = initial.iter().map(|r| r.entry.id.clone()).collect();

        // First hop
        let mut hop1_ids: Vec<String> = Vec::new();
        for result in &initial {
            let vid = String::from_utf8_lossy(&result.entry.id).to_string();
            if let Ok(neighbors) = engine.hop(&vid, Direction::Out, Some("SIMILAR"), None, None) {
                for nbr in neighbors {
                    let nid = nbr.id.clone();
                    candidate_ids.insert(nid.as_bytes().to_vec());
                    hop1_ids.push(nid);
                }
            }
        }

        // Second hop
        for vid in &hop1_ids {
            if let Ok(neighbors) = engine.hop(vid, Direction::Out, Some("SIMILAR"), None, None) {
                for nbr in neighbors {
                    candidate_ids.insert(nbr.id.as_bytes().to_vec());
                }
            }
        }

        // Step 3: Filtered search
        let results = index.search_filtered(query, k, &candidate_ids);
        graph2_time += start.elapsed();

        let hnsw_ids: HashSet<usize> = results.iter()
            .map(|r| String::from_utf8_lossy(&r.entry.id).strip_prefix("v").unwrap().parse::<usize>().unwrap())
            .collect();

        let hits = bf_ids.intersection(&hnsw_ids).count();
        graph2_recall += hits as f64 / k as f64;
    }
    let graph2_recall_pct = graph2_recall / num_queries as f64 * 100.0;
    println!("Recall@{}: {:.2}%", k, graph2_recall_pct);
    println!("Avg latency: {:?}", graph2_time / num_queries as u32);

    // Summary
    println!("\n{}", "=".repeat(72));
    println!("  Summary");
    println!("{}", "=".repeat(72));
    println!("  Pure Vector:       {:.2}%", pure_recall_pct);
    println!("  Graph+Vector 1hop: {:.2}%", graph1_recall_pct);
    println!("  Graph+Vector 2hop: {:.2}%", graph2_recall_pct);
    println!();
    if graph1_recall_pct > pure_recall_pct {
        println!("  >> 1-hop graph expansion improved recall by {:.2}%", graph1_recall_pct - pure_recall_pct);
    }
    if graph2_recall_pct > pure_recall_pct {
        println!("  >> 2-hop graph expansion improved recall by {:.2}%", graph2_recall_pct - pure_recall_pct);
    }
}
