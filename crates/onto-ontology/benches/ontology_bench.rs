//! OntoDB Ontology Performance Benchmark (v2 — strengthened)
//!
//! Tests:
//! 1. Ontology parsing (CREATE ONTOLOGY)
//! 2. Reasoning engine — single fact & batch facts
//! 3. Class hierarchy queries (get_all_subclasses, is_subclass_of)
//! 4. Triple reasoning with large fact sets
//! 5. Diamond/mesh inheritance stress test

use onto_ontology::model::{Class, DataType, Ontology, Property, Triple};
use onto_ontology::parser::OntologyParser;
use onto_ontology::reasoner::Reasoner;
use std::time::Instant;

/// Deep tree: depth levels, each parent has `width` children.
fn build_deep_ontology(depth: usize, width: usize) -> Ontology {
    let mut onto = Ontology::new("bench_deep");
    onto.add_class(Class::new("Thing"));
    let mut prev_level = vec!["Thing".to_string()];
    for level in 0..depth {
        let mut current_level = Vec::new();
        for parent in &prev_level {
            for w in 0..width {
                let name = format!("L{}_{}_C{}", level, parent, w);
                onto.add_class(Class::new(&name).with_superclass(parent));
                current_level.push(name);
            }
        }
        prev_level = current_level;
    }
    onto
}

/// Wide: `num_classes` direct subclasses of Thing, each with `props_per_class` properties.
fn build_wide_ontology(num_classes: usize, props_per_class: usize) -> Ontology {
    let mut onto = Ontology::new("bench_wide");
    onto.add_class(Class::new("Thing"));
    for i in 0..num_classes {
        let class_name = format!("Class_{}", i);
        onto.add_class(Class::new(&class_name).with_superclass("Thing"));
        for p in 0..props_per_class {
            let prop_name = format!("prop_{}_{}", i, p);
            onto.add_property(Property::new(&prop_name, &class_name, DataType::String));
        }
    }
    onto
}

/// Diamond inheritance: Thing → A,B → C (C inherits from both A and B).
fn build_diamond_ontology(num_diamonds: usize, chain_depth: usize) -> Ontology {
    let mut onto = Ontology::new("bench_diamond");
    onto.add_class(Class::new("Thing"));
    for d in 0..num_diamonds {
        let mut prev_a = "Thing".to_string();
        let mut prev_b = "Thing".to_string();
        for level in 0..chain_depth {
            let a = format!("D{}_A{}", d, level);
            let b = format!("D{}_B{}", d, level);
            onto.add_class(Class::new(&a).with_superclass(&prev_a));
            onto.add_class(Class::new(&b).with_superclass(&prev_b));
            prev_a = a;
            prev_b = b;
        }
        // Merge: C inherits from both chain ends
        let c = format!("D{}_Merge", d);
        onto.add_class(
            Class::new(&c)
                .with_superclass(&prev_a)
                .with_superclass(&prev_b),
        );
    }
    onto
}

/// Mesh: each class has `fan_in` parents, creating many cross-links.
fn build_mesh_ontology(layers: usize, width: usize, fan_in: usize) -> Ontology {
    let mut onto = Ontology::new("bench_mesh");
    onto.add_class(Class::new("Thing"));
    let mut prev_layer: Vec<String> = (0..width).map(|i| format!("L0_{}", i)).collect();
    for name in &prev_layer {
        onto.add_class(Class::new(name).with_superclass("Thing"));
    }
    for layer in 1..layers {
        let mut current_layer = Vec::new();
        for i in 0..width {
            let name = format!("L{}_{}", layer, i);
            let parents: Vec<String> = (0..fan_in.min(prev_layer.len()))
                .map(|j| prev_layer[(i + j) % prev_layer.len()].clone())
                .collect();
            let mut class = Class::new(&name);
            for p in &parents {
                class = class.with_superclass(p);
            }
            onto.add_class(class);
            current_layer.push(name);
        }
        prev_layer = current_layer;
    }
    onto
}

fn build_realistic_ontology() -> Ontology {
    let mut onto = Ontology::new("ecommerce");

    // Class hierarchy
    onto.add_class(Class::new("Thing"));
    onto.add_class(Class::new("Entity").with_superclass("Thing"));
    onto.add_class(Class::new("Person").with_superclass("Entity"));
    onto.add_class(Class::new("Customer").with_superclass("Person"));
    onto.add_class(Class::new("VIPCustomer").with_superclass("Customer"));
    onto.add_class(Class::new("Employee").with_superclass("Person"));
    onto.add_class(Class::new("Manager").with_superclass("Employee"));
    onto.add_class(Class::new("Product").with_superclass("Entity"));
    onto.add_class(Class::new("DigitalProduct").with_superclass("Product"));
    onto.add_class(Class::new("PhysicalProduct").with_superclass("Product"));
    onto.add_class(Class::new("Order").with_superclass("Entity"));
    onto.add_class(Class::new("Review").with_superclass("Entity"));
    onto.add_class(Class::new("Category").with_superclass("Entity"));

    // Equivalent classes
    onto.add_class(Class::new("Buyer").with_equivalent_class("Customer"));
    onto.classes
        .get_mut("Customer")
        .unwrap()
        .equivalent_classes
        .push("Buyer".to_string());

    // Properties
    onto.add_property(Property::new("name", "Entity", DataType::String));
    onto.add_property(Property::new("email", "Person", DataType::String));
    onto.add_property(Property::new("price", "Product", DataType::Float64));
    onto.add_property(Property::new("quantity", "Order", DataType::Int64));
    onto.add_property(Property::new("rating", "Review", DataType::Float64));
    onto.add_property(Property::new("title", "Review", DataType::String));
    onto.add_property(
        Property::new("manages", "Manager", DataType::String).with_inverse_of("reportsTo"),
    );
    onto.add_property(
        Property::new("reportsTo", "Employee", DataType::String).with_inverse_of("manages"),
    );
    onto.add_property(Property::new("ancestor", "Person", DataType::String).transitive());
    onto.add_property(Property::new("friendOf", "Person", DataType::String).symmetric());
    onto.add_property(
        Property::new("worksUnder", "Employee", DataType::String)
            .with_subproperty_of("reportsTo"),
    );
    onto.add_property(Property::new("discount", "VIPCustomer", DataType::Float64));

    onto
}

fn fmt_duration(d: std::time::Duration) -> String {
    if d.as_micros() < 1000 {
        format!("{}µs", d.as_micros())
    } else if d.as_millis() < 1000 {
        format!("{:.1}ms", d.as_secs_f64() * 1000.0)
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

fn main() {
    println!("{}", "=".repeat(72));
    println!("  OntoDB Ontology Performance Benchmark v2");
    println!("{}", "=".repeat(72));
    println!();

    // ═══════════════════════════════════════════════════════════════
    //  1. Ontology Parsing
    // ═══════════════════════════════════════════════════════════════
    println!("─── 1. Ontology Parsing ───");

    let sql_small = r#"
        CREATE ONTOLOGY shop (
            CLASS Product SUPERCLASS Thing,
            CLASS Customer SUPERCLASS Thing,
            CLASS VIPCustomer SUPERCLASS Customer,
            PROPERTY name DOMAIN Product RANGE STRING,
            PROPERTY price DOMAIN Product RANGE FLOAT64,
            PROPERTY email DOMAIN Customer RANGE STRING,
            PROPERTY discount DOMAIN VIPCustomer RANGE FLOAT64
        );
    "#;
    let iters = 10000;
    let start = Instant::now();
    for _ in 0..iters {
        let _ = OntologyParser::parse(sql_small).unwrap();
    }
    println!("  Small (4 classes, 4 props): {:?}/parse", start.elapsed() / iters);

    let mut big_sql = String::from("CREATE ONTOLOGY big (\n");
    for i in 0..100 {
        big_sql.push_str(&format!("  CLASS Class_{},\n", i));
    }
    for i in 0..200 {
        big_sql.push_str(&format!(
            "  PROPERTY prop_{} DOMAIN Class_{} RANGE STRING,\n",
            i,
            i % 100
        ));
    }
    big_sql.push_str(");\n");

    let iters = 1000;
    let start = Instant::now();
    for _ in 0..iters {
        let _ = OntologyParser::parse(&big_sql).unwrap();
    }
    println!("  Big (100 classes, 200 props): {:?}/parse", start.elapsed() / iters);

    let mut huge_sql = String::from("CREATE ONTOLOGY huge (\n");
    for i in 0..500 {
        huge_sql.push_str(&format!("  CLASS Class_{},\n", i));
    }
    for i in 0..1000 {
        huge_sql.push_str(&format!(
            "  PROPERTY prop_{} DOMAIN Class_{} RANGE STRING,\n",
            i,
            i % 500
        ));
    }
    huge_sql.push_str(");\n");

    let iters = 200;
    let start = Instant::now();
    for _ in 0..iters {
        let _ = OntologyParser::parse(&huge_sql).unwrap();
    }
    println!("  Huge (500 classes, 1000 props): {:?}/parse", start.elapsed() / iters);

    let mut mega_sql = String::from("CREATE ONTOLOGY mega (\n");
    for i in 0..2000 {
        mega_sql.push_str(&format!("  CLASS Class_{},\n", i));
    }
    for i in 0..5000 {
        mega_sql.push_str(&format!(
            "  PROPERTY prop_{} DOMAIN Class_{} RANGE STRING,\n",
            i,
            i % 2000
        ));
    }
    mega_sql.push_str(");\n");

    let iters = 50;
    let start = Instant::now();
    for _ in 0..iters {
        let _ = OntologyParser::parse(&mega_sql).unwrap();
    }
    println!("  Mega (2000 classes, 5000 props): {:?}/parse", start.elapsed() / iters);
    println!();

    // ═══════════════════════════════════════════════════════════════
    //  2. Reasoning Engine — Single Fact
    // ═══════════════════════════════════════════════════════════════
    println!("─── 2. Reasoning Engine (single fact) ───");

    for &(depth, width) in &[(3, 3), (5, 3), (7, 2)] {
        let onto = build_deep_ontology(depth, width);
        let reasoner = Reasoner::new(onto);
        let mut leaf = "Thing".to_string();
        for level in 0..depth {
            leaf = format!("L{}_{}_C0", level, leaf);
        }
        let facts = vec![Triple::type_of("entity", &leaf)];
        let iters = 1000;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = reasoner.reason(&facts);
        }
        let result = reasoner.reason(&facts);
        println!(
            "  Tree depth={}, width={}: {} inferred, {} iters, {:?}/reason",
            depth, width, result.inferred.len(), result.iterations, start.elapsed() / iters
        );
    }

    for &n in &[100, 500, 1000, 2000] {
        let onto = build_wide_ontology(n, 1);
        let reasoner = Reasoner::new(onto);
        let facts = vec![Triple::type_of("entity", "Class_0")];
        let iters = 1000;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = reasoner.reason(&facts);
        }
        let result = reasoner.reason(&facts);
        println!(
            "  Wide {} classes: {} inferred, {} iters, {:?}/reason",
            n, result.inferred.len(), result.iterations, start.elapsed() / iters
        );
    }
    println!();

    // ═══════════════════════════════════════════════════════════════
    //  3. Reasoning Engine — Batch Facts (三元组批量推理)
    // ═══════════════════════════════════════════════════════════════
    println!("─── 3. Reasoning Engine (batch facts / 三元组批量推理) ───");

    // Realistic ontology with properties, inverse, transitive, symmetric
    {
        let onto = build_realistic_ontology();
        let reasoner = Reasoner::new(onto);

        // 10 entities
        let mut facts = Vec::new();
        for i in 0..10 {
            facts.push(Triple::type_of(&format!("emp_{}", i), "Employee"));
            facts.push(Triple::new(&format!("emp_{}", i), "reportsTo", "mgr_0"));
        }
        facts.push(Triple::type_of("mgr_0", "Manager"));
        facts.push(Triple::new("alice", "friendOf", "bob"));
        facts.push(Triple::new("alice", "ancestor", "bob"));
        facts.push(Triple::new("bob", "ancestor", "charlie"));

        let iters = 5000;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = reasoner.reason(&facts);
        }
        let result = reasoner.reason(&facts);
        println!(
            "  Realistic ontology, {} input facts: {} inferred, {} iters, {:?}/reason",
            facts.len(),
            result.inferred.len(),
            result.iterations,
            start.elapsed() / iters
        );
    }

    // Scale up: 1K, 5K, 10K entities
    for &n_entities in &[1_000, 5_000, 10_000] {
        let onto = build_wide_ontology(50, 1);
        let reasoner = Reasoner::new(onto);

        let mut facts = Vec::new();
        for i in 0..n_entities {
            let class_idx = i % 50;
            facts.push(Triple::type_of(
                &format!("entity_{}", i),
                &format!("Class_{}", class_idx),
            ));
        }

        let iters = 10;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = reasoner.reason(&facts);
        }
        let result = reasoner.reason(&facts);
        println!(
            "  50 classes, {} entities: {} inferred, {} iters, {:?}/reason",
            n_entities,
            result.inferred.len(),
            result.iterations,
            start.elapsed() / iters
        );
    }
    println!();

    // ═══════════════════════════════════════════════════════════════
    //  4. Class Hierarchy Queries
    // ═══════════════════════════════════════════════════════════════
    println!("─── 4. Class Hierarchy Queries ───");

    for &(depth, width) in &[(3, 3), (5, 3), (7, 3), (10, 2)] {
        let onto = build_deep_ontology(depth, width);
        let total = onto.classes.len();
        let iters = 1000;

        let mut leaf = "Thing".to_string();
        for level in 0..depth {
            leaf = format!("L{}_{}_C0", level, leaf);
        }

        let start = Instant::now();
        for _ in 0..iters {
            let _ = onto.get_all_subclasses("Thing");
        }
        let subs = onto.get_all_subclasses("Thing");
        println!(
            "  get_all_subclasses (depth={}, {} classes, {} subs): {:?}/query",
            depth, total, subs.len(), start.elapsed() / iters
        );

        let start = Instant::now();
        for _ in 0..iters {
            let _ = onto.is_subclass_of(&leaf, "Thing");
        }
        println!(
            "  is_subclass_of (depth={}): {:?}/query",
            depth, start.elapsed() / iters
        );

        let start = Instant::now();
        for _ in 0..iters {
            let _ = onto.get_class_properties(&leaf);
        }
        println!(
            "  get_class_properties (depth={}): {:?}/query",
            depth, start.elapsed() / iters
        );
        println!();
    }

    // Diamond & mesh stress tests
    for &(diamonds, depth) in &[(10, 5), (50, 5), (20, 10)] {
        let onto = build_diamond_ontology(diamonds, depth);
        let total = onto.classes.len();
        let iters = 1000;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = onto.get_all_subclasses("Thing");
        }
        let subs = onto.get_all_subclasses("Thing");
        println!(
            "  get_all_subclasses ({} diamonds×depth{}, {} classes, {} subs): {:?}/query",
            diamonds, depth, total, subs.len(), start.elapsed() / iters
        );
    }

    for &(layers, width, fan_in) in &[(5, 20, 3), (8, 30, 5), (10, 50, 5)] {
        let onto = build_mesh_ontology(layers, width, fan_in);
        let total = onto.classes.len();
        let iters = 1000;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = onto.get_all_subclasses("Thing");
        }
        let subs = onto.get_all_subclasses("Thing");
        println!(
            "  get_all_subclasses (mesh {}×{}×fan{}, {} classes, {} subs): {:?}/query",
            layers, width, fan_in, total, subs.len(), start.elapsed() / iters
        );
    }
    println!();

    // ═══════════════════════════════════════════════════════════════
    //  5. Reasoning with Transitive + Symmetric + Inverse
    // ═══════════════════════════════════════════════════════════════
    println!("─── 5. Property Inference (transitive/symmetric/inverse) ───");

    {
        let onto = build_realistic_ontology();
        let reasoner = Reasoner::new(onto);

        // Chain of 100 ancestors + 50 friend pairs + 50 reportsTo
        let mut facts = Vec::new();
        for i in 0..100 {
            facts.push(Triple::new(
                &format!("person_{}", i),
                "ancestor",
                &format!("person_{}", i + 1),
            ));
        }
        for i in 0..50 {
            facts.push(Triple::new(
                &format!("person_{}", i),
                "friendOf",
                &format!("person_{}", i + 200),
            ));
        }
        for i in 0..50 {
            facts.push(Triple::type_of(&format!("emp_{}", i), "Employee"));
            facts.push(Triple::new(&format!("emp_{}", i), "reportsTo", "mgr_0"));
        }
        facts.push(Triple::type_of("mgr_0", "Manager"));

        let iters = 1000;
        let start = Instant::now();
        for _ in 0..iters {
            let _ = reasoner.reason(&facts);
        }
        let result = reasoner.reason(&facts);
        println!(
            "  {} input facts (100 ancestor+50 friend+50 inverse): {} inferred, {} iters, {:?}/reason",
            facts.len(),
            result.inferred.len(),
            result.iterations,
            start.elapsed() / iters
        );

        // Longer transitive chains
        for &chain_len in &[50, 100, 200] {
            let mut facts2 = Vec::new();
            for i in 0..chain_len {
                facts2.push(Triple::new(
                    &format!("node_{}", i),
                    "ancestor",
                    &format!("node_{}", i + 1),
                ));
            }
            let iters2 = if chain_len <= 100 { 1000 } else { 200 };
            let start = Instant::now();
            for _ in 0..iters2 {
                let _ = reasoner.reason(&facts2);
            }
            let result2 = reasoner.reason(&facts2);
            println!(
                "  {}-node transitive chain: {} inferred (expected {}), {} iters, {:?}/reason",
                chain_len,
                result2.inferred.len(),
                chain_len * (chain_len - 1) / 2,
                result2.iterations,
                start.elapsed() / iters2
            );
        }
    }
    println!();

    // ═══════════════════════════════════════════════════════════════
    //  Summary
    // ═══════════════════════════════════════════════════════════════
    println!("{}", "=".repeat(72));
    println!("  Benchmark complete.");
    println!("{}", "=".repeat(72));
}
