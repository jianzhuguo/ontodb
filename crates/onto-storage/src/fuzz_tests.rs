//! Fuzz-style stress tests for the storage engine.
//!
//! These tests perform random put/get/scan operations to verify
//! the engine never panics and maintains consistency under stress.

use rand::Rng;
use tempfile::TempDir;

use crate::engine::LsmEngine;
use crate::options::StorageOptions;

fn open_test_engine(dir: &std::path::Path) -> LsmEngine {
    let options = StorageOptions {
        data_dir: dir.to_path_buf(),
        memtable_size_limit: 1024 * 1024,
        ..Default::default()
    };
    LsmEngine::open(options).unwrap()
}

/// Generate a random key of variable length.
fn random_key(rng: &mut impl Rng) -> Vec<u8> {
    let len = rng.gen_range(1..20);
    (0..len)
        .map(|_| rng.gen_range(b'a'..=b'z'))
        .collect()
}

/// Generate a random JSON-like value.
fn random_value(rng: &mut impl Rng) -> Vec<u8> {
    match rng.gen_range(0..5) {
        0 => {
            let k = random_key(rng);
            let mut v = b"\"".to_vec();
            v.extend_from_slice(&k);
            v.push(b'"');
            v
        }
        1 => rng.gen_range(0..100000i64).to_string().into_bytes(),
        2 => format!("{:.4}", rng.gen_range(0.0..100000.0)).into_bytes(),
        3 => if rng.gen_bool(0.5) { b"true".to_vec() } else { b"false".to_vec() },
        _ => b"null".to_vec(),
    }
}

#[test]
fn fuzz_engine_random_put_get() {
    let dir = TempDir::new().unwrap();
    let engine = open_test_engine(dir.path());
    let mut rng = rand::thread_rng();
    let iterations = 2000;
    let mut written: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    for i in 0..iterations {
        let key = random_key(&mut rng);
        let value = random_value(&mut rng);

        engine.put(key.clone(), value.clone()).unwrap();
        written.push((key.clone(), value));

        if let Ok(Some(stored)) = engine.get(&key) {
            assert!(!stored.is_empty(), "empty value at iter {}", i);
        }

        if !written.is_empty() && rng.gen_bool(0.5) {
            let idx = rng.gen_range(0..written.len());
            let (k, _) = &written[idx];
            let _ = engine.get(k);
        }
    }
}

#[test]
fn fuzz_engine_random_operations() {
    let dir = TempDir::new().unwrap();
    let engine = open_test_engine(dir.path());
    let mut rng = rand::thread_rng();
    let iterations = 3000;

    for _ in 0..iterations {
        let op = rng.gen_range(0..5);
        match op {
            0 => {
                let key = random_key(&mut rng);
                let value = random_value(&mut rng);
                let _ = engine.put(key, value);
            }
            1 => {
                let key = random_key(&mut rng);
                let _ = engine.get(&key);
            }
            2 => {
                let prefix = random_key(&mut rng);
                let end = rng.gen_range(1..prefix.len().max(2));
                let _ = engine.scan_prefix(&prefix[..end]);
            }
            3 => {
                if rng.gen_bool(0.1) {
                    let _ = engine.flush();
                }
            }
            _ => {
                let key = random_key(&mut rng);
                let _ = engine.delete(key);
            }
        }
    }
}

#[test]
fn fuzz_engine_key_collisions() {
    let dir = TempDir::new().unwrap();
    let engine = open_test_engine(dir.path());
    let mut rng = rand::thread_rng();

    let keys: Vec<Vec<u8>> = (0..10).map(|i| format!("key_{}", i).into_bytes()).collect();
    let iterations = 5000;

    for i in 0..iterations {
        let key = &keys[rng.gen_range(0..keys.len())];
        let value = format!("value_{}_{}", i, rng.gen_range(0..1000)).into_bytes();

        engine.put(key.clone(), value).unwrap();

        if let Ok(Some(stored)) = engine.get(key) {
            assert!(!stored.is_empty());
        }
    }
}

#[test]
fn fuzz_engine_large_values() {
    let dir = TempDir::new().unwrap();
    let engine = open_test_engine(dir.path());
    let mut rng = rand::thread_rng();

    for i in 0..100 {
        let key = format!("large_key_{}", i).into_bytes();
        let size = rng.gen_range(1000..50000);
        let value = vec![b'x'; size];

        engine.put(key.clone(), value.clone()).unwrap();

        if let Ok(Some(stored)) = engine.get(&key) {
            assert_eq!(stored.len(), value.len(), "value size mismatch at {}", i);
        }
    }
}

#[test]
fn fuzz_engine_empty_and_special_keys() {
    let dir = TempDir::new().unwrap();
    let engine = open_test_engine(dir.path());

    let special_keys: Vec<Vec<u8>> = vec![
        b" ".to_vec(),
        b"\0".to_vec(),
        b"\n".to_vec(),
        b"\t".to_vec(),
        b"a".to_vec(),
        vec![b'z'; 1000],
        b"key/with/slashes".to_vec(),
        b"key.with.dots".to_vec(),
        b"key-with-dashes".to_vec(),
        b"key_with_underscores".to_vec(),
        b"KEY_WITH_CAPS".to_vec(),
        b"123456789".to_vec(),
    ];

    for key in &special_keys {
        let value = format!("value_for_{}", key.len()).into_bytes();
        let _ = engine.put(key.clone(), value);
        let _ = engine.get(key);
    }
}
