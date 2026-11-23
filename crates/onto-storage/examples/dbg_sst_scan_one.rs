//! Scan ONE SST file with flushed phase markers, so a giant-alloc abort
//! pinpoints the failing file/entry position from the last printed line.
//!
//! Usage: dbg_sst_scan_one.exe <path>
//! Prints: START / OPEN ok / AT <n> (every 20k entries) / OK summary.

use std::io::Write;
use std::path::PathBuf;

use onto_core::types::EntryKind;
use onto_storage::lsm::streaming_merge::StreamingMergeIterator;

fn flush() {
    let _ = std::io::stdout().flush();
}

fn main() {
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: dbg_sst_scan_one <path>"));
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    println!("START {name}");
    flush();

    let mut iter = match StreamingMergeIterator::new(&[&path]) {
        Ok(it) => it,
        Err(e) => {
            println!("OPEN FAIL {name}: {e}");
            flush();
            std::process::exit(2);
        }
    };
    println!("OPEN ok {name}");
    flush();

    let mut n = 0u64;
    let mut dels = 0u64;
    let mut max_val = 0u64;
    let mut max_key = 0u64;
    let mut big_vals: Vec<(u64, String)> = Vec::new();

    while iter.has_next() {
        if n % 20_000 == 0 && n > 0 {
            println!("AT {n} dels={dels} maxval={max_val}");
            flush();
        }
        let Some(entry) = iter.next() else { break };
        n += 1;
        let vlen = entry.value.len() as u64;
        let klen = entry.key.len() as u64;
        if vlen > max_val {
            max_val = vlen;
            if vlen > 1_000_000 {
                let preview: String = String::from_utf8_lossy(&entry.key).chars().take(60).collect();
                println!("BIGVAL entry#{n} vlen={vlen} klen={klen} key={preview}");
                flush();
                big_vals.push((vlen, preview));
            }
        }
        if klen > max_key {
            max_key = klen;
        }
        if entry.kind == EntryKind::Delete {
            dels += 1;
        }
    }

    println!(
        "OK {name} entries={n} dels={dels} maxval={max_val} maxkey={max_key} bigvals={}",
        big_vals.len()
    );
    flush();
}
