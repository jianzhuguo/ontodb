//! Generate SST files with oversized bloom filters to exercise startup
//! memory behavior (lazy bloom/index loading) at scale without a real corpus.
//!
//! Each file carries a bloom sized for `bloom_expected` entries even though it
//! only contains `entries` real rows, so a few hundred files reproduce the
//! multi-GB eager materialization a 1300+ SST production directory causes.
//!
//! Usage:
//!   cargo run --release -p onto-storage --example gen_fat_ssts -- <dir> <count> [bloom_expected] [entries_per_sst]
use onto_core::Entry;
use onto_storage::lsm::sstable::SsTableBuilder;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "scratch_laze".into()));
    let count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(200);
    let bloom_expected: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3_000_000);
    let entries: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(16);

    std::fs::create_dir_all(&dir).expect("create dir");
    println!(
        "writing {} SSTs to {:?} (bloom expected_entries={}, {} rows each)",
        count, dir, bloom_expected, entries
    );

    let mut total = 0u64;
    for i in 0..count {
        let mut b = SsTableBuilder::with_expected_entries(bloom_expected);
        b.set_compression_level(1);
        for j in 0..entries {
            let key = format!("gen|{:06}|{:08}", i, j);
            b.add(&Entry::put(key.into_bytes(), vec![(j % 251) as u8; 96], 1));
        }
        let path = dir.join(format!("L1_{}.sst", i));
        b.build(&path).expect("build sst");
        total += std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if (i + 1) % 50 == 0 || i + 1 == count {
            println!("built {}/{} ({} MB on disk)", i + 1, count, total / 1024 / 1024);
        }
    }
    println!("done: {} files, {} MB", count, total / 1024 / 1024);
}
