//! Diagnostic: which keys lose their tombstone in count_prefix_across_flush?
use onto_storage::engine::LsmEngine;
use onto_storage::options::StorageOptions;

fn main() {
    let dir = tempfile::tempdir().unwrap();
    let options = StorageOptions {
        data_dir: dir.path().to_path_buf(),
        memtable_size_limit: 256,
        ..Default::default()
    };
    let engine = LsmEngine::open(options).unwrap();

    for i in 0..100u32 {
        let key = format!("Item::{i:04}");
        engine.put(key.into_bytes(), vec![b'v'; 40]).unwrap();
    }
    for i in 0..10u32 {
        let key = format!("Item::{i:04}");
        engine.delete(key.into_bytes()).unwrap();
    }
    engine.flush().unwrap();
    // Quiesce background compaction so the LSM is stable.
    let _ = engine.flush_compaction();

    let (live, tombs) = engine.count_prefix(b"Item::").unwrap();
    println!("count_prefix live={live} tombs={tombs}");

    // Dump LSM shape via a full scan of one resurrected key's neighborhood.
    let mut resurrected = Vec::new();
    for i in 0..10u32 {
        let key = format!("Item::{i:04}");
        match engine.get(key.as_bytes()).unwrap() {
            Some(v) => {
                println!("  RESURRECTED get({key}) = {:?}", String::from_utf8_lossy(&v));
                resurrected.push(key);
            }
            None => println!("  ok deleted {key}"),
        }
    }
    println!("resurrected_by_get={}", resurrected.len());

    // Inspect on-disk SST inventory + per-key versions.
    use onto_storage::lsm::sstable::SsTable;
    let mut files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sst"))
        .collect();
    files.sort();
    println!("--- SST files ({}):", files.len());
    for f in &files {
        let meta = std::fs::metadata(f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        print!("  {name} size={}", meta.len());
        let sst = SsTable::open(f).unwrap();
        let mut hits = Vec::new();
        for i in 0..10u32 {
            let key = format!("Item::{i:04}");
            if let Ok(Some((_, _, kind))) = sst.get_full(key.as_bytes()) {
                hits.push(format!("{key}:{kind:?}"));
            }
        }
        println!(" hits=[{}]", hits.join(", "));
    }
}
