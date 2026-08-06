//! Distance metrics for vector similarity search.

/// Distance metric used for vector comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        let diff = a[i] - b[i];
        sum += diff * diff;
    }
    sum.sqrt()
}

/// Cosine distance: 1 - (a . b) / (||a|| * ||b||)
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        return 1.0; // Maximum distance for zero vectors
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
