// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Vector normalization for optimized distance computation.
//!
//! When cosine distance is used, we L2-normalize each vector on insert.
//! At search time, cosine_distance(a, b) = 1 - dot(a, b), which is
//! just a dot product — no sqrt, no norm computation.
//!
//! This gives ~2x speedup for cosine search on high-dimensional vectors.

use crate::vector::distance::DistanceMetric;

/// L2-normalize a vector in-place.
///
/// Divides each component by the L2 norm (Euclidean length).
/// Zero vectors are left unchanged.
pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Check if a metric benefits from pre-normalization.
pub fn should_normalize(metric: &DistanceMetric) -> bool {
    matches!(metric, DistanceMetric::Cosine)
}

/// Distance between pre-normalized vectors (cosine = 1 - dot).
///
/// Precondition: both vectors must be L2-normalized.
/// Returns a value in [0, 2] where 0 = identical, 2 = opposite.
pub fn normalized_cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    1.0 - a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l2_normalize_unit_vector() {
        let mut v = vec![1.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert!((v[0] - 1.0).abs() < 1e-6);
        assert!((v[1] - 0.0).abs() < 1e-6);
        assert!((v[2] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_l2_normalize_non_unit() {
        let mut v = vec![3.0, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn test_l2_normalize_zero_vector() {
        let mut v = vec![0.0, 0.0, 0.0];
        l2_normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_normalized_cosine_distance_identical() {
        let mut a = vec![1.0, 0.0, 0.0];
        l2_normalize(&mut a);
        let d = normalized_cosine_distance(&a, &a);
        assert!((d - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalized_cosine_distance_orthogonal() {
        let mut a = vec![1.0, 0.0];
        let mut b = vec![0.0, 1.0];
        l2_normalize(&mut a);
        l2_normalize(&mut b);
        let d = normalized_cosine_distance(&a, &b);
        assert!((d - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalized_cosine_distance_opposite() {
        let mut a = vec![1.0, 0.0];
        let mut b = vec![-1.0, 0.0];
        l2_normalize(&mut a);
        l2_normalize(&mut b);
        let d = normalized_cosine_distance(&a, &b);
        assert!((d - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_should_normalize_cosine() {
        assert!(should_normalize(&DistanceMetric::Cosine));
        assert!(!should_normalize(&DistanceMetric::L2));
        assert!(!should_normalize(&DistanceMetric::InnerProduct));
    }

    #[test]
    fn test_normalized_distance_matches_raw_cosine() {
        use crate::vector::distance::{distance, DistanceMetric};

        let a = vec![0.3, 0.4, 0.5, 0.6, 0.7];
        let b = vec![0.1, 0.2, 0.8, 0.9, 0.3];

        let raw = distance(&a, &b, DistanceMetric::Cosine);

        let mut na = a.clone();
        let mut nb = b.clone();
        l2_normalize(&mut na);
        l2_normalize(&mut nb);
        let normalized = normalized_cosine_distance(&na, &nb);

        assert!(
            (raw - normalized).abs() < 1e-5,
            "raw={}, normalized={}",
            raw,
            normalized
        );
    }
}
