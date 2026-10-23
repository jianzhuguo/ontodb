// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Heap-pointer and Vec sanity guards.
//!
//! A `Vec` with `len > 0 && cap == 0` is an impossible allocation (dangling
//! `ptr` like `0x1`). Dereferencing it takes the process down mid-compaction
//! or mid-flush. Same for non-heap pointers that show up where a node/child
//! pointer should be (observed as `rdi=0x80` AV in `persist_immutable_to_sst`).

/// A pointer that could be a live Rust heap allocation on this process.
///
/// Rejects null, `Vec::new()` dangling (`0x1`), small integers (lengths
/// mis-read as pointers), and non-canonical / kernel-looking values.
pub fn is_plausible_heap_ptr(p: usize) -> bool {
    // User-space heap on Windows x64 sits well above 64KB and below the
    // canonical high half. Keep the band wide to avoid false negatives.
    (0x10000..0x0000_7FFF_FFFF_FFFF).contains(&p)
}

/// `Vec` is safe to read if it is empty (no deref) or has a real allocation.
pub fn vec_is_safe(v: &[u8]) -> bool {
    v.is_empty() || is_plausible_heap_ptr(v.as_ptr() as usize)
}

/// True when a `Vec` has the impossible shape `len > 0 && cap == 0`.
///
/// `Vec::new()` is `(ptr=dangling, cap=0, len=0)` — safe. The dangerous
/// shape is `len > 0` with no allocation; `as_slice()` on it is UB.
pub fn vec_is_dangling(v: &Vec<u8>) -> bool {
    v.capacity() == 0 && !v.is_empty()
}

/// Validate a child / next node pointer before following it.
/// Returns `Err` with a short label when the pointer looks like a length,
/// a tag, or a dangling value rather than a heap node.
#[inline]
pub fn check_node_ptr(p: usize, what: &str) -> Result<usize, String> {
    if is_plausible_heap_ptr(p) {
        Ok(p)
    } else {
        Err(format!(
            "{what}: pointer 0x{p:X} is not a plausible heap node \
             (len/tag/small-int mis-read as pointer?)"
        ))
    }
}

/// Guard a `Vec` before cloning / slicing. Empty is fine; dangling is not.
#[inline]
pub fn check_vec(v: &Vec<u8>, what: &str) -> Result<(), String> {
    if vec_is_dangling(v) {
        return Err(format!(
            "{what}: dangling Vec (len={}, cap=0) — refusing to read",
            v.len()
        ));
    }
    if !v.is_empty() && !is_plausible_heap_ptr(v.as_ptr() as usize) {
        return Err(format!(
            "{what}: data ptr 0x{:X} is not a plausible heap pointer",
            v.as_ptr() as usize
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_small_and_dangling() {
        assert!(!is_plausible_heap_ptr(0x80));
        assert!(!is_plausible_heap_ptr(0x1));
        assert!(!is_plausible_heap_ptr(0x0));
        assert!(!is_plausible_heap_ptr(0x3D2));
        assert!(is_plausible_heap_ptr(0x16E32D71900));
    }

    #[test]
    fn empty_vec_is_safe() {
        let v: Vec<u8> = Vec::new();
        assert!(vec_is_safe(&v));
        assert!(!vec_is_dangling(&v));
        assert!(check_vec(&v, "t").is_ok());
    }

    #[test]
    fn real_vec_is_safe() {
        let v = vec![1u8, 2, 3];
        assert!(vec_is_safe(&v));
        assert!(check_vec(&v, "t").is_ok());
    }
}
