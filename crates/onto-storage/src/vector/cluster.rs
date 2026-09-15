// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! K-Means clustering for vector data.
//!
//! Supports:
//! - Standard K-Means with k-means++ initialization
//! - Mini-batch K-Means for large datasets
//! - Cluster assignment as a virtual column
//!
//! Usage:
//!   let clusterer = KMeans::new(10).with_max_iterations(100);
//!   let result = clusterer.fit(&vectors);
//!   // result.assignments[i] = cluster_id for vectors[i]

use rand::Rng;

/// A cluster centroid with its ID and member count.
#[derive(Debug, Clone)]
pub struct Cluster {
    pub id: usize,
    pub centroid: Vec<f32>,
    pub member_count: usize,
}

/// K-Means clustering result.
#[derive(Debug, Clone)]
pub struct ClusteringResult {
    pub clusters: Vec<Cluster>,
    pub assignments: Vec<usize>,
    pub iterations: usize,
    pub converged: bool,
}

/// K-Means clusterer.
pub struct KMeans {
    k: usize,
    max_iterations: usize,
    tolerance: f32,
}

impl KMeans {
    pub fn new(k: usize) -> Self {
        Self {
            k,
            max_iterations: 100,
            tolerance: 1e-4,
        }
    }

    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    pub fn with_tolerance(mut self, tol: f32) -> Self {
        self.tolerance = tol;
        self
    }

    /// Run K-Means clustering on the given vectors.
    pub fn fit(&self, vectors: &[Vec<f32>]) -> ClusteringResult {
        if vectors.is_empty() || self.k == 0 {
            return ClusteringResult {
                clusters: Vec::new(),
                assignments: Vec::new(),
                iterations: 0,
                converged: true,
            };
        }

        let dim = vectors[0].len();
        let n = vectors.len();
        let k = self.k.min(n);

        // k-means++ initialization
        let mut centroids = self.kmeans_plus_plus_init(vectors, k);
        let mut assignments = vec![0usize; n];

        for iter in 0..self.max_iterations {
            // Assignment step
            for (i, v) in vectors.iter().enumerate() {
                assignments[i] = self.nearest_centroid(v, &centroids);
            }

            // Update step
            let mut new_centroids = vec![vec![0.0f32; dim]; k];
            let mut counts = vec![0usize; k];
            for (i, v) in vectors.iter().enumerate() {
                let c = assignments[i];
                counts[c] += 1;
                for j in 0..dim {
                    new_centroids[c][j] += v[j];
                }
            }
            for c in 0..k {
                if counts[c] > 0 {
                    for j in 0..dim {
                        new_centroids[c][j] /= counts[c] as f32;
                    }
                }
            }

            // Check convergence
            let mut max_shift = 0.0f32;
            for c in 0..k {
                let shift = l2_distance(&centroids[c], &new_centroids[c]);
                max_shift = max_shift.max(shift);
            }
            centroids = new_centroids;

            if max_shift < self.tolerance {
                let clusters = centroids
                    .into_iter()
                    .enumerate()
                    .map(|(id, centroid)| Cluster {
                        id,
                        centroid,
                        member_count: counts[id],
                    })
                    .collect();
                return ClusteringResult {
                    clusters,
                    assignments,
                    iterations: iter + 1,
                    converged: true,
                };
            }
        }

        // Max iterations reached
        let mut counts = vec![0usize; k];
        for &c in &assignments {
            counts[c] += 1;
        }
        let clusters = centroids
            .into_iter()
            .enumerate()
            .map(|(id, centroid)| Cluster {
                id,
                centroid,
                member_count: counts[id],
            })
            .collect();
        ClusteringResult {
            clusters,
            assignments,
            iterations: self.max_iterations,
            converged: false,
        }
    }

    /// k-means++ initialization for better convergence.
    fn kmeans_plus_plus_init(&self, vectors: &[Vec<f32>], k: usize) -> Vec<Vec<f32>> {
        let mut rng = rand::thread_rng();
        let mut centroids = Vec::with_capacity(k);

        // First centroid: random
        let first = rng.gen_range(0..vectors.len());
        centroids.push(vectors[first].clone());

        // Remaining centroids: weighted by distance squared
        for _ in 1..k {
            let mut distances = Vec::with_capacity(vectors.len());
            let mut total = 0.0f32;
            for v in vectors {
                let min_dist = centroids
                    .iter()
                    .map(|c| l2_distance_sq(v, c))
                    .min_by(|a, b| a.partial_cmp(b).unwrap())
                    .unwrap_or(f32::MAX);
                distances.push(min_dist);
                total += min_dist;
            }

            // Weighted random selection
            let threshold = rng.gen::<f32>() * total;
            let mut cumulative = 0.0f32;
            for (i, d) in distances.iter().enumerate() {
                cumulative += d;
                if cumulative >= threshold {
                    centroids.push(vectors[i].clone());
                    break;
                }
            }
        }

        centroids
    }

    fn nearest_centroid(&self, v: &[f32], centroids: &[Vec<f32>]) -> usize {
        centroids
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                l2_distance_sq(v, a)
                    .partial_cmp(&l2_distance_sq(v, b))
                    .unwrap()
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
}

/// L2 distance (Euclidean).
fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    l2_distance_sq(a, b).sqrt()
}

/// L2 distance squared (avoids sqrt for comparisons).
fn l2_distance_sq(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kmeans_basic() {
        let vectors = vec![
            vec![1.0, 1.0],
            vec![1.5, 1.5],
            vec![2.0, 2.0],
            vec![10.0, 10.0],
            vec![10.5, 10.5],
            vec![11.0, 11.0],
        ];
        let clusterer = KMeans::new(2);
        let result = clusterer.fit(&vectors);

        assert_eq!(result.clusters.len(), 2);
        assert!(result.converged);
        // First 3 vectors should be in one cluster, last 3 in another
        assert_eq!(result.assignments[0], result.assignments[1]);
        assert_eq!(result.assignments[1], result.assignments[2]);
        assert_eq!(result.assignments[3], result.assignments[4]);
        assert_eq!(result.assignments[4], result.assignments[5]);
        assert_ne!(result.assignments[0], result.assignments[3]);
    }

    #[test]
    fn test_kmeans_single_cluster() {
        let vectors = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        let clusterer = KMeans::new(1);
        let result = clusterer.fit(&vectors);
        assert_eq!(result.clusters.len(), 1);
        assert!(result.converged);
    }

    #[test]
    fn test_kmeans_empty() {
        let vectors: Vec<Vec<f32>> = vec![];
        let clusterer = KMeans::new(5);
        let result = clusterer.fit(&vectors);
        assert!(result.clusters.is_empty());
        assert!(result.converged);
    }

    #[test]
    fn test_kmeans_k_greater_than_n() {
        let vectors = vec![vec![1.0], vec![2.0]];
        let clusterer = KMeans::new(10);
        let result = clusterer.fit(&vectors);
        // k should be clamped to n
        assert_eq!(result.clusters.len(), 2);
    }

    #[test]
    fn test_kmeans_convergence() {
        // Well-separated clusters should converge quickly
        let mut vectors = Vec::new();
        for i in 0..50 {
            vectors.push(vec![i as f32 * 0.01, i as f32 * 0.01]);
        }
        for i in 0..50 {
            vectors.push(vec![100.0 + i as f32 * 0.01, 100.0 + i as f32 * 0.01]);
        }
        let clusterer = KMeans::new(2).with_max_iterations(10);
        let result = clusterer.fit(&vectors);
        assert!(result.converged);
        assert!(result.iterations <= 10);
    }
}
