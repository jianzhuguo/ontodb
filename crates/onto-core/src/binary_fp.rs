// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Binary payload helpers for BYTES-typed columns (fingerprints, embeddings).
//!
//! `DataType::Bytes` values are persisted as base64 strings inside JSON /
//! BinaryRow documents.  Bit-packed fingerprints are expanded back to
//! `f32` 0.0/1.0 vectors on the way into the HNSW index.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

/// Encodes raw bytes as the base64 string form used for `DataType::Bytes`.
pub fn bytes_to_b64(data: &[u8]) -> String {
    B64.encode(data)
}

/// Decodes a base64 string produced by [`bytes_to_b64`].
pub fn b64_to_bytes(s: &str) -> Option<Vec<u8>> {
    B64.decode(s.trim()).ok()
}

/// Expands bit-packed bytes to an `f32` 0.0/1.0 vector of `dim` bits.
///
/// Used to turn a compact binary fingerprint (e.g. 1024-bit Morgan → 128
/// bytes) into the float vector shape the HNSW index expects.  A mismatch
/// in bit length returns `None` so callers can skip rather than index a
/// wrong-sized vector.
pub fn packed_bits_to_f32(packed: &[u8], dim: usize) -> Option<Vec<f32>> {
    let need_bytes = dim.div_ceil(8);
    if packed.len() != need_bytes {
        return None;
    }
    let mut out = Vec::with_capacity(dim);
    for i in 0..dim {
        let byte = packed[i / 8];
        let bit = (byte >> (7 - (i % 8))) & 1;
        out.push(if bit == 1 { 1.0 } else { 0.0 });
    }
    Some(out)
}

/// Packs an `f32` 0.0/1.0 (or any non-zero) vector back into bit-packed bytes.
pub fn f32_to_packed_bits(vec: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; vec.len().div_ceil(8)];
    for (i, v) in vec.iter().enumerate() {
        if *v != 0.0 {
            out[i / 8] |= 1 << (7 - (i % 8));
        }
    }
    out
}

/// Parses a JSON value that holds either a float array (legacy) or a
/// base64 bit-packed string (Bytes) into an `f32` vector of `dim` entries.
pub fn json_to_f32_vec(val: &serde_json::Value, dim: usize) -> Option<Vec<f32>> {
    match val {
        serde_json::Value::Array(arr) => {
            let vec: Vec<f32> = arr
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
            if vec.len() == dim {
                Some(vec)
            } else {
                None
            }
        }
        serde_json::Value::String(s) => {
            let packed = b64_to_bytes(s)?;
            packed_bits_to_f32(&packed, dim)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn b64_roundtrip() {
        let data = vec![0u8, 1, 2, 255, 128];
        let enc = bytes_to_b64(&data);
        assert_eq!(b64_to_bytes(&enc).unwrap(), data);
    }

    #[test]
    fn bit_pack_roundtrip_1024() {
        let mut vec = vec![0.0f32; 1024];
        vec[0] = 1.0;
        vec[7] = 1.0;
        vec[8] = 1.0;
        vec[1023] = 1.0;
        let packed = f32_to_packed_bits(&vec);
        assert_eq!(packed.len(), 128);
        let back = packed_bits_to_f32(&packed, 1024).unwrap();
        assert_eq!(back, vec);
    }

    #[test]
    fn packed_size_mismatch_is_none() {
        let packed = vec![0u8; 16]; // 128 bits, not 1024
        assert!(packed_bits_to_f32(&packed, 1024).is_none());
    }

    #[test]
    fn json_accepts_both_shapes() {
        let arr = json!([1.0, 0.0, 1.0, 0.0]);
        assert_eq!(
            json_to_f32_vec(&arr, 4).unwrap(),
            vec![1.0, 0.0, 1.0, 0.0]
        );

        let packed = f32_to_packed_bits(&[1.0, 0.0, 1.0, 0.0]);
        let s = json!(bytes_to_b64(&packed));
        assert_eq!(
            json_to_f32_vec(&s, 4).unwrap(),
            vec![1.0, 0.0, 1.0, 0.0]
        );
    }
}
