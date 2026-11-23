//! Offline tombstone census over an SST directory (read-only, production-safe).
//!
//! Scans every *.sst directly via StreamingMergeIterator (NEVER opens
//! LsmEngine — engine open mutates a data dir) and reports Put/Delete
//! entry counts and bytes per LSM level and per key class.
//!
//! `--shadow` adds a two-pass estimate: pass A fills a bloom over
//! tombstoned key hashes, pass B counts Put bytes whose key is (probably)
//! tombstoned — i.e. deleted rows still occupying space. ~3% FP overcount.
//!
//! Run:
//!   cargo run -p onto-storage --release --example dbg_tombstone_census -- E:\sembio\data\ontodb_clean
//!   cargo run -p onto-storage --release --example dbg_tombstone_census -- E:\sembio\data\ontodb_clean --shadow

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use onto_core::types::EntryKind;
use onto_storage::lsm::streaming_merge::StreamingMergeIterator;

const BLOOM_BITS: usize = 1 << 30; // 2^30 bits = 128 MiB per plane

#[derive(Default, Clone, Debug)]
struct Agg {
    files: u64,
    file_bytes: u64,
    puts: u64,
    deletes: u64,
    put_bytes: u64,
    delete_bytes: u64,
}

fn level_of(name: &str) -> usize {
    name.trim_start_matches('L')
        .split('_')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(99)
}

fn classify(key: &[u8]) -> String {
    let s = String::from_utf8_lossy(key);
    for p in [
        "__idx2__",
        "__idx__",
        "__vec_meta__",
        "__vec_graph__",
        "__vec",
    ] {
        if s.starts_with(p) {
            return p.to_string();
        }
    }
    match s.find("::") {
        Some(i) => s[..i].to_string(),
        None => s.chars().take(24).collect(),
    }
}

#[inline]
fn hash64(key: &[u8]) -> u64 {
    // FNV-1a 64 — fast, deterministic, good enough for a bloom plane.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in key {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Set the two bloom bits for `key`. Returns (plane0 word/bit, plane1 word/bit).
#[inline]
fn bloom_coords(key: &[u8]) -> ((usize, u32), (usize, u32)) {
    let h = hash64(key);
    let mask = BLOOM_BITS - 1;
    let i0 = (h as usize & mask) / 64;
    let b0 = (h & 63) as u32;
    let h2 = (h >> 32) ^ h.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let i1 = (h2 as usize & mask) / 64;
    let b1 = (h2 & 63) as u32;
    ((i0, b0), (i1, b1))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(
        args.get(1)
            .map(|s| s.as_str())
            .unwrap_or(r"E:\sembio\data\ontodb_clean"),
    );
    let shadow = args.iter().any(|a| a == "--shadow");

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read data dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sst"))
        .collect();
    files.sort();
    println!("[census] dir={} files={}", dir.display(), files.len());
    println!(
        "[census] mode={}",
        if shadow { "count+shadow" } else { "count" }
    );

    let t0 = Instant::now();
    let mut by_level: HashMap<usize, Agg> = HashMap::new();
    let mut by_class: HashMap<String, Agg> = HashMap::new();
    let mut skipped = 0u64;

    // Pass 1: counts (and, with --shadow, fill the tombstone bloom).
    let mut p0 = vec![0u64; BLOOM_BITS / 64];
    let mut p1 = vec![0u64; BLOOM_BITS / 64];
    let mut total_deletes = 0u64;

    for (i, path) in files.iter().enumerate() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let level = level_of(&name);
        let file_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        let mut iter = match StreamingMergeIterator::new(&[path]) {
            Ok(it) => it,
            Err(e) => {
                eprintln!("  SKIP {name}: {e}");
                skipped += 1;
                continue;
            }
        };

        let lvl = by_level.entry(level).or_default();
        lvl.files += 1;
        lvl.file_bytes += file_bytes;

        while iter.has_next() {
            let Some(entry) = iter.next() else { break };
            let key_len = entry.key.len() as u64;
            let val_len = entry.value.len() as u64;
            let is_del = entry.kind == EntryKind::Delete;

            let cls = by_class.entry(classify(&entry.key)).or_default();
            if is_del {
                cls.deletes += 1;
                cls.delete_bytes += key_len + val_len + 16;
                lvl.deletes += 1;
                lvl.delete_bytes += key_len + val_len + 16;
                total_deletes += 1;
                if shadow {
                    let ((i0, b0), (i1, b1)) = bloom_coords(&entry.key);
                    p0[i0] |= 1u64 << b0;
                    p1[i1] |= 1u64 << b1;
                }
            } else {
                cls.puts += 1;
                cls.put_bytes += key_len + val_len + 16;
                lvl.puts += 1;
                lvl.put_bytes += key_len + val_len + 16;
            }
        }

        if (i + 1) % 25 == 0 || i + 1 == files.len() {
            println!(
                "  [{}/{}] {} elapsed={:.0}s",
                i + 1,
                files.len(),
                name,
                t0.elapsed().as_secs_f64()
            );
        }
    }

    // Pass 2: estimate Put bytes whose key has a tombstone somewhere.
    let (mut shadowed_puts, mut shadowed_put_bytes) = (0u64, 0u64);
    if shadow {
        println!(
            "[shadow] pass A done: {} tombstone keys bloomed, pass B scanning puts...",
            total_deletes
        );
        for path in &files {
            let Ok(mut iter) = StreamingMergeIterator::new(&[path]) else {
                continue;
            };
            while iter.has_next() {
                let Some(entry) = iter.next() else { break };
                if entry.kind == EntryKind::Delete {
                    continue;
                }
                let ((i0, b0), (i1, b1)) = bloom_coords(&entry.key);
                if p0[i0] & (1u64 << b0) != 0 && p1[i1] & (1u64 << b1) != 0 {
                    shadowed_puts += 1;
                    shadowed_put_bytes += entry.key.len() as u64 + entry.value.len() as u64 + 16;
                }
            }
        }
    }

    println!("\n===== per-level =====");
    println!(
        "{:<6} {:>6} {:>9} {:>13} {:>9} {:>12} {:>9} {:>9}",
        "level", "files", "diskMB", "puts", "putMB", "deletes", "delKB", "del%"
    );
    let mut levels: Vec<_> = by_level.into_iter().collect();
    levels.sort_by_key(|(l, _)| *l);
    for (l, a) in &levels {
        let total_e = a.puts + a.deletes;
        let del_pct = if total_e > 0 {
            100.0 * a.deletes as f64 / total_e as f64
        } else {
            0.0
        };
        println!(
            "{:<6} {:>6} {:>9} {:>13} {:>9} {:>12} {:>9} {:>8.2}%",
            format!("L{l}"),
            a.files,
            a.file_bytes / 1024 / 1024,
            a.puts,
            a.put_bytes / 1024 / 1024,
            a.deletes,
            a.delete_bytes / 1024,
            del_pct
        );
    }

    println!("\n===== per-class (top 30 by delete bytes) =====");
    println!(
        "{:<36} {:>6} {:>8} {:>13} {:>10} {:>11} {:>7}",
        "class", "files", "putMB", "puts", "delKB", "deletes", "del%"
    );
    let mut classes: Vec<_> = by_class.into_iter().collect();
    classes.sort_by_key(|(_, a)| std::cmp::Reverse(a.delete_bytes));
    for (name, a) in classes.iter().take(30) {
        let total_e = a.puts + a.deletes;
        let del_pct = if total_e > 0 {
            100.0 * a.deletes as f64 / total_e as f64
        } else {
            0.0
        };
        println!(
            "{:<36} {:>6} {:>8} {:>13} {:>10} {:>11} {:>6.2}%",
            name,
            a.files,
            a.put_bytes / 1024 / 1024,
            a.puts,
            a.delete_bytes / 1024,
            a.deletes,
            del_pct
        );
    }

    let mut grand = Agg::default();
    for (_, a) in &levels {
        grand.files += a.files;
        grand.file_bytes += a.file_bytes;
        grand.puts += a.puts;
        grand.deletes += a.deletes;
        grand.put_bytes += a.put_bytes;
        grand.delete_bytes += a.delete_bytes;
    }
    println!(
        "\n[total] files={} diskMB={} puts={} deletes={} ({:.2}%) deleteKB={} putMB={} skipped_files={}",
        grand.files,
        grand.file_bytes / 1024 / 1024,
        grand.puts,
        grand.deletes,
        if grand.puts + grand.deletes > 0 {
            100.0 * grand.deletes as f64 / (grand.puts + grand.deletes) as f64
        } else {
            0.0
        },
        grand.delete_bytes / 1024,
        grand.put_bytes / 1024 / 1024,
        skipped
    );
    if shadow {
        let total_e = grand.puts + grand.deletes;
        println!(
            "[shadow] puts whose key is tombstoned: {} entries, {} MB (~{:.1}% of put bytes) [+~3% bloom FP]",
            shadowed_puts,
            shadowed_put_bytes / 1024 / 1024,
            if grand.put_bytes > 0 {
                100.0 * shadowed_put_bytes as f64 / grand.put_bytes as f64
            } else {
                0.0
            }
        );
        println!(
            "[shadow] reclaimable estimate ≈ shadowed puts + tombstone entries = {} MB",
            (shadowed_put_bytes + grand.delete_bytes) / 1024 / 1024
        );
        let _ = total_e;
    }
    println!("[census] done in {:.0}s", t0.elapsed().as_secs_f64());
}
