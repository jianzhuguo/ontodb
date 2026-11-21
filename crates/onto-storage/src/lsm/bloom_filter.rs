// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
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
    ///
    /// The two base hashes depend only on `key`, so they are computed once and
    /// the k probes are derived by repeated addition (double hashing). The old
    /// implementation recomputed both hashes for every probe — at a 1% false
    /// positive rate that scanned the key 14 times per insert instead of once.
    pub fn insert(&mut self, key: &[u8]) {
        let (mut hash, step) = Self::base_hashes(key);
        for _ in 0..self.num_hashes {
            let (word, mask) = self.bit_mask(hash);
            self.bits[word] |= mask;
            // Equivalent to h1 + i*h2 mod 2^64, without the per-probe multiply.
            hash = hash.wrapping_add(step);
        }
    }

    /// Tests if a key might be in the set.
    ///
    /// Returns `false` if the key is definitely NOT in the set.
    /// Returns `true` if the key MIGHT be in the set (with false positive probability).
    pub fn might_contain(&self, key: &[u8]) -> bool {
        let (mut hash, step) = Self::base_hashes(key);
        for _ in 0..self.num_hashes {
            let (word, mask) = self.bit_mask(hash);
            if self.bits[word] & mask == 0 {
                return false;
            }
            hash = hash.wrapping_add(step);
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

    /// FNV-1a and the murmur-style mixer in one pass over `key`.
    ///
    /// Both hashes are independent, so interleaving their state updates gives
    /// bit-for-bit the same result as the two separate loops that existed
    /// before, at half the memory traffic.
    #[inline]
    fn base_hashes(key: &[u8]) -> (u64, u64) {
        let mut fnv = 0xcbf29ce484222325u64; // FNV offset basis
        let mut murmur = 0x5bd1e995u64;
        for &byte in key {
            fnv = (fnv ^ byte as u64).wrapping_mul(0x100000001b3); // FNV prime
            murmur ^= byte as u64;
            murmur = murmur.wrapping_mul(0x5bd1e995);
            murmur ^= murmur >> 15;
        }
        (fnv, murmur)
    }

    /// Maps a probe hash to a `(word index, bit mask)` pair.
    #[inline]
    fn bit_mask(&self, hash: u64) -> (usize, u64) {
        let bit_idx = (hash as usize) % self.num_bits;
        (bit_idx / 64, 1u64 << (bit_idx % 64))
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

    // The original implementations, kept as an oracle. The on-disk filter is
    // shared between versions, so the optimisation must not change a single
    // bit — otherwise old SSTables would report false negatives for new keys
    // and vice versa.
    fn legacy_fnv(key: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &byte in key {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    fn legacy_murmur(key: &[u8]) -> u64 {
        let mut h: u64 = 0x5bd1e995;
        for &byte in key {
            h ^= byte as u64;
            h = h.wrapping_mul(0x5bd1e995);
            h ^= h >> 15;
        }
        h
    }

    fn legacy_probe(bf: &BloomFilter, key: &[u8], i: usize) -> usize {
        let hash = legacy_fnv(key)
            .wrapping_add((i as u64).wrapping_mul(legacy_murmur(key)));
        ((hash as usize) % bf.num_bits)
    }

    fn legacy_insert(bf: &mut BloomFilter, key: &[u8]) {
        for i in 0..bf.num_hashes {
            let bit_idx = legacy_probe(bf, key, i);
            bf.bits[bit_idx / 64] |= 1u64 << (bit_idx % 64);
        }
    }

    fn legacy_might_contain(bf: &BloomFilter, key: &[u8]) -> bool {
        (0..bf.num_hashes).all(|i| {
            let bit_idx = legacy_probe(bf, key, i);
            bf.bits[bit_idx / 64] & (1u64 << (bit_idx % 64)) != 0
        })
    }

    fn sample_keys() -> Vec<Vec<u8>> {
        let mut keys = vec![Vec::new(), b"a".to_vec(), vec![0xff; 512]];
        for i in 0..256usize {
            let mut key = format!("user:{}:", i).into_bytes();
            key.extend(std::iter::repeat((i % 251) as u8).take(i % 37));
            keys.push(key);
        }
        keys
    }

    /// Guards both changes: fusing the two hash loops and replacing `i*h2`
    /// with repeated addition must leave the serialized bit array unchanged.
    #[test]
    fn optimized_probes_match_original_bit_for_bit() {
        let keys = sample_keys();
        for &(n, p) in &[
            (1usize, 0.01f64),
            (17, 0.05),
            (1000, 0.01),
            (100_000, 0.001),
        ] {
            let mut fast = BloomFilter::new(n, p);
            let mut legacy = BloomFilter::new(n, p);
            assert_eq!(fast.num_hashes, legacy.num_hashes);
            assert_eq!(fast.num_bits, legacy.num_bits);

            for key in &keys {
                fast.insert(key);
                legacy_insert(&mut legacy, key);
            }
            assert_eq!(
                fast.to_bytes(),
                legacy.to_bytes(),
                "on-disk bit pattern changed for n={n}, p={p}"
            );

            for key in &keys {
                assert_eq!(
                    fast.might_contain(key),
                    legacy_might_contain(&legacy, key),
                    "present-key lookup changed for n={n}, p={p}"
                );
            }
        }
    }

    /// False positives are part of the observable behaviour too: a key that
    /// the old filter rejected must still be rejected by the new one.
    #[test]
    fn absent_keys_keep_identical_lookup_results() {
        let present = sample_keys();
        let mut fast = BloomFilter::new(1000, 0.01);
        let mut legacy = BloomFilter::new(1000, 0.01);
        for key in &present {
            fast.insert(key);
            legacy_insert(&mut legacy, key);
        }

        for i in 0..2048u32 {
            let key = format!("absent-{i}").into_bytes();
            assert_eq!(
                fast.might_contain(&key),
                legacy_might_contain(&legacy, &key),
                "absent-key lookup changed for {i}"
            );
        }
    }
}
