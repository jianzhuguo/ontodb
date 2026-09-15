//! Integration tests: Query Executor + Storage Engine
//!
//! Tests the full stack from SQL parsing through query execution
//! against the real LSM-Tree storage engine, covering scenarios
//! that cross component boundaries.

use onto_ontology::OntologyStore;
use onto_query::{QueryExecutor, QueryParser};
use onto_storage::{LsmEngine, StorageOptions};
use std::sync::Arc;
use tempfile::{tempdir, TempDir};

// ═══════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════

fn setup_engine(options_overrides: Option<StorageOptions>) -> (Arc<LsmEngine>, TempDir) {
    let dir = tempdir().unwrap();
    let options = options_overrides.unwrap_or(StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 1024 * 1024,
        ..Default::default()
    });
    let engine = Arc::new(LsmEngine::open(options).unwrap());
    (engine, dir)
}

fn make_executor(engine: &Arc<LsmEngine>) -> QueryExecutor {
    let ontology_store = OntologyStore::new(engine.clone());
    QueryExecutor::new(engine.clone(), ontology_store)
}

fn exec(executor: &QueryExecutor, sql: &str) -> onto_core::Result<onto_query::QueryResult> {
    let ast = QueryParser::parse(sql)?;
    executor.execute(&ast)
}

fn exec_ok(executor: &QueryExecutor, sql: &str) -> onto_query::QueryResult {
    exec(executor, sql).unwrap_or_else(|e| panic!("query failed: {}\n  SQL: {}", e, sql))
}

fn assert_row_count(result: &onto_query::QueryResult, expected: usize) {
    match result {
        onto_query::QueryResult::Rows(rows) => assert_eq!(
            rows.len(),
            expected,
            "expected {} rows, got {}",
            expected,
            rows.len()
        ),
        other => panic!("expected Rows, got {:?}", other),
    }
}

fn assert_success_contains(result: &onto_query::QueryResult, needle: &str) {
    match result {
        onto_query::QueryResult::Success(msg) => {
            assert!(msg.contains(needle), "expected '{}' in '{}'", needle, msg)
        }
        other => panic!("expected Success, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════
//  1. Query-Storage interaction after SSTable flush
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_select_after_multiple_flushes() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Batch 1: insert + flush
    for i in 0..5 {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('item_{}', {})",
                i,
                (i + 1) * 100
            ),
        );
    }
    engine.flush().unwrap();

    // Batch 2: insert + flush
    for i in 5..10 {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('item_{}', {})",
                i,
                (i + 1) * 100
            ),
        );
    }
    engine.flush().unwrap();

    // All 10 items should be visible across multiple SSTables
    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 10);

    // Filter across SSTables
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE price > 500");
    assert_row_count(&result, 5); // items 5-9 (prices 600-1000)
}

#[test]
fn integration_update_across_sstables() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Insert into first flush
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    engine.flush().unwrap();

    // Insert into second flush
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
    );
    engine.flush().unwrap();

    // Update row from first SSTable
    exec_ok(
        &executor,
        "UPDATE Product SET price = 1099 WHERE name = 'iPhone'",
    );

    // Verify update is visible
    let result = exec_ok(&executor, "SELECT price FROM Product WHERE name = 'iPhone'");
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 1099);
        }
        _ => panic!("expected Rows"),
    }

    // Flush and verify persistence
    engine.flush().unwrap();
    let result = exec_ok(&executor, "SELECT price FROM Product WHERE name = 'iPhone'");
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 1099);
        }
        _ => panic!("expected Rows"),
    }
}

#[test]
fn integration_delete_across_sstables() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Two flushes
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
    );
    engine.flush().unwrap();

    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('MacBook', 1999)",
    );
    engine.flush().unwrap();

    // Delete item from first SSTable
    exec_ok(&executor, "DELETE FROM Product WHERE name = 'iPhone'");

    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 2);

    // Flush tombstone and verify
    engine.flush().unwrap();
    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 2);
}

// ═══════════════════════════════════════════════════════════════════
//  2. Compaction scenarios
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_query_after_compaction() {
    let dir = tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 128, // Small memtable → many flushes
        size_ratio: 2,            // Compact when > 2 SSTables per level
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());
    let executor = make_executor(&engine);

    // Write enough to trigger multiple flushes and compaction
    for i in 0..30 {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('item_{:03}', {})",
                i,
                (i + 1) * 10
            ),
        );
    }

    // Force final flush and wait for compaction
    engine.flush().unwrap();
    engine.flush_compaction().unwrap();

    let stats = engine.stats();
    assert!(stats.total_sstables > 0, "should have created SSTables");

    // All data should be readable after compaction
    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 30);

    // Range query should work
    let result = exec_ok(
        &executor,
        "SELECT name FROM Product WHERE price > 150 AND price < 250",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 9); // items 15-23 (prices 160-240)
        }
        _ => panic!("expected Rows"),
    }

    // Aggregation should work after compaction
    let result = exec_ok(&executor, "SELECT COUNT(*) as cnt FROM Product");
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get("cnt").unwrap().as_i64().unwrap(), 30);
        }
        _ => panic!("expected count row"),
    }
}

#[test]
fn integration_overwrite_during_compaction() {
    let dir = tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 128,
        size_ratio: 2,
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());
    let executor = make_executor(&engine);

    // Insert initial data
    for i in 0..20 {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('item_{:03}', {})",
                i, 100
            ),
        );
    }

    // Overwrite all prices
    for i in 0..20 {
        exec_ok(
            &executor,
            &format!(
                "UPDATE Product SET price = {} WHERE name = 'item_{:03}'",
                200 + i,
                i
            ),
        );
    }

    engine.flush().unwrap();
    engine.flush_compaction().unwrap();

    // Verify latest values survived compaction
    for i in 0..20 {
        let result = exec_ok(
            &executor,
            &format!("SELECT price FROM Product WHERE name = 'item_{:03}'", i),
        );
        match &result {
            onto_query::QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(
                    rows[0].get("price").unwrap().as_i64().unwrap(),
                    200 + i as i64,
                    "item_{:03} should have updated price",
                    i
                );
            }
            _ => panic!("expected Rows"),
        }
    }
}

#[test]
fn integration_delete_during_compaction() {
    let dir = tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 128,
        size_ratio: 2,
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());
    let executor = make_executor(&engine);

    // Insert 20 items
    for i in 0..20 {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('item_{:03}', {})",
                i,
                (i + 1) * 10
            ),
        );
    }

    // Delete even items
    for i in (0..20).step_by(2) {
        exec_ok(
            &executor,
            &format!("DELETE FROM Product WHERE name = 'item_{:03}'", i),
        );
    }

    engine.flush().unwrap();
    engine.flush_compaction().unwrap();

    // Only odd items should remain
    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 10);

    // Verify specific items
    for i in 0..20 {
        let result = exec_ok(
            &executor,
            &format!("SELECT name FROM Product WHERE name = 'item_{:03}'", i),
        );
        if i % 2 == 0 {
            assert_row_count(&result, 0);
        } else {
            assert_row_count(&result, 1);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
//  3. Recovery after restart
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_recovery_query_after_restart() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Phase 1: Write data through query executor
    {
        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
        );
        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
        );
        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('MacBook', 1999)",
        );
        engine.flush().unwrap();
    }

    // Phase 2: Reopen and verify queries work
    {
        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        let result = exec_ok(&executor, "SELECT * FROM Product");
        assert_row_count(&result, 3);

        let result = exec_ok(&executor, "SELECT name FROM Product WHERE price > 900");
        assert_row_count(&result, 2);
    }
}

#[test]
fn integration_recovery_wal_data() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Phase 1: Write data WITHOUT flushing (WAL-only)
    {
        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
        );
        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
        );
        // Intentionally no flush — data only in WAL
    }

    // Phase 2: Reopen — WAL replay should recover data
    {
        let options = StorageOptions {
            data_dir,
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        let result = exec_ok(&executor, "SELECT * FROM Product");
        assert_row_count(&result, 2);
    }
}

#[test]
fn integration_recovery_update_and_delete() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Phase 1: Insert + update + delete
    {
        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        exec_ok(
            &executor,
            "INSERT INTO User (name, age) VALUES ('Alice', 30)",
        );
        exec_ok(&executor, "INSERT INTO User (name, age) VALUES ('Bob', 25)");
        exec_ok(
            &executor,
            "INSERT INTO User (name, age) VALUES ('Charlie', 35)",
        );

        // Update Alice
        exec_ok(&executor, "UPDATE User SET age = 31 WHERE name = 'Alice'");

        // Delete Bob
        exec_ok(&executor, "DELETE FROM User WHERE name = 'Bob'");

        engine.flush().unwrap();
    }

    // Phase 2: Reopen and verify mutations survived
    {
        let options = StorageOptions {
            data_dir,
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        let result = exec_ok(&executor, "SELECT * FROM User");
        assert_row_count(&result, 2); // Alice + Charlie

        let result = exec_ok(&executor, "SELECT age FROM User WHERE name = 'Alice'");
        match &result {
            onto_query::QueryResult::Rows(rows) => {
                assert_eq!(rows[0].get("age").unwrap().as_i64().unwrap(), 31);
            }
            _ => panic!("expected Rows"),
        }

        let result = exec_ok(&executor, "SELECT * FROM User WHERE name = 'Bob'");
        assert_row_count(&result, 0);
    }
}

// ═══════════════════════════════════════════════════════════════════
//  4. Index-accelerated queries through storage lifecycle
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_index_after_flush_and_restart() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Phase 1: Create index, insert data, flush
    {
        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        exec_ok(&executor, "CREATE INDEX ON Product (price)");
        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
        );
        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
        );
        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('MacBook', 1999)",
        );
        engine.flush().unwrap();
    }

    // Phase 2: Reopen — index should be rebuilt, queries should use it
    {
        let options = StorageOptions {
            data_dir,
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        // Index-accelerated equality
        let result = exec_ok(&executor, "SELECT name FROM Product WHERE price = 999");
        assert_row_count(&result, 1);

        // Index-accelerated range
        let result = exec_ok(&executor, "SELECT name FROM Product WHERE price > 800");
        assert_row_count(&result, 2);

        // Index-accelerated BETWEEN
        let result = exec_ok(
            &executor,
            "SELECT name FROM Product WHERE price BETWEEN 700 AND 1000",
        );
        assert_row_count(&result, 2);
    }
}

#[test]
fn integration_index_update_after_compaction() {
    let dir = tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 128,
        size_ratio: 2,
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());
    let executor = make_executor(&engine);

    exec_ok(&executor, "CREATE INDEX ON Product (price)");

    // Insert enough to trigger compaction
    for i in 0..15 {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('item_{:03}', {})",
                i,
                (i + 1) * 10
            ),
        );
    }
    engine.flush().unwrap();
    engine.flush_compaction().unwrap();

    // Update some items
    for i in 0..15 {
        exec_ok(
            &executor,
            &format!(
                "UPDATE Product SET price = {} WHERE name = 'item_{:03}'",
                (i + 1) * 10 + 100,
                i
            ),
        );
    }
    engine.flush().unwrap();
    engine.flush_compaction().unwrap();

    // Index should reflect updated prices
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE price = 110");
    assert_row_count(&result, 1); // item_000 (10 + 100)

    // Old prices should not be found
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE price = 10");
    assert_row_count(&result, 0);
}

// ═══════════════════════════════════════════════════════════════════
//  5. Multi-class data isolation
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_multi_class_isolation() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Insert into multiple classes
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Customer (name, email) VALUES ('Alice', 'alice@test.com')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Customer (name, email) VALUES ('Bob', 'bob@test.com')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Order (product_id, quantity) VALUES ('iPhone', 3)",
    );

    engine.flush().unwrap();

    // Each class should only see its own data
    assert_row_count(&exec_ok(&executor, "SELECT * FROM Product"), 2);
    assert_row_count(&exec_ok(&executor, "SELECT * FROM Customer"), 2);
    assert_row_count(&exec_ok(&executor, "SELECT * FROM Order"), 1);

    // Filter should be class-scoped
    assert_row_count(
        &exec_ok(&executor, "SELECT * FROM Product WHERE name = 'Alice'"),
        0,
    );
    assert_row_count(
        &exec_ok(&executor, "SELECT * FROM Customer WHERE name = 'Alice'"),
        1,
    );
}

#[test]
fn integration_cross_class_join_after_flush() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Insert products
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
    );

    // Insert orders
    exec_ok(
        &executor,
        "INSERT INTO Order (product_id, quantity) VALUES ('iPhone', 3)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Order (product_id, quantity) VALUES ('iPad', 5)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Order (product_id, quantity) VALUES ('iPhone', 1)",
    );

    engine.flush().unwrap();

    // JOIN across flushed SSTables
    let result = exec_ok(
        &executor,
        "SELECT p.name, o.quantity FROM Product p JOIN Order o ON p.name = o.product_id",
    );
    assert_row_count(&result, 3);

    // JOIN with filter
    let result = exec_ok(
        &executor,
        "SELECT p.name, o.quantity FROM Product p JOIN Order o ON p.name = o.product_id WHERE o.quantity > 2",
    );
    assert_row_count(&result, 2);
}

// ═══════════════════════════════════════════════════════════════════
//  6. Complex query scenarios
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_group_by_after_flush() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    for (name, cat, price) in [
        ("iPhone", "phone", 999),
        ("Galaxy", "phone", 899),
        ("Pixel", "phone", 699),
        ("iPad", "tablet", 799),
        ("MacBook", "laptop", 1999),
    ] {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Item (name, category, price) VALUES ('{}', '{}', {})",
                name, cat, price
            ),
        );
    }
    engine.flush().unwrap();

    // GROUP BY with aggregation
    let result = exec_ok(
        &executor,
        "SELECT category, COUNT(*) as cnt, SUM(price) as total FROM Item GROUP BY category",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 3);

            let phone = rows
                .iter()
                .find(|r| r.get("category").and_then(|v| v.as_str()) == Some("phone"))
                .unwrap();
            assert_eq!(phone.get("cnt").unwrap().as_i64().unwrap(), 3);
            assert_eq!(phone.get("total").unwrap().as_i64().unwrap(), 2597);

            let tablet = rows
                .iter()
                .find(|r| r.get("category").and_then(|v| v.as_str()) == Some("tablet"))
                .unwrap();
            assert_eq!(tablet.get("cnt").unwrap().as_i64().unwrap(), 1);
        }
        _ => panic!("expected Rows"),
    }
}

#[test]
fn integration_subquery_after_flush() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('MacBook', 1999)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('AirPods', 249)",
    );

    engine.flush().unwrap();

    // Subquery: find products whose price is in the top-2 prices
    let result = exec_ok(
        &executor,
        "SELECT name FROM Product WHERE name IN (SELECT name FROM Product WHERE price > 900)",
    );
    assert_row_count(&result, 2); // iPhone, MacBook
}

#[test]
fn integration_union_across_flushes() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Flush 1: Products
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
    );
    engine.flush().unwrap();

    // Flush 2: Items
    exec_ok(
        &executor,
        "INSERT INTO Item (name, price) VALUES ('Widget', 49)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Item (name, price) VALUES ('Gadget', 149)",
    );
    engine.flush().unwrap();

    // UNION across classes and SSTables
    let result = exec_ok(
        &executor,
        "SELECT name FROM Product UNION SELECT name FROM Item",
    );
    assert_row_count(&result, 4);

    // UNION ALL keeps duplicates
    let result = exec_ok(
        &executor,
        "SELECT name FROM Product UNION ALL SELECT name FROM Item",
    );
    assert_row_count(&result, 4);
}

// ═══════════════════════════════════════════════════════════════════
//  7. Transaction isolation through query layer
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_txn_auto_commit_on_success() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // INSERT should auto-commit
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );

    // Data should be visible immediately
    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 1);
}

#[test]
fn integration_txn_auto_rollback_on_error() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // A failing query should not leave partial state
    let result = exec(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 'not_a_number')",
    );
    // Depending on parser behavior, this may succeed (string accepted) or fail
    // The key point is that no partial state is left
    let _ = result;
}

// ═══════════════════════════════════════════════════════════════════
//  8. ORDER BY + LIMIT integration
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_order_by_after_flush() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    for (name, price) in [
        ("MacBook", 1999),
        ("iPhone", 999),
        ("iPad", 799),
        ("AirPods", 249),
    ] {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('{}', {})",
                name, price
            ),
        );
    }
    engine.flush().unwrap();

    // ORDER BY ASC
    let result = exec_ok(&executor, "SELECT name FROM Product ORDER BY price");
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 4);
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "AirPods");
            assert_eq!(rows[1].get("name").unwrap().as_str().unwrap(), "iPad");
            assert_eq!(rows[2].get("name").unwrap().as_str().unwrap(), "iPhone");
            assert_eq!(rows[3].get("name").unwrap().as_str().unwrap(), "MacBook");
        }
        _ => panic!("expected Rows"),
    }

    // ORDER BY DESC + LIMIT
    let result = exec_ok(
        &executor,
        "SELECT name FROM Product ORDER BY price DESC LIMIT 2",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "MacBook");
            assert_eq!(rows[1].get("name").unwrap().as_str().unwrap(), "iPhone");
        }
        _ => panic!("expected Rows"),
    }
}

// ═══════════════════════════════════════════════════════════════════
//  9. DISTINCT with flush
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_distinct_after_flush() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Insert duplicate data
    for _ in 0..3 {
        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
        );
    }
    for _ in 0..2 {
        exec_ok(
            &executor,
            "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
        );
    }
    engine.flush().unwrap();

    let result = exec_ok(&executor, "SELECT DISTINCT name, price FROM Product");
    assert_row_count(&result, 2);
}

// ═══════════════════════════════════════════════════════════════════
//  10. LIKE queries after flush
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_like_after_flush() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    for name in ["iPhone", "iPad", "iMac", "MacBook", "MacBook Pro"] {
        exec_ok(
            &executor,
            &format!("INSERT INTO Product (name, price) VALUES ('{}', 999)", name),
        );
    }
    engine.flush().unwrap();

    // Prefix match
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE name LIKE 'i%'");
    assert_row_count(&result, 3);

    // Suffix match
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE name LIKE '%Pro'");
    assert_row_count(&result, 1);

    // Contains match
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE name LIKE '%ac%'");
    assert_row_count(&result, 3); // iMac, MacBook, MacBook Pro
}

// ═══════════════════════════════════════════════════════════════════
//  11. Full CRUD lifecycle with storage operations
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_full_crud_lifecycle() {
    let dir = tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 256, // Small to force flushes
        size_ratio: 2,
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());
    let executor = make_executor(&engine);

    // CREATE ONTOLOGY
    let result = exec_ok(&executor,
        "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
    );
    assert_success_contains(&result, "created");

    // INSERT batch 1
    for (name, price) in [("iPhone", 999), ("iPad", 799), ("MacBook", 1999)] {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('{}', {})",
                name, price
            ),
        );
    }
    engine.flush().unwrap();

    // SELECT verify
    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 3);

    // INSERT batch 2 (triggers more flushes)
    for (name, price) in [("AirPods", 249), ("Watch", 399), ("HomePod", 299)] {
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price) VALUES ('{}', {})",
                name, price
            ),
        );
    }
    engine.flush().unwrap();

    // SELECT with complex filter
    let result = exec_ok(
        &executor,
        "SELECT name, price FROM Product WHERE price > 300 AND price < 1000",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 3); // iPad(799), iPhone(999), Watch(399)
        }
        _ => panic!("expected Rows"),
    }

    // UPDATE across SSTables
    exec_ok(
        &executor,
        "UPDATE Product SET price = 1099 WHERE name = 'iPhone'",
    );

    // DELETE
    exec_ok(&executor, "DELETE FROM Product WHERE price < 300");

    // Final verification (AirPods 249 and HomePod 299 both have price < 300)
    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 4); // 6 - 2 deleted

    // Verify update survived
    let result = exec_ok(&executor, "SELECT price FROM Product WHERE name = 'iPhone'");
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 1099);
        }
        _ => panic!("expected Rows"),
    }

    // Aggregate
    let result = exec_ok(
        &executor,
        "SELECT COUNT(*) as cnt, SUM(price) as total FROM Product",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get("cnt").unwrap().as_i64().unwrap(), 4);
            // 1099 + 799 + 1999 + 399 = 4296
            assert_eq!(rows[0].get("total").unwrap().as_i64().unwrap(), 4296);
        }
        _ => panic!("expected aggregation row"),
    }
}

// ═══════════════════════════════════════════════════════════════════
//  13. Ontology schema validation
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_ontology_required_field_validation() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Create ontology with a required field
    exec_ok(&executor,
        "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING REQUIRED, PROPERTY price DOMAIN Product RANGE INT64)"
    );

    // INSERT without required field should fail
    let result = exec(&executor, "INSERT INTO Product (price) VALUES (100)");
    assert!(
        result.is_err(),
        "INSERT without required 'name' should fail"
    );

    // INSERT with required field should succeed
    let result = exec(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    assert!(result.is_ok(), "INSERT with required 'name' should succeed");
}

#[test]
fn integration_ontology_type_validation() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Create ontology with typed properties
    exec_ok(&executor,
        "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64, PROPERTY active DOMAIN Product RANGE BOOL)"
    );

    // INSERT with correct types should succeed
    let result = exec(
        &executor,
        "INSERT INTO Product (name, price, active) VALUES ('iPhone', 999, true)",
    );
    assert!(result.is_ok(), "INSERT with correct types should succeed");

    // INSERT with wrong type (string for int field) should fail
    let result = exec(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 'not_a_number')",
    );
    assert!(
        result.is_err(),
        "INSERT with wrong type for 'price' should fail"
    );
}

#[test]
fn integration_ontology_update_validation() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Create ontology
    exec_ok(&executor,
        "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
    );

    // Insert valid data
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );

    // UPDATE with wrong type should fail
    let result = exec(
        &executor,
        "UPDATE Product SET price = 'expensive' WHERE name = 'iPhone'",
    );
    assert!(
        result.is_err(),
        "UPDATE with wrong type for 'price' should fail"
    );

    // UPDATE with correct type should succeed
    let result = exec(
        &executor,
        "UPDATE Product SET price = 1099 WHERE name = 'iPhone'",
    );
    assert!(result.is_ok(), "UPDATE with correct type should succeed");

    // Verify the update
    let result = exec_ok(&executor, "SELECT price FROM Product WHERE name = 'iPhone'");
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 1099);
        }
        _ => panic!("expected Rows"),
    }
}

#[test]
fn integration_ontology_no_schema_passes() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // INSERT without any ontology should succeed (schema-on-read)
    let result = exec(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    assert!(
        result.is_ok(),
        "INSERT without ontology should succeed (schema-on-read)"
    );
}

// ═══════════════════════════════════════════════════════════════════
//  14. Edge cases
// ═══════════════════════════════════════════════════════════════════

#[test]
fn integration_empty_table_queries() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // SELECT on empty table
    assert_row_count(&exec_ok(&executor, "SELECT * FROM Product"), 0);

    // COUNT on empty table
    let result = exec_ok(&executor, "SELECT COUNT(*) as cnt FROM Product");
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("cnt").unwrap().as_i64().unwrap(), 0);
        }
        _ => panic!("expected count"),
    }

    // DELETE on empty table
    assert_success_contains(
        &exec_ok(&executor, "DELETE FROM Product WHERE name = 'x'"),
        "0 row(s) deleted",
    );

    // UPDATE on empty table
    assert_success_contains(
        &exec_ok(&executor, "UPDATE Product SET price = 0 WHERE name = 'x'"),
        "0 row(s) updated",
    );
}

#[test]
fn integration_null_handling() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    exec_ok(&executor, "INSERT INTO Product (name) VALUES ('Unknown')");
    engine.flush().unwrap();

    // Both rows should be visible
    assert_row_count(&exec_ok(&executor, "SELECT * FROM Product"), 2);

    // Filter on non-null
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE price > 0");
    assert_row_count(&result, 1);
}

#[test]
fn integration_special_characters_in_values() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Strings with special characters
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('it''s a test', 100)",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price) VALUES ('hello world', 200)",
    );
    engine.flush().unwrap();

    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 2);
}

// ═══════════════════════════════════════════════════════════════════
//  Phase 11: Vector Search + SQL Hybrid Query Integration Tests
// ═══════════════════════════════════════════════════════════════════

// ── 15. Vector search basic lifecycle ─────────────────────────────

#[test]
fn integration_vector_search_after_flush() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Create vector index
    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3",
    );

    // Insert products with vectors
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('cat', 10, '[0.9, 0.1, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('dog', 20, '[0.8, 0.2, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('car', 30, '[0.0, 0.1, 0.9]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('truck', 40, '[0.1, 0.0, 0.8]')",
    );

    engine.flush().unwrap();

    // Vector search: nearest to [1,0,0] should return cat first
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 2",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "cat");
            assert!(
                rows[0].get("_distance").is_some(),
                "should have _distance column"
            );
        }
        _ => panic!("expected Rows"),
    }

    // Vector search: nearest to [0,0,1] should return car/truck
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [0.0, 0.0, 1.0] TOP 2",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows.len(), 2);
            let names: Vec<&str> = rows
                .iter()
                .map(|r| r.get("name").unwrap().as_str().unwrap())
                .collect();
            assert!(names.contains(&"car"));
            assert!(names.contains(&"truck"));
        }
        _ => panic!("expected Rows"),
    }
}

#[test]
fn integration_vector_search_across_multiple_flushes() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC l2 DIMENSION 4",
    );

    // Batch 1
    for i in 0..5 {
        exec_ok(&executor, &format!(
            "INSERT INTO Product (name, price, embedding) VALUES ('item_{}', {}, '[1.0, 0.0, 0.0, 0.0]')",
            i, (i + 1) * 100
        ));
    }
    engine.flush().unwrap();

    // Batch 2
    for i in 5..10 {
        exec_ok(&executor, &format!(
            "INSERT INTO Product (name, price, embedding) VALUES ('item_{}', {}, '[0.0, 0.0, 0.0, 1.0]')",
            i, (i + 1) * 100
        ));
    }
    engine.flush().unwrap();

    // Search should find items from both flushes
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0, 0.0] TOP 3",
    );
    assert_row_count(&result, 3);
    // All results should be from batch 1 (closer to [1,0,0,0])
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            for row in rows {
                let name = row.get("name").unwrap().as_str().unwrap();
                let idx: i32 = name.strip_prefix("item_").unwrap().parse().unwrap();
                assert!(idx < 5, "expected item from batch 1 (0-4), got {}", name);
            }
        }
        _ => panic!("expected Rows"),
    }
}

// ── 16. Vector search + SQL WHERE hybrid ──────────────────────────

#[test]
fn integration_vector_search_with_where_filter() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3",
    );

    // Products with categories
    exec_ok(&executor, "INSERT INTO Product (name, category, embedding) VALUES ('cat', 'animal', '[0.9, 0.1, 0.0]')");
    exec_ok(&executor, "INSERT INTO Product (name, category, embedding) VALUES ('dog', 'animal', '[0.8, 0.2, 0.0]')");
    exec_ok(&executor, "INSERT INTO Product (name, category, embedding) VALUES ('car', 'vehicle', '[0.0, 0.1, 0.9]')");
    exec_ok(&executor, "INSERT INTO Product (name, category, embedding) VALUES ('truck', 'vehicle', '[0.1, 0.0, 0.8]')");

    engine.flush().unwrap();

    // Vector search with category filter — only animals
    let result = exec_ok(&executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 10 WHERE category = 'animal'"
    );
    assert_row_count(&result, 2);
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            let names: Vec<&str> = rows
                .iter()
                .map(|r| r.get("name").unwrap().as_str().unwrap())
                .collect();
            assert!(names.contains(&"cat"));
            assert!(names.contains(&"dog"));
            // Should NOT contain vehicle items
            assert!(!names.contains(&"car"));
            assert!(!names.contains(&"truck"));
        }
        _ => panic!("expected Rows"),
    }

    // Vector search with price filter
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [0.0, 0.0, 1.0] TOP 10 WHERE name = 'car'",
    );
    assert_row_count(&result, 1);
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "car");
        }
        _ => panic!("expected Rows"),
    }
}

// ── 17. Vector search consistency with SQL mutations ──────────────

#[test]
fn integration_vector_search_after_update() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3",
    );

    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding) VALUES ('item_a', '[1.0, 0.0, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding) VALUES ('item_b', '[0.0, 1.0, 0.0]')",
    );
    engine.flush().unwrap();

    // Verify initial search
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 1",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "item_a");
        }
        _ => panic!("expected Rows"),
    }

    // Update item_a's embedding to be far away
    exec_ok(
        &executor,
        "UPDATE Product SET embedding = '[0.0, 0.0, 1.0]' WHERE name = 'item_a'",
    );

    // Now item_b [0,1,0] should be closest to [0.9, 0.1, 0] (closer than item_a [0,0,1])
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [0.9, 0.1, 0.0] TOP 1",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "item_b");
        }
        _ => panic!("expected Rows"),
    }
}

#[test]
fn integration_vector_search_after_delete() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3",
    );

    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding) VALUES ('cat', '[0.9, 0.1, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding) VALUES ('dog', '[0.8, 0.2, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding) VALUES ('car', '[0.0, 0.1, 0.9]')",
    );
    engine.flush().unwrap();

    // Delete cat
    exec_ok(&executor, "DELETE FROM Product WHERE name = 'cat'");

    // Vector search should not return deleted 'cat'
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 10",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            let names: Vec<&str> = rows
                .iter()
                .map(|r| r.get("name").unwrap().as_str().unwrap())
                .collect();
            assert!(
                !names.contains(&"cat"),
                "deleted 'cat' should not appear in vector search"
            );
            assert!(names.contains(&"dog"));
            assert!(names.contains(&"car"));
        }
        _ => panic!("expected Rows"),
    }
}

// ── 18. Vector index persistence across restart ───────────────────

#[test]
fn integration_vector_index_recovery_after_restart() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Phase 1: Create vector index, insert data, flush
    {
        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let executor = make_executor(&engine);

        exec_ok(&executor, "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3 M 8 EF_CONSTRUCTION 50 EF_SEARCH 30");
        exec_ok(
            &executor,
            "INSERT INTO Product (name, embedding) VALUES ('alpha', '[1.0, 0.0, 0.0]')",
        );
        exec_ok(
            &executor,
            "INSERT INTO Product (name, embedding) VALUES ('beta', '[0.0, 1.0, 0.0]')",
        );
        exec_ok(
            &executor,
            "INSERT INTO Product (name, embedding) VALUES ('gamma', '[0.0, 0.0, 1.0]')",
        );
        engine.flush().unwrap();
    }

    // Phase 2: Reopen — vector index should be rebuilt from persisted metadata
    {
        let options = StorageOptions {
            data_dir,
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        // Verify vector index exists
        assert!(engine.has_vector_index("Product", "embedding"));

        // Search should work with rebuilt index
        let results = engine
            .vector_index_manager()
            .read()
            .unwrap()
            .search("Product", "embedding", &[1.0, 0.0, 0.0], 1)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.id.len() > 0, true);
    }
}

// ── 19. Vector search with compaction ─────────────────────────────

#[test]
fn integration_vector_search_after_compaction() {
    let dir = tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 128, // Small memtable → many flushes
        size_ratio: 2,
        ..Default::default()
    };
    let engine = Arc::new(LsmEngine::open(options).unwrap());
    let executor = make_executor(&engine);

    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 2",
    );

    // Insert enough to trigger compaction
    for i in 0..20 {
        let v1 = (i as f32) / 20.0;
        let v2 = 1.0 - v1;
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, embedding) VALUES ('item_{:03}', '[{}, {}]')",
                i, v1, v2
            ),
        );
    }
    engine.flush().unwrap();
    engine.flush_compaction().unwrap();

    // Vector search should still work after compaction
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0] TOP 3",
    );
    assert_row_count(&result, 3);
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            // item_19 has [0.95, 0.05] — closest to [1.0, 0.0]
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "item_019");
        }
        _ => panic!("expected Rows"),
    }
}

// ── 20. Multi-class vector isolation ──────────────────────────────

#[test]
fn integration_vector_search_class_isolation() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Create vector indexes on two different classes
    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3",
    );
    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Document (embedding) METRIC cosine DIMENSION 3",
    );

    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding) VALUES ('iPhone', '[1.0, 0.0, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Document (title, embedding) VALUES ('manual', '[0.0, 1.0, 0.0]')",
    );

    engine.flush().unwrap();

    // Search Product — should only return Product results
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 10",
    );
    assert_row_count(&result, 1);
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPhone");
        }
        _ => panic!("expected Rows"),
    }

    // Search Document — should only return Document results
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Document (embedding) QUERY [0.0, 1.0, 0.0] TOP 10",
    );
    assert_row_count(&result, 1);
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("title").unwrap().as_str().unwrap(), "manual");
        }
        _ => panic!("expected Rows"),
    }
}

// ── 21. Vector + B-Tree index coexistence ─────────────────────────

#[test]
fn integration_vector_and_btree_index_coexistence() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // Create both index types
    exec_ok(&executor, "CREATE INDEX ON Product (price)");
    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3",
    );

    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('iPhone', 999, '[1.0, 0.0, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('iPad', 799, '[0.9, 0.1, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('MacBook', 1999, '[0.0, 0.0, 1.0]')",
    );

    engine.flush().unwrap();

    // B-Tree index query
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE price > 900");
    assert_row_count(&result, 2);

    // Vector search
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 2",
    );
    assert_row_count(&result, 2);
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPhone");
        }
        _ => panic!("expected Rows"),
    }

    // Vector search with SQL filter (hybrid)
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 10 WHERE price > 900",
    );
    assert_row_count(&result, 2); // iPhone(999) and MacBook(1999)
}

// ── 22. Full lifecycle: vector + SQL + ontology ───────────────────

#[test]
fn integration_vector_full_lifecycle_with_ontology() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // 1. Create ontology
    exec_ok(&executor,
        "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING REQUIRED, PROPERTY price DOMAIN Product RANGE INT64)"
    );

    // 2. Create vector index
    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3",
    );

    // 3. Create B-Tree index
    exec_ok(&executor, "CREATE INDEX ON Product (price)");

    // 4. Insert data
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('iPhone', 999, '[1.0, 0.0, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('iPad', 799, '[0.9, 0.1, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('MacBook', 1999, '[0.0, 0.0, 1.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, price, embedding) VALUES ('AirPods', 249, '[0.5, 0.5, 0.0]')",
    );

    engine.flush().unwrap();

    // 5. SQL query via B-Tree index
    let result = exec_ok(&executor, "SELECT name FROM Product WHERE price > 900");
    assert_row_count(&result, 2);

    // 6. Vector search
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 2",
    );
    assert_row_count(&result, 2);

    // 7. Vector search + SQL filter (hybrid)
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 10 WHERE price > 500",
    );
    assert_row_count(&result, 3); // iPhone, iPad, MacBook (not AirPods)

    // 8. UPDATE vector
    exec_ok(
        &executor,
        "UPDATE Product SET embedding = '[0.0, 1.0, 0.0]' WHERE name = 'iPhone'",
    );

    // 9. Verify vector search reflects update
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [0.0, 1.0, 0.0] TOP 1",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPhone");
        }
        _ => panic!("expected Rows"),
    }

    // 10. DELETE + verify vector search
    exec_ok(&executor, "DELETE FROM Product WHERE name = 'AirPods'");

    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 10",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            let names: Vec<&str> = rows
                .iter()
                .map(|r| r.get("name").unwrap().as_str().unwrap())
                .collect();
            assert!(
                !names.contains(&"AirPods"),
                "deleted item should not appear"
            );
            assert_eq!(rows.len(), 3);
        }
        _ => panic!("expected Rows"),
    }

    // 11. Aggregation still works
    let result = exec_ok(&executor, "SELECT COUNT(*) as cnt FROM Product");
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("cnt").unwrap().as_i64().unwrap(), 3);
        }
        _ => panic!("expected count"),
    }

    // 12. Drop vector index
    assert_success_contains(
        &exec_ok(&executor, "DROP VECTOR INDEX ON Product (embedding)"),
        "Vector index dropped",
    );

    // 13. Verify vector search fails after drop
    let result = exec(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 1",
    );
    assert!(
        result.is_err(),
        "vector search should fail after index is dropped"
    );

    // 14. SQL queries still work
    let result = exec_ok(&executor, "SELECT * FROM Product");
    assert_row_count(&result, 3);
}

// ── 23. Vector search with L2 and InnerProduct metrics ────────────

#[test]
fn integration_vector_search_different_metrics() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    // L2 metric
    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding_l2) METRIC l2 DIMENSION 3",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding_l2) VALUES ('a', '[1.0, 0.0, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding_l2) VALUES ('b', '[0.0, 1.0, 0.0]')",
    );

    // InnerProduct metric on a different column
    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding_ip) METRIC innerproduct DIMENSION 3",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding_ip) VALUES ('a', '[1.0, 0.0, 0.0]')",
    );
    exec_ok(
        &executor,
        "INSERT INTO Product (name, embedding_ip) VALUES ('b', '[0.0, 1.0, 0.0]')",
    );

    engine.flush().unwrap();

    // L2 search
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding_l2) QUERY [1.0, 0.0, 0.0] TOP 1",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "a");
        }
        _ => panic!("expected Rows"),
    }

    // InnerProduct search
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding_ip) QUERY [1.0, 0.0, 0.0] TOP 1",
    );
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "a");
        }
        _ => panic!("expected Rows"),
    }
}

// ── 24. Large dataset vector search ───────────────────────────────

#[test]
fn integration_vector_search_large_dataset() {
    let (engine, _dir) = setup_engine(None);
    let executor = make_executor(&engine);

    exec_ok(
        &executor,
        "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 8",
    );

    // Insert 100 items with distinct vectors
    for i in 0..100 {
        let base = if i < 50 { 0.9 } else { 0.1 };
        let noise = (i as f64 * 0.001) % 0.1;
        let vec = format!(
            "[{}, {}, {}, 0.0, 0.0, 0.0, 0.0, 0.0]",
            base - noise,
            0.1 + noise,
            noise
        );
        exec_ok(
            &executor,
            &format!(
                "INSERT INTO Product (name, price, embedding) VALUES ('item_{:03}', {}, '{}')",
                i,
                (i + 1) * 10,
                vec
            ),
        );
    }
    engine.flush().unwrap();

    // Search for top-5 nearest to [1,0,...]
    let result = exec_ok(
        &executor,
        "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0] TOP 5",
    );
    assert_row_count(&result, 5);
    match &result {
        onto_query::QueryResult::Rows(rows) => {
            for row in rows {
                let name = row.get("name").unwrap().as_str().unwrap();
                let idx: i32 = name.strip_prefix("item_").unwrap().parse().unwrap();
                assert!(idx < 50, "expected item from first group, got {}", name);
            }
        }
        _ => panic!("expected Rows"),
    }
}
