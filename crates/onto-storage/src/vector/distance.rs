//! Distance metrics for vector similarity search.

/// Distance metric used for vector comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DistanceMetric {
    /// Euclidean (L2) distance. Lower is more similar.
    L2,
    /// Cosine distance. Lower is more similar (1 - cosine_similarity).
    Cosine,
    /// Inner product (dot product). Higher is more similar (negated for min-heap).
    InnerProduct,
}

/// Computes the distance between two vectors using the given metric.
/// Returns a f32 where LOWER means MORE similar (for L2 and Cosine)
/// or NEGATIVE means MORE similar (for InnerProduct, negated dot product).
pub fn distance(a: &[f32], b: &[f32], metric: DistanceMetric) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "vector dimensions must match");
    match metric {
        DistanceMetric::L2 => l2_distance(a, b),
        DistanceMetric::Cosine => cosine_distance(a, b),
        DistanceMetric::InnerProduct => inner_product_distance(a, b),
    }
}

/// L2 (Euclidean) distance: sqrt(sum((a_i - b_i)^2))
/// Optimized with loop unrolling for better SIMD auto-vectorization.
fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len();
    let mut sum = 0.0f32;
    let mut i = 0;

    // Process 4 elements at a time (SIMD-friendly)
    while i + 4 <= len {
        let d0 = a[i] - b[i];
        let d1 = a[i+1] - b[i+1];
        let d2 = a[i+2] - b[i+2];
        let d3 = a[i+3] - b[i+3];
        sum += d0*d0 + d1*d1 + d2*d2 + d3*d3;
        i += 4;
    }

    // Handle remaining elements
    while i < len {
        let diff = a[i] - b[i];
        sum += diff * diff;
        i += 1;
    }

    sum.sqrt()
}

/// Cosine distance: 1 - (a . b) / (||a|| * ||b||)
/// Optimized with loop unrolling.
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len();
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    let mut i = 0;

    // Process 4 elements at a time
    while i + 4 <= len {
        let (a0, a1, a2, a3) = (a[i], a[i+1], a[i+2], a[i+3]);
        let (b0, b1, b2, b3) = (b[i], b[i+1], b[i+2], b[i+3]);
        dot += a0*b0 + a1*b1 + a2*b2 + a3*b3;
        norm_a += a0*a0 + a1*a1 + a2*a2 + a3*a3;
        norm_b += b0*b0 + b1*b1 + b2*b2 + b3*b3;
        i += 4;
    }

    // Handle remaining
    while i < len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
        i += 1;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        return 1.0;
    }
    1.0 - (dot / denom)
}

/// Inner product distance: -(a . b) (negated so lower = more similar)
fn inner_product_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
    }
    -dot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_l2_distance() {
        let a = [1.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];
        let d = distance(&a, &b, DistanceMetric::L2);
        assert!((d - 2.0f32.sqrt()).abs() < 1e-6);

        // Same vector → distance 0
        let d = distance(&a, &a, DistanceMetric::L2);
        assert!(d.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_distance() {
        let a = [1.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];
        let d = distance(&a, &b, DistanceMetric::Cosine);
        assert!((d - 1.0).abs() < 1e-6); // Orthogonal → cosine distance = 1

        // Same direction → cosine distance = 0
        let c = [2.0, 0.0, 0.0];
        let d = distance(&a, &c, DistanceMetric::Cosine);
        assert!(d.abs() < 1e-6);
    }

    #[test]
    fn test_inner_product_distance() {
        let a = [1.0, 2.0, 3.0];
        let b = [4.0, 5.0, 6.0];
        let d = distance(&a, &b, DistanceMetric::InnerProduct);
        assert!((d - (-32.0)).abs() < 1e-6); // -(1*4 + 2*5 + 3*6) = -32
    }
}
