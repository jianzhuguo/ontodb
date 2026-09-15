//! Reasoning benchmark example.
//!
//! Run with: cargo run --release --example bench_reasoning

use onto_ontology::model::*;
use onto_ontology::Reasoner;

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

    let mut prev_level = vec!["Entity".to_string()];
    for level in 1..=depth {
        let mut current_level = Vec::new();
        for i in 0..width {
            let class_name = format!("L{}_C{}", level, i);
            let parent = &prev_level[i % prev_level.len()];
            onto.add_class(Class::new(&class_name).with_superclass(parent));
            onto.add_property(Property::new(
                format!("attr_{}", class_name),
                &class_name,
                DataType::String,
            ));
            current_level.push(class_name);
        }
        prev_level = current_level;
    }
    onto
}

fn bench<F: Fn()>(name: &str, iterations: u32, f: F) {
    // Warmup
    for _ in 0..3 {
        f();
    }
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed = start.elapsed();
    let per_iter_us = elapsed.as_micros() as f64 / iterations as f64;
    println!(
        "  {:<45} {:>8.1} µs/iter  ({:>5} iters, {:.1}ms total)",
        name,
        per_iter_us,
        iterations,
        elapsed.as_secs_f64() * 1000.0
    );
}

fn main() {
    println!("OntoQL Reasoning Benchmark (release mode)");
    println!("==========================================\n");

    // ── 1. Ontology construction ──
    println!("[1] Ontology Construction");
    bench("Small (3x5 = 15 classes)", 1000, || {
        let _ = build_ontology(3, 5);
    });
    bench("Medium (5x10 = ~50 classes)", 500, || {
        let _ = build_ontology(5, 10);
    });
    bench("Large (8x10 = ~80 classes)", 200, || {
        let _ = build_ontology(8, 10);
    });

    // ── 2. Reasoning: subclass propagation ──
    println!("\n[2] Subclass Propagation (Cax-sco)");
    let onto_small = build_ontology(3, 5);
    let reasoner_small = Reasoner::new(onto_small);
    let facts_50: Vec<_> = (0..50)
        .map(|i| onto_ontology::model::Triple::type_of(&format!("e{}", i), "L3_C0"))
        .collect();
    let facts_200: Vec<_> = (0..200)
        .map(|i| onto_ontology::model::Triple::type_of(&format!("e{}", i), "L3_C0"))
        .collect();

    bench("15 classes, 50 facts", 1000, || {
        let _ = reasoner_small.reason(&facts_50);
    });
    bench("15 classes, 200 facts", 500, || {
        let _ = reasoner_small.reason(&facts_200);
    });

    let onto_medium = build_ontology(5, 10);
    let reasoner_medium = Reasoner::new(onto_medium);
    let facts_100: Vec<_> = (0..100)
        .map(|i| onto_ontology::model::Triple::type_of(&format!("e{}", i), "L5_C0"))
        .collect();

    bench("~50 classes, 100 facts", 500, || {
        let _ = reasoner_medium.reason(&facts_100);
    });

    // ── 3. Transitive closure (Prp-trp) ──
    println!("\n[3] Transitive Closure (Prp-trp)");
    let onto = build_ontology(3, 5);
    let reasoner = Reasoner::new(onto);

    let chain_5: Vec<_> = (0..5)
        .map(|i| {
            onto_ontology::model::Triple::new(
                &format!("n{}", i),
                "ancestor",
                &format!("n{}", i + 1),
            )
        })
        .collect();
    let chain_10: Vec<_> = (0..10)
        .map(|i| {
            onto_ontology::model::Triple::new(
                &format!("n{}", i),
                "ancestor",
                &format!("n{}", i + 1),
            )
        })
        .collect();
    let chain_20: Vec<_> = (0..20)
        .map(|i| {
            onto_ontology::model::Triple::new(
                &format!("n{}", i),
                "ancestor",
                &format!("n{}", i + 1),
            )
        })
        .collect();

    bench("Chain of 5", 5000, || {
        let _ = reasoner.reason(&chain_5);
    });
    bench("Chain of 10", 2000, || {
        let _ = reasoner.reason(&chain_10);
    });
    bench("Chain of 20", 500, || {
        let _ = reasoner.reason(&chain_20);
    });

    // ── 4. Inverse property (Prp-inv) ──
    println!("\n[4] Inverse Property (Prp-inv)");
    let onto = build_ontology(3, 5);
    let reasoner = Reasoner::new(onto);

    let inv_10: Vec<_> = (0..10)
        .map(|i| {
            onto_ontology::model::Triple::new(
                &format!("emp{}", i),
                "manages",
                &format!("mgr{}", i % 3),
            )
        })
        .collect();
    let inv_50: Vec<_> = (0..50)
        .map(|i| {
            onto_ontology::model::Triple::new(
                &format!("emp{}", i),
                "manages",
                &format!("mgr{}", i % 5),
            )
        })
        .collect();

    bench("10 facts", 5000, || {
        let _ = reasoner.reason(&inv_10);
    });
    bench("50 facts", 2000, || {
        let _ = reasoner.reason(&inv_50);
    });

    // ── 5. Symmetric property (Prp-symp) ──
    println!("\n[5] Symmetric Property (Prp-symp)");
    let onto = build_ontology(3, 5);
    let reasoner = Reasoner::new(onto);

    let symp_10: Vec<_> = (0..10)
        .map(|i| {
            onto_ontology::model::Triple::new(
                &format!("p{}", i),
                "colleague",
                &format!("p{}", (i + 1) % 10),
            )
        })
        .collect();

    bench("10 facts", 5000, || {
        let _ = reasoner.reason(&symp_10);
    });

    // ── 6. Mixed reasoning (all rules) ──
    println!("\n[6] Mixed Reasoning (all 7 rules)");
    let onto = build_ontology(5, 10);
    let reasoner = Reasoner::new(onto);

    let mut mixed_facts: Vec<_> = (0..30)
        .map(|i| onto_ontology::model::Triple::type_of(&format!("e{}", i), "L5_C0"))
        .collect();
    mixed_facts.extend((0..10).map(|i| {
        onto_ontology::model::Triple::new(&format!("e{}", i), "manages", &format!("e{}", i + 10))
    }));
    mixed_facts.extend((0..5).map(|i| {
        onto_ontology::model::Triple::new(&format!("e{}", i), "ancestor", &format!("e{}", i + 1))
    }));
    mixed_facts.extend((0..5).map(|i| {
        onto_ontology::model::Triple::new(&format!("e{}", i), "colleague", &format!("e{}", i + 5))
    }));

    bench("~50 classes, 50 mixed facts", 500, || {
        let result = reasoner.reason(&mixed_facts);
        std::hint::black_box(&result);
    });

    // ── 7. Explain (derivation trace) ──
    println!("\n[7] Explain (derivation trace)");
    let onto = build_ontology(3, 5);
    let reasoner = Reasoner::new(onto);
    let facts = vec![onto_ontology::model::Triple::type_of("alice", "L3_C0")];
    let target = onto_ontology::model::Triple::type_of("alice", "Entity");

    bench("Explain subclass derivation", 5000, || {
        let _ = reasoner.explain(&facts, &target);
    });

    println!("\n==========================================");
    println!("Benchmark complete.");
}
