//! Ontology namespace migration tool.
//!
//! Migrates existing ontologies to the new namespace-aware structure.
//! Documents are NOT migrated - they keep the original `class::id` format.
//!
//! Usage:
//!   cargo run --bin migrate-namespace -- --data-dir ./ontodb_data
//!   cargo run --bin migrate-namespace -- --data-dir ./ontodb_data --dry-run

use onto_core::Result;
use onto_ontology::{OntologyStore, Namespace, Ontology, DEFAULT_NAMESPACE};
use onto_storage::{LsmEngine, StorageOptions};
use std::sync::Arc;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    
    let mut data_dir = "./ontodb_data".to_string();
    let mut dry_run = false;
    let mut target_namespace = DEFAULT_NAMESPACE.to_string();
    
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => {
                i += 1;
                if i < args.len() {
                    data_dir = args[i].clone();
                }
            }
            "--dry-run" => {
                dry_run = true;
            }
            "--namespace" => {
                i += 1;
                if i < args.len() {
                    target_namespace = args[i].clone();
                }
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                print_help();
                return Ok(());
            }
        }
        i += 1;
    }

    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║         OntoDB Namespace Migration Tool v0.2.0           ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();
    println!("Configuration:");
    println!("  Data directory: {}", data_dir);
    println!("  Target namespace: {}", target_namespace);
    println!("  Dry run: {}", if dry_run { "YES (no changes will be made)" } else { "NO" });
    println!();
    println!("Note: Only ontologies are migrated. Documents keep original format.");
    println!();

    let options = StorageOptions {
        data_dir: PathBuf::from(&data_dir),
        ..Default::default()
    };

    let engine = Arc::new(LsmEngine::open(options)?);
    let store = OntologyStore::new(engine.clone());

    // Step 1: Create target namespace
    println!("Step 1: Creating target namespace '{}'...", target_namespace);
    if !dry_run {
        let ns = Namespace::new(&target_namespace);
        store.save_namespace(&ns)?;
        println!("  ✓ Created namespace '{}'", target_namespace);
    } else {
        println!("  [DRY RUN] Would create namespace '{}'", target_namespace);
    }

    // Step 2: Scan and migrate ontologies
    println!("\nStep 2: Scanning existing ontologies...");
    let prefix = b"__ontology__";
    let entries = engine.scan_prefix(prefix)?;

    let mut migrated = 0;
    let mut skipped = 0;
    let mut errors = 0;

    for (key, val_bytes) in &entries {
        let key_str = String::from_utf8_lossy(key);
        
        // Skip namespaced ontologies (they have :: in the key after prefix)
        let ontology_key = &key_str[prefix.len()..];
        if ontology_key.contains("::") {
            println!("  ⊘ Skipping already namespaced: {}", ontology_key);
            skipped += 1;
            continue;
        }

        // Parse the ontology
        match Ontology::from_json_slice(val_bytes) {
            Ok(mut ontology) => {
                // Skip if already has namespace
                if ontology.namespace.is_some() {
                    println!("  ⊘ Skipping '{}' (already has namespace)", ontology.name);
                    skipped += 1;
                    continue;
                }

                let new_key = format!("__ontology__{}::{}", target_namespace, ontology.name);
                println!("  → Migrating '{}' -> '{}'", ontology.name, new_key);
                
                if !dry_run {
                    // Delete old key
                    engine.delete(key.clone())?;
                    
                    // Add namespace
                    ontology.namespace = Some(target_namespace.clone());
                    
                    // Save with new key
                    store.save_with_engine(&engine, &ontology)?;
                }
                
                migrated += 1;
            }
            Err(e) => {
                eprintln!("  ✗ Error parsing ontology: {}", e);
                errors += 1;
            }
        }
    }

    // Summary
    println!("\n╔════════════════════════════════════════════════════════════╗");
    println!("║                    Migration Summary                      ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();
    println!("  Ontologies migrated:  {:>6}", migrated);
    println!("  Ontologies skipped:   {:>6}", skipped);
    println!("  Ontologies errors:    {:>6}", errors);
    println!();
    println!("  Target namespace: '{}'", target_namespace);
    println!("  Documents: NOT migrated (keep original format)");
    println!();
    
    if dry_run {
        println!("  ⚠️  DRY RUN - No changes were made");
        println!("  Run without --dry-run to apply migration");
    } else {
        println!("  ✓ Migration complete!");
    }
    println!();

    Ok(())
}

fn print_help() {
    println!("OntoDB Namespace Migration Tool");
    println!();
    println!("USAGE:");
    println!("  cargo run --bin migrate-namespace [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("  --data-dir <DIR>      Data directory (default: ./ontodb_data)");
    println!("  --namespace <NAME>    Target namespace (default: _default)");
    println!("  --dry-run             Show what would be migrated without making changes");
    println!("  -h, --help            Show this help message");
    println!();
    println!("EXAMPLES:");
    println!("  # Preview migration");
    println!("  cargo run --bin migrate-namespace -- --data-dir ./data --dry-run");
    println!();
    println!("  # Apply migration with default namespace");
    println!("  cargo run --bin migrate-namespace -- --data-dir ./data");
    println!();
    println!("  # Apply migration with custom namespace");
    println!("  cargo run --bin migrate-namespace -- --data-dir ./data --namespace myproject");
    println!();
    println!("NOTE:");
    println!("  Only ontologies are migrated. Documents keep the original class::id format.");
}
