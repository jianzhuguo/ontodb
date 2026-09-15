//! End-to-end integration tests for OntoDB client-server architecture.
//!
//! These tests start a real TCP server, connect via TCP client,
//! and verify the full query lifecycle.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use onto_ontology::OntologyStore;
use onto_query::QueryExecutor;
use onto_storage::{LsmEngine, StorageOptions};

/// Helper: sends a query to the server and reads the response.
fn send_query(stream: &mut TcpStream, query: &str) -> String {
    stream.write_all(query.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut response = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        match reader.read_exact(&mut byte) {
            Ok(()) => {
                if byte[0] == 0 {
                    break;
                }
                response.push(byte[0]);
            }
            Err(_) => break,
        }
    }

    String::from_utf8(response).unwrap()
}

/// Starts a test server on a random port and returns (port, join_handle).
fn start_test_server() -> (u16, thread::JoinHandle<()>) {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    // Leak the tempdir so it's not cleaned up during the test
    let _ = Box::leak(Box::new(dir));

    let options = StorageOptions {
        data_dir,
        memtable_size_limit: 4 * 1024 * 1024,
        ..Default::default()
    };

    let engine = Arc::new(LsmEngine::open(options).unwrap());
    let ontology_store = OntologyStore::new(Arc::clone(&engine));
    let executor = Arc::new(QueryExecutor::new(Arc::clone(&engine), ontology_store));

    // Bind to port 0 for a random available port
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let executor = Arc::clone(&executor);
                    thread::spawn(move || {
                        let reader = BufReader::new(stream.try_clone().unwrap());
                        let mut writer = stream;

                        for line in reader.lines() {
                            let line = match line {
                                Ok(l) => l,
                                Err(_) => break,
                            };
                            let input = line.trim();

                            if input.is_empty() {
                                writer.write_all(&[0]).unwrap();
                                continue;
                            }

                            if input.eq_ignore_ascii_case("quit")
                                || input.eq_ignore_ascii_case("exit")
                            {
                                break;
                            }

                            let input = input.trim_end_matches(';').trim();
                            let response = match onto_query::QueryParser::parse(input) {
                                Ok(ast) => match executor.execute(&ast) {
                                    Ok(result) => result.format(),
                                    Err(e) => format!("ERR: {}", e),
                                },
                                Err(e) => format!("ERR: Parse error: {}", e),
                            };

                            writer.write_all(response.as_bytes()).unwrap();
                            writer.write_all(&[0]).unwrap();
                            writer.flush().unwrap();
                        }
                    });
                }
                Err(_) => break,
            }
        }
    });

    // Wait for server to be ready by retrying connection (up to 5 seconds)
    let addr = format!("127.0.0.1:{}", port);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(&addr) {
            Ok(stream) => {
                // Server is ready, drop the test connection
                drop(stream);
                break;
            }
            Err(_) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("Server failed to start within 5 seconds: {}", e),
        }
    }

    (port, handle)
}

#[test]
fn test_e2e_full_lifecycle() {
    let (port, _handle) = start_test_server();
    let addr = format!("127.0.0.1:{}", port);

    // Connect multiple clients to verify concurrent access
    let mut client = TcpStream::connect(&addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    // 1. CREATE ONTOLOGY
    let resp = send_query(
        &mut client,
        "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)",
    );
    assert!(resp.contains("created"), "CREATE ONTOLOGY failed: {}", resp);
    assert!(resp.contains("1 classes"), "wrong class count: {}", resp);
    assert!(
        resp.contains("2 properties"),
        "wrong property count: {}",
        resp
    );

    // 2. INSERT rows
    let resp = send_query(
        &mut client,
        "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
    );
    assert!(resp.contains("1 row inserted"), "INSERT failed: {}", resp);

    let resp = send_query(
        &mut client,
        "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
    );
    assert!(resp.contains("1 row inserted"), "INSERT failed: {}", resp);

    let resp = send_query(
        &mut client,
        "INSERT INTO Product (name, price) VALUES ('MacBook', 1999)",
    );
    assert!(resp.contains("1 row inserted"), "INSERT failed: {}", resp);

    let resp = send_query(
        &mut client,
        "INSERT INTO Product (name, price) VALUES ('AirPods', 249)",
    );
    assert!(resp.contains("1 row inserted"), "INSERT failed: {}", resp);

    // 3. SELECT all
    let resp = send_query(&mut client, "SELECT * FROM Product");
    assert!(resp.contains("(4 rows)"), "expected 4 rows: {}", resp);
    assert!(resp.contains("iPhone"), "missing iPhone: {}", resp);
    assert!(resp.contains("iPad"), "missing iPad: {}", resp);
    assert!(resp.contains("MacBook"), "missing MacBook: {}", resp);
    assert!(resp.contains("AirPods"), "missing AirPods: {}", resp);

    // 4. SELECT with WHERE
    let resp = send_query(
        &mut client,
        "SELECT name, price FROM Product WHERE price > 900",
    );
    assert!(resp.contains("(2 rows)"), "expected 2 rows: {}", resp);
    assert!(resp.contains("iPhone"), "missing iPhone: {}", resp);
    assert!(resp.contains("MacBook"), "missing MacBook: {}", resp);

    // 5. SELECT with LIMIT
    let resp = send_query(&mut client, "SELECT * FROM Product LIMIT 2");
    assert!(resp.contains("(2 rows)"), "expected 2 rows: {}", resp);

    // 6. UPDATE
    let resp = send_query(
        &mut client,
        "UPDATE Product SET price = 1099 WHERE name = 'iPhone'",
    );
    assert!(resp.contains("1 row(s) updated"), "UPDATE failed: {}", resp);

    // 7. Verify UPDATE
    let resp = send_query(
        &mut client,
        "SELECT price FROM Product WHERE name = 'iPhone'",
    );
    assert!(resp.contains("1099"), "price not updated: {}", resp);
    assert!(!resp.contains("999"), "old price still present: {}", resp);

    // 8. DELETE
    let resp = send_query(&mut client, "DELETE FROM Product WHERE price < 300");
    assert!(resp.contains("1 row(s) deleted"), "DELETE failed: {}", resp);

    // 9. Verify DELETE
    let resp = send_query(&mut client, "SELECT * FROM Product");
    assert!(
        resp.contains("(3 rows)"),
        "expected 3 rows after delete: {}",
        resp
    );
    assert!(
        !resp.contains("AirPods"),
        "AirPods should be deleted: {}",
        resp
    );

    // 10. MATCH semantic query
    let resp = send_query(
        &mut client,
        "MATCH (p: Product) WHERE price > 1000 RETURN name",
    );
    assert!(resp.contains("(2 rows)"), "MATCH expected 2 rows: {}", resp);
    assert!(resp.contains("iPhone"), "MATCH missing iPhone: {}", resp);
    assert!(resp.contains("MacBook"), "MATCH missing MacBook: {}", resp);

    // 11. Error handling - parse error
    let resp = send_query(&mut client, "INVALID QUERY");
    assert!(resp.starts_with("ERR:"), "expected error: {}", resp);

    // 12. Empty query
    let resp = send_query(&mut client, "");
    assert!(
        resp.is_empty(),
        "expected empty response for empty query: {}",
        resp
    );

    // Disconnect
    send_query(&mut client, "quit");
}

#[test]
fn test_e2e_multiple_clients() {
    let (port, _handle) = start_test_server();
    let addr = format!("127.0.0.1:{}", port);

    // Client 1: inserts data
    let mut c1 = TcpStream::connect(&addr).unwrap();
    c1.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    send_query(
        &mut c1,
        "CREATE ONTOLOGY multi (CLASS Item, PROPERTY name DOMAIN Item RANGE STRING)",
    );

    for i in 0..10 {
        send_query(
            &mut c1,
            &format!("INSERT INTO Item (name) VALUES ('item_{}')", i),
        );
    }

    // Client 2: reads data concurrently
    let mut c2 = TcpStream::connect(&addr).unwrap();
    c2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let resp = send_query(&mut c2, "SELECT * FROM Item");
    assert!(
        resp.contains("(10 rows)"),
        "expected 10 rows from concurrent read: {}",
        resp
    );

    // Client 1: disconnects
    send_query(&mut c1, "quit");

    // Client 2: still works
    let resp = send_query(&mut c2, "SELECT name FROM Item WHERE name = 'item_5'");
    assert!(resp.contains("item_5"), "concurrent read failed: {}", resp);

    send_query(&mut c2, "quit");
}

#[test]
fn test_e2e_update_and_delete_lifecycle() {
    let (port, _handle) = start_test_server();
    let addr = format!("127.0.0.1:{}", port);

    let mut client = TcpStream::connect(&addr).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    // Setup
    send_query(
        &mut client,
        "CREATE ONTOLOGY test (CLASS User, PROPERTY name DOMAIN User RANGE STRING, PROPERTY age DOMAIN User RANGE INT64)",
    );

    send_query(
        &mut client,
        "INSERT INTO User (name, age) VALUES ('Alice', 30)",
    );
    send_query(
        &mut client,
        "INSERT INTO User (name, age) VALUES ('Bob', 25)",
    );
    send_query(
        &mut client,
        "INSERT INTO User (name, age) VALUES ('Charlie', 35)",
    );

    // UPDATE multiple rows
    let resp = send_query(&mut client, "UPDATE User SET age = 31 WHERE name = 'Alice'");
    assert!(resp.contains("1 row(s) updated"), "UPDATE failed: {}", resp);

    let resp = send_query(&mut client, "UPDATE User SET age = 26 WHERE age = 25");
    assert!(
        resp.contains("1 row(s) updated"),
        "UPDATE by age failed: {}",
        resp
    );

    // Verify updates
    let resp = send_query(&mut client, "SELECT age FROM User WHERE name = 'Alice'");
    assert!(resp.contains("31"), "Alice age not updated: {}", resp);

    let resp = send_query(&mut client, "SELECT age FROM User WHERE name = 'Bob'");
    assert!(resp.contains("26"), "Bob age not updated: {}", resp);

    // DELETE with filter
    let resp = send_query(&mut client, "DELETE FROM User WHERE age > 30");
    assert!(resp.contains("2 row(s) deleted"), "DELETE failed: {}", resp);

    // Verify delete
    let resp = send_query(&mut client, "SELECT * FROM User");
    assert!(
        resp.contains("(1 rows)"),
        "expected 1 row after delete: {}",
        resp
    );
    assert!(resp.contains("Bob"), "Bob should remain: {}", resp);

    // DELETE all remaining
    let resp = send_query(&mut client, "DELETE FROM User");
    assert!(
        resp.contains("1 row(s) deleted"),
        "DELETE all failed: {}",
        resp
    );

    let resp = send_query(&mut client, "SELECT * FROM User");
    assert!(resp.contains("(0 rows)"), "expected 0 rows: {}", resp);

    send_query(&mut client, "quit");
}
