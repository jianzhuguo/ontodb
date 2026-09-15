// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Shared utility functions for parsers.

/// Find a substring case-insensitively (ASCII only).
/// Returns the byte position in `haystack` where `needle` first occurs,
/// or None if not found. This avoids `to_uppercase()` index misalignment
/// with non-ASCII characters.
pub fn find_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let needle_upper: Vec<u8> = needle.bytes().map(|b| b.to_ascii_uppercase()).collect();
    let hay_bytes = haystack.as_bytes();
    let nlen = needle_upper.len();
    if hay_bytes.len() < nlen {
        return None;
    }
    'outer: for i in 0..=hay_bytes.len() - nlen {
        for j in 0..nlen {
            if hay_bytes[i + j].to_ascii_uppercase() != needle_upper[j] {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

/// Check if `s` starts with `prefix` case-insensitively (ASCII only).
/// Avoids allocating a new String via `to_uppercase()`.
pub fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len()
        && s.as_bytes()[..prefix.len()]
            .iter()
            .zip(prefix.as_bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Check if `s` ends with `suffix` case-insensitively (ASCII only).
pub fn ends_with_ignore_ascii_case(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len()
        && s.as_bytes()[s.len() - suffix.len()..]
            .iter()
            .zip(suffix.as_bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Safe slice from start to end, returning empty string if out of bounds.
pub fn safe_slice(s: &str, start: usize, end: usize) -> &str {
    if start >= s.len() || end > s.len() || start >= end {
        ""
    } else {
        &s[start..end]
    }
}

/// Safe slice from start to end of string, returning empty string if out of bounds.
pub fn safe_slice_from(s: &str, start: usize) -> &str {
    if start >= s.len() {
        ""
    } else {
        &s[start..]
    }
}

/// Trim trailing semicolons from a query string.
pub fn trim_semicolons(s: &str) -> &str {
    s.trim_end_matches(';').trim()
}
