//! Rollback script to reverse namespace migration.
//!
//! This script reverses the ontology migration by:
//! 1. Removing namespace from ontology keys
//! 2. Deleting namespace metadata
//!
//! Note: Documents are NOT affected - they keep original format.

use onto_core::Result;
use onto_ontology::{Ontology, OntologyStore, DEFAULT_NAMESPACE};
use onto_storage::{LsmEngine, StorageOptions};
use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<()> {
    let data_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "./ontodb_data".to_string());

    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║         OntoDB Namespace Migration Rollback              ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();
    println!("Rolling back migration in: {}", data_dir);
    println!();

    let options = StorageOptions {
        data_dir: PathBuf::from(&data_dir),
        ..Default::default()
    };

    let engine = Arc::new(LsmEngine::open(options)?);
    let store = OntologyStore::new(engine.clone());

    // Step 1: Scan and rollback ontologies
    println!("Step 1: Rolling back ontologies...");
    let prefix = b"__ontology__";
    let entries = engine.scan_prefix(prefix)?;

    let mut rolled_back = 0;
    let mut skipped = 0;

    for (key, val_bytes) in &entries {
        let key_str = String::from_utf8_lossy(key);
        let ontology_key = &key_str[prefix.len()..];

        // Only process namespaced ontologies
        if !ontology_key.contains("::") {
            println!("  ⊘ Skipping non-namespaced: {}", ontology_key);
            skipped += 1;
            continue;
        }

        // Parse the ontology
        match Ontology::from_json_slice(val_bytes) {
            Ok(mut ontology) => {
                let old_name = ontology.name.clone();
                let namespace = ontology.namespace.clone().unwrap_or_default();

                println!(
                    "  → Rolling back '{}::{}' -> '{}'",
                    namespace, old_name, old_name
                );

                // Delete namespaced key
                engine.delete(key.clone())?;

                // Remove namespace
                ontology.namespace = None;

                // Save with original key
                store.save_with_engine(&engine, &ontology)?;

                rolled_back += 1;
            }
            Err(e) => {
                eprintln!("  ✗ Error parsing ontology: {}", e);
            }
        }
    }

    // Step 2: Delete namespace metadata
    println!("\nStep 2: Removing namespace metadata...");
    match store.delete_namespace(DEFAULT_NAMESPACE) {
        Ok(_) => println!("  ✓ Deleted namespace '{}'", DEFAULT_NAMESPACE),
        Err(e) => println!("  ⊘ Namespace not found or error: {}", e),
    }

    // Summary
    println!("\n╔════════════════════════════════════════════════════════════╗");
    println!("║                    Rollback Summary                       ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();
    println!("  Ontologies rolled back:  {:>6}", rolled_back);
    println!("  Ontologies skipped:      {:>6}", skipped);
    println!();
    println!("  Documents: NOT affected (keep original format)");
    println!();
    println!("  ✓ Rollback complete!");
    println!();

    Ok(())
}
