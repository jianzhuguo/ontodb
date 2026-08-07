//! Graph Benchmark - Tests CRUD, traversal, and graph+vector hybrid query performance.

use onto_graph::{GraphStore, Vertex, Edge, PropValue, TraversalEngine, Direction};
use onto_storage::vector::{DistanceMetric, HnswConfig, HnswIndex, VectorEntry};
use rand::Rng;
use std::time::Instant;

fn build_social_graph(num_vertices: usize, avg_edges_per_vertex: usize) -> GraphStore {
    let store = GraphStore::new();
    let mut rng = rand::thread_rng();

    println!("Building graph with {} vertices...", num_vertices);

    // Add vertices
    for i in 0..num_vertices {
        let labels = if i % 10 == 0 {
            vec!["Person".to_string(), "Manager".to_string()]
        } else {
            vec!["Person".to_string()]
        };
        store.add_vertex(
            Vertex::new(format!("v{}", i), labels)
                .with_property("name", PropValue::String(format!("User_{}", i)))
                .with_property("age", PropValue::Int(rng.gen_range(18..70)))
                .with_property("score", PropValue::Float(rng.gen_range(0.0..100.0)))
        ).unwrap();
    }

    // Add edges (random social connections)
    let mut edge_count = 0;
    for i in 0..num_vertices {
        let num_edges = rng.gen_range(1..=avg_edges_per_vertex * 2);
        for _ in 0..num_edges {
            let target = rng.gen_range(0..num_vertices);
            if target != i {
                let label = if rng.gen_bool(0.7) { "KNOWS" } else { "WORKS_WITH" };
                let edge_id = format!("e{}_{}", i, edge_count);
                store.add_edge(
                    Edge::new(edge_id, format!("v{}", i), format!("v{}", target), label)
                        .with_property("weight", PropValue::Float(rng.gen_range(0.0..1.0)))
                ).unwrap();
                edge_count += 1;
            }
        }
    }

    println!("Graph built: {} vertices, {} edges", store.vertex_count(), store.edge_count());
    store
}

fn bench_vertex_crud(store: &GraphStore, n: usize) {
    let start = Instant::now();

    // Write
    for i in 0..n {
        let _ = store.add_vertex(
            Vertex::new(format!("bench_{}", i), vec!["Bench".to_string()])
                .with_property("value", PropValue::Int(i as i64))
        );
    }
    let write_time = start.elapsed();

    // Read
    let start = Instant::now();
    for i in 0..n {
        let _ = store.get_vertex(&format!("bench_{}", i));
    }
    let read_time = start.elapsed();

    println!("  Vertex CRUD: write={:.2}s ({:.0}/s), read={:.2}s ({:.0}/s)",
        write_time.as_secs_f64(), n as f64 / write_time.as_secs_f64(),
        read_time.as_secs_f64(), n as f64 / read_time.as_secs_f64());
}

fn bench_edge_crud(store: &GraphStore, n: usize) {
    // Ensure vertices exist
    for i in 0..n {
        let _ = store.add_vertex(Vertex::new(format!("e_src_{}", i), vec![]));
        let _ = store.add_vertex(Vertex::new(format!("e_dst_{}", i), vec![]));
    }

    let start = Instant::now();
    for i in 0..n {
        let _ = store.add_edge(
            Edge::new(format!("bench_e_{}", i), format!("e_src_{}", i), format!("e_dst_{}", i), "LINK")
        );
    }
    let write_time = start.elapsed();

    let start = Instant::now();
    for i in 0..n {
        let _ = store.get_edge(&format!("bench_e_{}", i));
    }
    let read_time = start.elapsed();

    println!("  Edge CRUD: write={:.2}s ({:.0}/s), read={:.2}s ({:.0}/s)",
        write_time.as_secs_f64(), n as f64 / write_time.as_secs_f64(),
        read_time.as_secs_f64(), n as f64 / read_time.as_secs_f64());
}

fn bench_traversal(store: &GraphStore, num_queries: usize, max_depth: usize) {
    let engine = TraversalEngine::new(store);
    let mut rng = rand::thread_rng();
    let num_vertices = store.vertex_count();

    // Single-hop
    let start = Instant::now();
    let mut total_neighbors = 0;
    for _ in 0..num_queries {
        let vid = format!("v{}", rng.gen_range(0..num_vertices));
        let neighbors = engine.hop(&vid, Direction::Out, Some("KNOWS"), None, None).unwrap();
        total_neighbors += neighbors.len();
    }
    let hop1_time = start.elapsed();

    // Multi-hop BFS (string-based)
    let start = Instant::now();
    let mut total_visited = 0;
    for _ in 0..num_queries {
        let vid = format!("v{}", rng.gen_range(0..num_vertices));
        let result = engine.traverse_bfs(&vid, max_depth, Direction::Out, Some("KNOWS"), None).unwrap();
        total_visited += result.vertices.len();
    }
    let bfs_time = start.elapsed();

    // Fast BFS (integer-indexed)
    let start = Instant::now();
    let mut total_fast_visited = 0;
    for _ in 0..num_queries {
        let vid = format!("v{}", rng.gen_range(0..num_vertices));
        if let Ok(result) = engine.traverse_bfs_fast(&vid, max_depth, Direction::Out) {
            total_fast_visited += result.len();
        }
    }
    let bfs_fast_time = start.elapsed();

    println!("  Single-hop: {:.2}s ({:.0} qps), avg_neighbors={:.1}",
        hop1_time.as_secs_f64(), num_queries as f64 / hop1_time.as_secs_f64(),
        total_neighbors as f64 / num_queries as f64);
    println!("  {}-hop BFS: {:.2}s ({:.0} qps), avg_visited={:.1}",
        max_depth, bfs_time.as_secs_f64(), num_queries as f64 / bfs_time.as_secs_f64(),
        total_visited as f64 / num_queries as f64);
    println!("  {}-hop BFS (fast): {:.2}s ({:.0} qps), avg_visited={:.1}",
        max_depth, bfs_fast_time.as_secs_f64(), num_queries as f64 / bfs_fast_time.as_secs_f64(),
        total_fast_visited as f64 / num_queries as f64);
}

fn bench_graph_vector_hybrid(store: &GraphStore, num_queries: usize) {
    let engine = TraversalEngine::new(store);
    let mut rng = rand::thread_rng();
    let num_vertices = store.vertex_count();

    // Build vector index for all vertices
    let dim = 64;
    let config = HnswConfig::new(dim, DistanceMetric::L2)
        .with_m(16).with_ef_construction(200).with_ef_search(100);
    let mut index = HnswIndex::new(config);

    let vectors: Vec<Vec<f32>> = (0..num_vertices)
        .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
        .collect();

    for (i, v) in vectors.iter().enumerate() {
        index.insert(VectorEntry {
            id: format!("v{}", i).into_bytes(),
            vector: v.clone(),
        });
    }

    println!("  Vector index built for {} vertices", num_vertices);

    // Hybrid query: graph traversal + vector search
    let start = Instant::now();
    for _ in 0..num_queries {
        let vid = format!("v{}", rng.gen_range(0..num_vertices));
        let query_vec: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();

        // Step 1: Graph traversal to get subgraph
        let neighbors = engine.hop(&vid, Direction::Out, Some("KNOWS"), None, None).unwrap();
        let neighbor_ids: Vec<String> = neighbors.iter().map(|v| v.id.clone()).collect();

        // Step 2: Vector search within subgraph
        let _results = index.search_filtered(&query_vec, 5, &neighbor_ids.iter().map(|id| id.as_bytes().to_vec()).collect());
    }
    let hybrid_time = start.elapsed();

    println!("  Graph+Vector hybrid: {:.2}s ({:.0} qps)",
        hybrid_time.as_secs_f64(), num_queries as f64 / hybrid_time.as_secs_f64());
}

fn main() {
    println!("{}", "=".repeat(72));
    println!("  OntoDB Graph Benchmark");
    println!("{}", "=".repeat(72));
    println!();

    // Test 1: Small graph (1K vertices)
    println!("--- Small Graph (1K vertices) ---");
    let store = build_social_graph(1000, 10);
    bench_vertex_crud(&store, 10000);
    bench_edge_crud(&store, 10000);
    bench_traversal(&store, 1000, 2);
    bench_graph_vector_hybrid(&store, 100);
    println!();

    // Test 2: Medium graph (10K vertices)
    println!("--- Medium Graph (10K vertices) ---");
    let store = build_social_graph(10000, 15);
    bench_vertex_crud(&store, 10000);
    bench_edge_crud(&store, 10000);
    bench_traversal(&store, 500, 3);
    bench_graph_vector_hybrid(&store, 50);
    println!();

    // Test 3: Large graph (100K vertices) - deep traversal tests
    println!("--- Large Graph (100K vertices) ---");
    let store = build_social_graph(100000, 20);
    bench_vertex_crud(&store, 10000);
    bench_edge_crud(&store, 10000);
    bench_traversal(&store, 100, 3);
    bench_traversal(&store, 50, 5);
    bench_traversal(&store, 20, 6);
    bench_graph_vector_hybrid(&store, 20);
    println!();

    println!("{}", "=".repeat(72));
    println!("  Benchmark complete.");
    println!("{}", "=".repeat(72));
}
