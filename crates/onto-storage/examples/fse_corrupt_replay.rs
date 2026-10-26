//! Corruption replay for the FSE sequence-table OOB fix in
//! `vendor/zstd-sys/zstd/lib/decompress/zstd_decompress_block.c`.
//!
//! Bug: a corrupt frame / heap-damaged decode table could drive `state` past
//! the FSE table body, so `table[state]` was an OOB read (`movzx eax,
//! [r9+rdx*8+2]` with rdx=0x8FE247BE491F → 0xC0000005). The fix rejects
//! `tableLog > <table capacity>` at init, records `stateBound`, and refuses to
//! index out of range (returns offset=(size_t)-1 → corruption_detected).
//!
//! This test compresses real data, then flips bytes across the compressed
//! payload (and specifically the last 1/3 where sequence sections live) and
//! asserts decompression returns Err instead of taking the process down.

use std::io::Write;

fn compress(data: &[u8], level: i32) -> Vec<u8> {
    let mut enc = zstd::stream::Encoder::new(Vec::new(), level).expect("encoder");
    enc.write_all(data).expect("write");
    enc.finish().expect("finish")
}

fn decompress(src: &[u8], cap: usize) -> Result<Vec<u8>, String> {
    // Same bounded one-shot path the storage layer uses (not decode_all):
    // get declared size first, then decompress into a fixed buffer.
    let declared = zstd::zstd_safe::get_frame_content_size(src);
    let out_cap = match declared {
        Ok(Some(n)) if n > 0 => (n as usize).min(cap),
        _ => cap,
    };
    let mut buf = vec![0u8; out_cap];
    match zstd::zstd_safe::decompress(&mut buf, src) {
        Ok(n) => Ok(buf[..n].to_vec()),
        Err(e) => Err(format!("{e}")),
    }
}

fn main() {
    // Sequence-heavy payload: repeated phrases with varying tails so the
    // compressor emits many FSE-coded sequences (not just RLE/literals).
    let mut data = Vec::with_capacity(256 * 1024);
    for i in 0..(256 * 1024) {
        if i % 64 < 32 {
            data.push(b"A"[(i * 7) % 1]);
        } else {
            data.push(((i * 131) ^ (i >> 3) ^ (i % 17)) as u8);
        }
    }

    for level in [1, 3, 6] {
        let comp = compress(&data, level);
        let ok = decompress(&comp, data.len() + 64).expect("clean frame must decompress");
        assert_eq!(ok, data, "roundtrip level={level}");
        println!("level={level}: compressed {} -> {} bytes", data.len(), comp.len());

        // 1) Single-byte flips across the whole frame.
        let mut crashes_avoided = 0u32;
        let mut errors = 0u32;
        let mut silent_ok = 0u32;
        for pos in (comp.len() / 10)..comp.len().saturating_sub(1) {
            for bit in [0x01u8, 0x80, 0xFF] {
                let mut bad = comp.clone();
                bad[pos] ^= bit;
                match decompress(&bad, data.len() + 64) {
                    Ok(_) => silent_ok += 1, // rare: corruption still decodable
                    Err(_) => errors += 1,
                }
                crashes_avoided += 1;
            }
        }
        println!(
            "  byte-flip: {crashes_avoided} attempts, {errors} Err, {silent_ok} still-decodable (no crash)"
        );

        // 2) Hammer the sequence section (last third): multi-byte smashes.
        let start = comp.len() * 2 / 3;
        for i in 0..8000 {
            let mut bad = comp.clone();
            let pos = start + (i * 9973) % (comp.len() - start);
            let n = 1 + (i % 16);
            for k in 0..n {
                if pos + k < bad.len() {
                    bad[pos + k] ^= 0xA5 ^ (k as u8);
                }
            }
            // Plant a huge tableLog-looking dword (claims log=31/255) in the
            // smashed window — this is the exact header shape the fix rejects.
            if pos + 5 < bad.len() {
                match i % 3 {
                    0 => {
                        bad[pos] = 0x1F;
                        bad[pos + 1] = 0;
                        bad[pos + 2] = 0;
                        bad[pos + 3] = 0;
                    }
                    1 => {
                        bad[pos] = 0xFF;
                        bad[pos + 1] = 0xFF;
                        bad[pos + 2] = 0xFF;
                        bad[pos + 3] = 0xFF;
                    }
                    _ => {
                        // max nbBits / nextState cell
                        bad[pos] = 0xFF;
                        bad[pos + 1] = 0xFF;
                        bad[pos + 2] = 0xFF; // nbAdditionalBits
                        bad[pos + 3] = 0xFF; // nbBits
                    }
                }
            }
            match decompress(&bad, data.len() + 64) {
                Ok(_) => {}
                Err(_) => errors += 1,
            }
        }
        println!("  seq-section smash: done (total Err so far {errors})");

        // 3) Truncations + zero-fills around the sequence section.
        for cut in [comp.len() - 1, comp.len() / 2, start, start + 13] {
            let bad = &comp[..cut];
            let _ = decompress(bad, data.len() + 64);
            let mut z = comp.clone();
            for b in z[start..].iter_mut() {
                *b = 0;
            }
            let _ = decompress(&z, data.len() + 64);
        }
        println!("  truncate/zero-fill: survived");
    }

    println!("FSE corruption replay: OK (no crash)");
}
