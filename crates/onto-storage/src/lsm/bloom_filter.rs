// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Bloom filter for efficient point lookups.
//!
//! A probabilistic data structure that tells us if a key is
//! definitely NOT in a set, or PROBABLY in the set.
//! This avoids expensive disk reads for keys that don't exist.

/// A simple bloom filter implementation.
pub struct BloomFilter {
    bits: Vec<u64>,
    num_hashes: usize,
    num_bits: usize,
}

impl BloomFilter {
    /// Creates a new bloom filter.
    ///
    /// - `expected_items`: Expected number of items to insert.
    /// - `fp_rate`: Desired false positive rate (e.g., 0.01 for 1%).
    pub fn new(expected_items: usize, fp_rate: f64) -> Self {
        // Optimal number of bits: -n * ln(p) / (ln(2)^2)
        let num_bits = Self::optimal_bits(expected_items, fp_rate).max(64);
        // Optimal number of hash functions: (m/n) * ln(2)
        let num_hashes = Self::optimal_hashes(num_bits, expected_items).max(1);

        let num_u64 = num_bits.div_ceil(64);
        Self {
            bits: vec![0u64; num_u64],
            num_hashes,
            num_bits,
        }
    }

    /// Inserts a key into the bloom filter.
    pub fn insert(&mut self, key: &[u8]) {
        for i in 0..self.num_hashes {
            let hash = self.hash(key, i);
            let bit_idx = (hash as usize) % self.num_bits;
            let word_idx = bit_idx / 64;
            let bit_offset = bit_idx % 64;
            self.bits[word_idx] |= 1u64 << bit_offset;
        }
    }

    /// Tests if a key might be in the set.
    ///
    /// Returns `false` if the key is definitely NOT in the set.
    /// Returns `true` if the key MIGHT be in the set (with false positive probability).
    pub fn might_contain(&self, key: &[u8]) -> bool {
        for i in 0..self.num_hashes {
            let hash = self.hash(key, i);
            let bit_idx = (hash as usize) % self.num_bits;
            let word_idx = bit_idx / 64;
            let bit_offset = bit_idx % 64;
            if self.bits[word_idx] & (1u64 << bit_offset) == 0 {
                return false;
            }
        }
        true
    }

    /// Serializes the bloom filter to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + self.bits.len() * 8);
        buf.extend_from_slice(&(self.num_hashes as u32).to_le_bytes());
        buf.extend_from_slice(&(self.num_bits as u32).to_le_bytes());
        for &word in &self.bits {
            buf.extend_from_slice(&word.to_le_bytes());
        }
        buf
    }

    /// Deserializes a bloom filter from bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let num_hashes =
            u32::from_le_bytes(data[0..4].try_into().expect("should be valid")) as usize;
        let num_bits = u32::from_le_bytes(data[4..8].try_into().expect("should be valid")) as usize;
        // Cap allocation size to prevent OOM from malformed data (max ~128MB)
        if num_bits > 128 * 1024 * 1024 * 8 {
            return None;
        }
        let num_u64 = num_bits.div_ceil(64);

        if data.len() < 8 + num_u64 * 8 {
            return None;
        }

        let mut bits = Vec::with_capacity(num_u64);
        for i in 0..num_u64 {
            let start = 8 + i * 8;
            let word =
                u64::from_le_bytes(data[start..start + 8].try_into().expect("should be valid"));
            bits.push(word);
        }

        Some(Self {
            bits,
            num_hashes,
            num_bits,
        })
    }

    fn hash(&self, key: &[u8], hash_index: usize) -> u64 {
        // Double hashing: h(i) = h1 + i * h2
        let h1 = Self::fnv_hash(key);
        let h2 = Self::murmur_hash(key);
        h1.wrapping_add((hash_index as u64).wrapping_mul(h2))
    }

    fn fnv_hash(key: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
        for &byte in key {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3); // FNV prime
        }
        hash
    }

    fn murmur_hash(key: &[u8]) -> u64 {
        let mut h: u64 = 0x5bd1e995;
        for &byte in key {
            h ^= byte as u64;
            h = h.wrapping_mul(0x5bd1e995);
            h ^= h >> 15;
        }
        h
    }

    fn optimal_bits(n: usize, p: f64) -> usize {
        let ln2_sq = std::f64::consts::LN_2 * std::f64::consts::LN_2;
        (-(n as f64) * p.ln() / ln2_sq).ceil() as usize
    }

    fn optimal_hashes(m: usize, n: usize) -> usize {
        if n == 0 {
            return 1;
        }
        ((m as f64 / n as f64) * std::f64::consts::LN_2).ceil() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_filter_basic() {
        let mut bf = BloomFilter::new(1000, 0.01);

        bf.insert(b"hello");
        bf.insert(b"world");
        bf.insert(b"rust");

        assert!(bf.might_contain(b"hello"));
        assert!(bf.might_contain(b"world"));
        assert!(bf.might_contain(b"rust"));
        // Very unlikely to false positive on a small set
        // (but not impossible, hence "might_contain")
    }

    #[test]
    fn test_bloom_filter_serialization() {
        let mut bf = BloomFilter::new(100, 0.01);
        bf.insert(b"test");
        bf.insert(b"data");

        let bytes = bf.to_bytes();
        let bf2 = BloomFilter::from_bytes(&bytes).expect("should be valid");

        assert!(bf2.might_contain(b"test"));
        assert!(bf2.might_contain(b"data"));
    }
}
