//! Comprehensive Graph Performance Benchmarks
//!
//! Covers: BFS (sequential + parallel), Dijkstra, PageRank, cache operations,
//! and incremental updates.

use onto_graph::{
    analytics::{connected_components, pagerank},
    store::EdgeChange,
    Direction, Edge, GraphStore, PropValue, Vertex,
};
#[cfg(feature = "enterprise")]
use onto_graph::analytics::{betweenness_centrality, dijkstra};
use rand::Rng;
use std::time::Instant;

/// Build a social graph with configurable size and density.
fn build_social_graph(num_vertices: usize, avg_edges_per_vertex: usize) -> GraphStore {
    let store = GraphStore::new();
    let mut rng = rand::thread_rng();

    for i in 0..num_vertices {
        let labels = vec!["Person".to_string()];
        store
            .add_vertex(
                Vertex::new(format!("v{}", i), labels)
                    .with_property("name", PropValue::String(format!("User_{}", i)))
                    .with_property("age", PropValue::Int(rng.gen_range(18..70))),
            )
            .unwrap();
    }

    let mut edge_count = 0;
    for i in 0..num_vertices {
        let num_edges = rng.gen_range(1..=avg_edges_per_vertex * 2);
        for _ in 0..num_edges {
            let target = rng.gen_range(0..num_vertices);
            if target != i {
                let edge_id = format!("e{}_{}", i, edge_count);
                store
                    .add_edge(
                        Edge::new(edge_id, format!("v{}", i), format!("v{}", target), "KNOWS")
                            .with_property("weight", PropValue::Float(rng.gen_range(0.1..10.0))),
                    )
                    .unwrap();
                edge_count += 1;
            }
        }
    }

    store
}

/// Build a biomedical graph (Drug-Disease-Protein).
fn build_biomedical_graph(num_drugs: usize, num_diseases: usize, num_proteins: usize) -> GraphStore {
    let store = GraphStore::new();
    let mut rng = rand::thread_rng();

    // Add drugs
    for i in 0..num_drugs {
        store
            .add_vertex(
                Vertex::new(format!("Drug::{}", i), vec!["Drug".to_string()])
                    .with_property("name", PropValue::String(format!("Drug_{}", i))),
            )
            .unwrap();
    }

    // Add diseases
    for i in 0..num_diseases {
        store
            .add_vertex(
                Vertex::new(format!("Disease::{}", i), vec!["Disease".to_string()])
                    .with_property("name", PropValue::String(format!("Disease_{}", i))),
            )
            .unwrap();
    }

    // Add proteins
    for i in 0..num_proteins {
        store
            .add_vertex(
                Vertex::new(format!("Protein::{}", i), vec!["Protein".to_string()])
                    .with_property("name", PropValue::String(format!("Protein_{}", i))),
            )
            .unwrap();
    }

    // Add relationships
    let mut edge_id = 0;

    // Drug treats Disease
    for i in 0..num_drugs {
        let num_targets = rng.gen_range(1..=5);
        for _ in 0..num_targets {
            let disease = rng.gen_range(0..num_diseases);
            store
                .add_edge(Edge::new(
                    format!("e{}", edge_id),
                    format!("Drug::{}", i),
                    format!("Disease::{}", disease),
                    "treats",
                ))
                .unwrap();
            edge_id += 1;
        }
    }

    // Disease associated_with Protein
    for i in 0..num_diseases {
        let num_targets = rng.gen_range(1..=3);
        for _ in 0..num_targets {
            let protein = rng.gen_range(0..num_proteins);
            store
                .add_edge(Edge::new(
                    format!("e{}", edge_id),
                    format!("Disease::{}", i),
                    format!("Protein::{}", protein),
                    "associated_with",
                ))
                .unwrap();
            edge_id += 1;
        }
    }

    // Drug targets Protein
    for i in 0..num_drugs {
        let num_targets = rng.gen_range(1..=4);
        for _ in 0..num_targets {
            let protein = rng.gen_range(0..num_proteins);
            store
                .add_edge(Edge::new(
                    format!("e{}", edge_id),
                    format!("Drug::{}", i),
                    format!("Protein::{}", protein),
                    "targets",
                ))
                .unwrap();
            edge_id += 1;
        }
    }

    store
}

fn bench_sequential_bfs(store: &GraphStore, num_queries: usize, max_depth: usize) {
    let mut rng = rand::thread_rng();
    let num_vertices = store.vertex_count();

    let start = Instant::now();
    let mut total_visited = 0;
    for _ in 0..num_queries {
        let idx = rng.gen_range(0..num_vertices) as u32;
        let result = store.bfs_fast(idx, max_depth, Direction::Out);
        total_visited += result.len();
    }
    let elapsed = start.elapsed();

    println!(
        "  Sequential BFS (depth={}): {:.3}s, {:.0} qps, avg_visited={:.1}",
        max_depth,
        elapsed.as_secs_f64(),
        num_queries as f64 / elapsed.as_secs_f64(),
        total_visited as f64 / num_queries as f64
    );
}

#[cfg(feature = "enterprise")]
fn bench_parallel_bfs(store: &GraphStore, num_queries: usize, max_depth: usize) {
    let mut rng = rand::thread_rng();
    let num_vertices = store.vertex_count();

    let start = Instant::now();
    let mut total_visited = 0;
    for _ in 0..num_queries {
        let idx = rng.gen_range(0..num_vertices) as u32;
        let result = store.bfs_parallel(idx, max_depth, Direction::Out);
        total_visited += result.len();
    }
    let elapsed = start.elapsed();

    println!(
        "  Parallel BFS (depth={}): {:.3}s, {:.0} qps, avg_visited={:.1}",
        max_depth,
        elapsed.as_secs_f64(),
        num_queries as f64 / elapsed.as_secs_f64(),
        total_visited as f64 / num_queries as f64
    );
}

#[cfg(feature = "enterprise")]
fn bench_dijkstra(store: &GraphStore, num_queries: usize) {
    let mut rng = rand::thread_rng();
    let vertices: Vec<String> = (0..store.vertex_count().min(1000))
        .map(|i| format!("v{}", i))
        .collect();

    let start = Instant::now();
    let mut found_paths = 0;
    for _ in 0..num_queries {
        let src = &vertices[rng.gen_range(0..vertices.len())];
        let dst = &vertices[rng.gen_range(0..vertices.len())];
        let result = dijkstra(store, src, Some(dst), Some("weight"));
        if result.distances.get(dst).copied().unwrap_or(f64::INFINITY) < f64::INFINITY {
            found_paths += 1;
        }
    }
    let elapsed = start.elapsed();

    println!(
        "  Dijkstra: {:.3}s, {:.0} qps, paths_found={}/{}",
        elapsed.as_secs_f64(),
        num_queries as f64 / elapsed.as_secs_f64(),
        found_paths,
        num_queries
    );
}

fn bench_pagerank(store: &GraphStore, iterations: usize) {
    let start = Instant::now();
    let ranks = pagerank(store, 0.85, iterations, 1e-6);
    let elapsed = start.elapsed();

    println!(
        "  PageRank ({} iterations): {:.3}s, {} vertices",
        iterations,
        elapsed.as_secs_f64(),
        ranks.len()
    );
}

fn bench_connected_components(store: &GraphStore) {
    let start = Instant::now();
    let components = connected_components(store);
    let elapsed = start.elapsed();

    println!(
        "  Connected Components: {:.3}s, {} components",
        elapsed.as_secs_f64(),
        components.len()
    );
}

#[cfg(feature = "enterprise")]
fn bench_betweenness_centrality(store: &GraphStore, sample_size: usize) {
    // Use a smaller subgraph for betweenness (O(VE) is expensive)
    let subgraph = if store.vertex_count() > sample_size {
        let sub = GraphStore::new();
        let vertices: Vec<String> = (0..sample_size)
            .map(|i| format!("v{}", i))
            .collect();
        for v in &vertices {
            if let Some(vertex) = store.get_vertex(v) {
                let _ = sub.add_vertex(vertex);
            }
        }
        for v in &vertices {
            for edge in store.get_out_edges(v) {
                if vertices.contains(&edge.to) {
                    let _ = sub.add_edge(edge);
                }
            }
        }
        sub
    } else {
        GraphStore::new() // dummy
    };

    if subgraph.vertex_count() > 0 {
        let start = Instant::now();
        let centralities = betweenness_centrality(&subgraph);
        let elapsed = start.elapsed();

        println!(
            "  Betweenness Centrality ({} vertices): {:.3}s",
            centralities.len(),
            elapsed.as_secs_f64()
        );
    }
}

fn bench_cache_operations(store: &GraphStore, num_operations: usize) {
    // Simulate cache invalidation and reload
    let start = Instant::now();
    for i in 0..num_operations {
        store.invalidate_relation("KNOWS");
        store.mark_loaded("KNOWS", 1000 + i);
    }
    let elapsed = start.elapsed();

    println!(
        "  Cache invalidation ({} ops): {:.3}s, {:.0} ops/s",
        num_operations,
        elapsed.as_secs_f64(),
        num_operations as f64 / elapsed.as_secs_f64()
    );
}

fn bench_incremental_updates(store: &GraphStore, num_changes: usize) {
    let start = Instant::now();
    for i in 0..num_changes {
        store.record_edge_change(
            "KNOWS",
            EdgeChange::Added {
                source: format!("v{}", i % 1000),
                target: format!("v{}", (i + 1) % 1000),
                edge_id: format!("inc_e{}", i),
            },
        );
    }
    let record_time = start.elapsed();

    let start = Instant::now();
    let applied = store.apply_pending_changes("KNOWS");
    let apply_time = start.elapsed();

    println!(
        "  Incremental updates: record={:.3}s ({} changes), apply={:.3}s ({} applied)",
        record_time.as_secs_f64(),
        num_changes,
        apply_time.as_secs_f64(),
        applied
    );
}

fn main() {
    println!("=== OntoDB Graph Performance Benchmarks ===\n");

    // Small graph (1K vertices)
    println!("--- Small Graph (1K vertices, ~5K edges) ---");
    let small = build_social_graph(1000, 5);
    bench_sequential_bfs(&small, 1000, 3);
    #[cfg(feature = "enterprise")]
    bench_parallel_bfs(&small, 1000, 3);
    #[cfg(feature = "enterprise")]
    bench_dijkstra(&small, 1000);
    bench_pagerank(&small, 50);
    bench_connected_components(&small);
    bench_cache_operations(&small, 10000);
    bench_incremental_updates(&small, 1000);

    // Medium graph (10K vertices)
    println!("\n--- Medium Graph (10K vertices, ~50K edges) ---");
    let medium = build_social_graph(10000, 5);
    bench_sequential_bfs(&medium, 500, 4);
    #[cfg(feature = "enterprise")]
    bench_parallel_bfs(&medium, 500, 4);
    #[cfg(feature = "enterprise")]
    bench_dijkstra(&medium, 500);
    bench_pagerank(&medium, 50);
    bench_connected_components(&medium);
    #[cfg(feature = "enterprise")]
    bench_betweenness_centrality(&medium, 500);

    // Biomedical graph
    println!("\n--- Biomedical Graph (1K drugs, 500 diseases, 2K proteins) ---");
    let bio = build_biomedical_graph(1000, 500, 2000);
    println!(
        "  Built: {} vertices, {} edges",
        bio.vertex_count(),
        bio.edge_count()
    );
    bench_sequential_bfs(&bio, 500, 3);
    #[cfg(feature = "enterprise")]
    bench_parallel_bfs(&bio, 500, 3);
    bench_pagerank(&bio, 50);
    bench_connected_components(&bio);

    println!("\n=== Benchmarks Complete ===");
}
