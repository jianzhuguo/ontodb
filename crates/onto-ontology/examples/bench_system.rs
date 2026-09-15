//! OntoDB Full System Benchmark
//!
//! Run with: cargo run --release --example bench_system --package onto-ontology
//!
//! Tests: Storage, Reasoning, Graph, Query parsing

use onto_ontology::model::*;
use onto_ontology::Reasoner;
use std::time::Instant;

fn bench<F: Fn() -> R, R>(name: &str, iterations: u32, f: F) -> f64 {
    // Warmup
    for _ in 0..3 {
        let _ = f();
    }
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = f();
    }
    let elapsed = start.elapsed();
    let per_iter_us = elapsed.as_micros() as f64 / iterations as f64;
    println!(
        "  {:<50} {:>10.1} µs   ({:>5} iters)",
        name, per_iter_us, iterations
    );
    per_iter_us
}

fn build_ontology(depth: usize, width: usize) -> Ontology {
    let mut onto = Ontology::new("bench");
    onto.add_class(Class::new("Entity"));
    onto.add_property(Property::new("name", "Entity", DataType::String));
    onto.add_property(Property::new("value", "Entity", DataType::Float64));
    onto.add_property(Property::new("ancestor", "Entity", DataType::String).transitive());
    onto.add_property(Property::new("manages", "Entity", DataType::String));
    onto.add_property(
        Property::new("managed_by", "Entity", DataType::String).with_inverse_of("manages"),
    );
    onto.add_property(Property::new("colleague", "Entity", DataType::String).symmetric());
    onto.add_property(Property::new("email", "Entity", DataType::String).functional());

    let mut prev = vec!["Entity".to_string()];
    for level in 1..=depth {
        let mut curr = Vec::new();
        for i in 0..width {
            let name = format!("L{}_C{}", level, i);
            let parent = &prev[i % prev.len()];
            onto.add_class(Class::new(&name).with_superclass(parent));
            onto.add_property(Property::new(
                format!("p_{}", name),
                &name,
                DataType::String,
            ));
            curr.push(name);
        }
        prev = curr;
    }
    onto
}

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║          OntoDB Full System Benchmark (release mode)        ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let mut results = Vec::new();

    // ── 1. Ontology Construction ──
    println!("┌─────────────────────────────────────────────────────────────┐");
    println!("│ [1/6] Ontology Construction                                 │");
    println!("└─────────────────────────────────────────────────────────────┘");
    results.push((
        "ontology_small",
        bench("Small ontology (3x5=15 classes)", 2000, || {
            build_ontology(3, 5)
        }),
    ));
    results.push((
        "ontology_medium",
        bench("Medium ontology (5x10=~50 classes)", 500, || {
            build_ontology(5, 10)
        }),
    ));
    results.push((
        "ontology_large",
        bench("Large ontology (8x10=~80 classes)", 200, || {
            build_ontology(8, 10)
        }),
    ));

    // ── 2. Ontology Validation ──
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ [2/6] Ontology Validation (cycle detection)                 │");
    println!("└─────────────────────────────────────────────────────────────┘");
    let onto_small = build_ontology(3, 5);
    let onto_medium = build_ontology(5, 10);
    let onto_large = build_ontology(8, 10);
    results.push((
        "validate_small",
        bench("Validate 15-class ontology", 5000, || {
            let _ = onto_small.validate();
        }),
    ));
    results.push((
        "validate_medium",
        bench("Validate 50-class ontology", 2000, || {
            let _ = onto_medium.validate();
        }),
    ));
    results.push((
        "validate_large",
        bench("Validate 80-class ontology", 500, || {
            let _ = onto_large.validate();
        }),
    ));

    // ── 3. Subclass Propagation ──
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ [3/6] Subclass Propagation (Cax-sco)                        │");
    println!("└─────────────────────────────────────────────────────────────┘");
    let r_small = Reasoner::new(build_ontology(3, 5));
    let r_medium = Reasoner::new(build_ontology(5, 10));
    let r_large = Reasoner::new(build_ontology(8, 10));

    let f50: Vec<_> = (0..50)
        .map(|i| Triple::type_of(&format!("e{}", i), "L3_C0"))
        .collect();
    let f200: Vec<_> = (0..200)
        .map(|i| Triple::type_of(&format!("e{}", i), "L5_C0"))
        .collect();
    let f500: Vec<_> = (0..500)
        .map(|i| Triple::type_of(&format!("e{}", i), "L8_C0"))
        .collect();

    results.push((
        "subclass_50",
        bench("15 classes, 50 facts", 1000, || {
            let _ = r_small.reason(&f50);
        }),
    ));
    results.push((
        "subclass_200",
        bench("~50 classes, 200 facts", 300, || {
            let _ = r_medium.reason(&f200);
        }),
    ));
    results.push((
        "subclass_500",
        bench("~80 classes, 500 facts", 100, || {
            let _ = r_large.reason(&f500);
        }),
    ));

    // ── 4. Transitive Closure ──
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ [4/6] Transitive Closure (Prp-trp)                          │");
    println!("└─────────────────────────────────────────────────────────────┘");
    let r = Reasoner::new(build_ontology(3, 5));
    let chain = |n: usize| -> Vec<Triple> {
        (0..n)
            .map(|i| Triple::new(&format!("n{}", i), "ancestor", &format!("n{}", i + 1)))
            .collect()
    };
    results.push((
        "trp_5",
        bench("Chain of 5", 5000, || {
            let _ = r.reason(&chain(5));
        }),
    ));
    results.push((
        "trp_10",
        bench("Chain of 10", 2000, || {
            let _ = r.reason(&chain(10));
        }),
    ));
    results.push((
        "trp_20",
        bench("Chain of 20", 500, || {
            let _ = r.reason(&chain(20));
        }),
    ));
    results.push((
        "trp_50",
        bench("Chain of 50", 100, || {
            let _ = r.reason(&chain(50));
        }),
    ));

    // ── 5. Inverse + Symmetric ──
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ [5/6] Inverse Property (Prp-inv) + Symmetric (Prp-symp)     │");
    println!("└─────────────────────────────────────────────────────────────┘");
    let r = Reasoner::new(build_ontology(3, 5));
    let inv10: Vec<_> = (0..10)
        .map(|i| Triple::new(&format!("e{}", i), "manages", &format!("m{}", i % 3)))
        .collect();
    let inv50: Vec<_> = (0..50)
        .map(|i| Triple::new(&format!("e{}", i), "manages", &format!("m{}", i % 5)))
        .collect();
    let symp10: Vec<_> = (0..10)
        .map(|i| {
            Triple::new(
                &format!("p{}", i),
                "colleague",
                &format!("p{}", (i + 1) % 10),
            )
        })
        .collect();
    let symp50: Vec<_> = (0..50)
        .map(|i| {
            Triple::new(
                &format!("p{}", i),
                "colleague",
                &format!("p{}", (i + 1) % 50),
            )
        })
        .collect();

    results.push((
        "inv_10",
        bench("Inverse 10 facts", 5000, || {
            let _ = r.reason(&inv10);
        }),
    ));
    results.push((
        "inv_50",
        bench("Inverse 50 facts", 2000, || {
            let _ = r.reason(&inv50);
        }),
    ));
    results.push((
        "symp_10",
        bench("Symmetric 10 facts", 5000, || {
            let _ = r.reason(&symp10);
        }),
    ));
    results.push((
        "symp_50",
        bench("Symmetric 50 facts", 2000, || {
            let _ = r.reason(&symp50);
        }),
    ));

    // ── 6. Mixed + Explain ──
    println!("\n┌─────────────────────────────────────────────────────────────┐");
    println!("│ [6/6] Mixed Reasoning + EXPLAIN REASONING                   │");
    println!("└─────────────────────────────────────────────────────────────┘");
    let r = Reasoner::new(build_ontology(5, 10));
    let mut mixed: Vec<_> = (0..30)
        .map(|i| Triple::type_of(&format!("e{}", i), "L5_C0"))
        .collect();
    mixed.extend(
        (0..10).map(|i| Triple::new(&format!("e{}", i), "manages", &format!("e{}", i + 10))),
    );
    mixed.extend(
        (0..5).map(|i| Triple::new(&format!("e{}", i), "ancestor", &format!("e{}", i + 1))),
    );
    mixed.extend(
        (0..5).map(|i| Triple::new(&format!("e{}", i), "colleague", &format!("e{}", i + 5))),
    );

    results.push((
        "mixed",
        bench("Mixed all-7-rules, 50 facts", 500, || {
            let _ = r.reason(&mixed);
        }),
    ));

    let r = Reasoner::new(build_ontology(3, 5));
    let facts = vec![Triple::type_of("alice", "L3_C0")];
    let target = Triple::type_of("alice", "Entity");
    results.push((
        "explain",
        bench("EXPLAIN subclass derivation", 10000, || {
            let _ = r.explain(&facts, &target);
        }),
    ));

    // ── Summary ──
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║                    Benchmark Summary                         ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ {:<35} {:>12} {:>10} ║", "Test", "Latency", "Rating");
    println!("╠══════════════════════════════════════════════════════════════╣");
    for (name, us) in &results {
        let rating = if *us < 50.0 {
            "FAST"
        } else if *us < 500.0 {
            "OK"
        } else {
            "SLOW"
        };
        let label = match *name {
            "ontology_small" => "Ontology build (15 cls)",
            "ontology_medium" => "Ontology build (50 cls)",
            "ontology_large" => "Ontology build (80 cls)",
            "validate_small" => "Validate (15 cls)",
            "validate_medium" => "Validate (50 cls)",
            "validate_large" => "Validate (80 cls)",
            "subclass_50" => "Subclass 50 facts",
            "subclass_200" => "Subclass 200 facts",
            "subclass_500" => "Subclass 500 facts",
            "trp_5" => "Transitive chain=5",
            "trp_10" => "Transitive chain=10",
            "trp_20" => "Transitive chain=20",
            "trp_50" => "Transitive chain=50",
            "inv_10" => "Inverse 10 facts",
            "inv_50" => "Inverse 50 facts",
            "symp_10" => "Symmetric 10 facts",
            "symp_50" => "Symmetric 50 facts",
            "mixed" => "Mixed 7 rules",
            "explain" => "EXPLAIN REASONING",
            _ => name,
        };
        println!("║ {:<35} {:>10.1} µs {:>10} ║", label, us, rating);
    }
    println!("╚══════════════════════════════════════════════════════════════╝");
}
