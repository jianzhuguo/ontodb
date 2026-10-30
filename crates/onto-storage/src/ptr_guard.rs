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

/// True when `(ptr, cap, len)` can describe real, readable bytes.
///
/// Covers the impossible allocation shapes seen in the heap-corruption
/// crashes:
/// - `cap == 0 && len > 0` — dangling `Vec::new()` pointer carrying data;
/// - `cap > 0` but `ptr` is a sentinel / non-canonical value — the 19420
///   crash shape `(ptr=0x1, cap=8, len=6)`, which passed every `cap == 0`
///   guard and went straight into `memcpy` on address `0x1`;
/// - `cap == usize::MAX` — the `None`-marker encoding that a naive
///   `len <= cap` check accepts;
/// - `len > cap`.
///
/// Empty (`len == 0`) is always coherent: nothing is ever dereferenced, and
/// `Vec::new()`'s dangling `0x1` pointer is legal in that state.
pub fn vec_header_coherent(ptr: usize, cap: usize, len: usize) -> bool {
    if len == 0 {
        return true;
    }
    if cap == usize::MAX || len > cap {
        return false;
    }
    // Implies canonical x86-64: the band is strictly below 2^47, so bits
    // 63:48 are zero and the sign-extension check can never trip.
    is_plausible_heap_ptr(ptr)
}

/// Bytes of `v`, panicking with a snapshot when the Vec header is in an
/// impossible state.
///
/// Per the fail-fast ruling (2026-10-28): corrupt-buffer guards must panic
/// with a snapshot rather than degrade to `&[]`. An empty key/value silently
/// emitted into compaction output reorders or wrongly dedups rows and poisons
/// SSTables — worse for a database than a crash. A legitimate empty `Vec`
/// (including `Vec::new()`) still reads as an empty slice.
#[track_caller]
pub fn checked_vec<'a>(v: &'a Vec<u8>, what: &str, ctx: &str) -> &'a [u8] {
    let (ptr, cap, len) = (v.as_ptr() as usize, v.capacity(), v.len());
    if !vec_header_coherent(ptr, cap, len) {
        panic!(
            "{what}: corrupt Vec header ({ctx}) ptr={ptr:#x} cap={cap} len={len} \
             canonical={} plausible={} — refusing to read",
            matches!(ptr >> 47, 0 | 0x1ffff),
            is_plausible_heap_ptr(ptr),
        );
    }
    v.as_slice()
}

/// Bytes of a slice, panicking with a snapshot when a non-empty slice sits at
/// an implausible address (sentinel `0x1`, small integer, non-canonical).
#[track_caller]
pub fn checked_slice<'a>(s: &'a [u8], what: &str, ctx: &str) -> &'a [u8] {
    let (ptr, len) = (s.as_ptr() as usize, s.len());
    if len > 0 && !is_plausible_heap_ptr(ptr) {
        panic!(
            "{what}: corrupt slice ({ctx}) ptr={ptr:#x} len={len} — refusing to read",
        );
    }
    s
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

    #[test]
    fn header_coherent_accepts_legal_shapes() {
        // Vec::new(): dangling ptr is fine when nothing is read.
        assert!(vec_header_coherent(0x1, 0, 0));
        // Real allocation.
        assert!(vec_header_coherent(0x16E32D71900, 8, 6));
    }

    #[test]
    fn header_coherent_rejects_smashed_shapes() {
        // The 19420 crash shape: sentinel ptr with live-looking cap/len.
        assert!(!vec_header_coherent(0x1, 8, 6));
        // Non-canonical pointer (15936 crash shape).
        assert!(!vec_header_coherent(0xb78d20964436c2b5, 0x5e, 0x5e));
        // cap == 0 with data.
        assert!(!vec_header_coherent(0x1, 0, 6));
        // None-marker capacity.
        assert!(!vec_header_coherent(0x16E32D71900, usize::MAX, 1));
        // len > cap.
        assert!(!vec_header_coherent(0x16E32D71900, 4, 8));
    }
}
