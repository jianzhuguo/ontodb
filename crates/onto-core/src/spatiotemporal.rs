//! Spatio-temporal index for OntoDB.
//!
//! Combines spatial indexing (quadtree) with temporal indexing (timeline)
//! for efficient queries like:
//! - "Find all events within 1km of this location AND within the last 24 hours"
//! - "Find all entities in this bounding box AND on this date"
//!
//! Design:
//! - Quadtree splits space into quadrants
//! - Each quadrant maintains a timeline (sorted by timestamp)
//! - Queries prune both spatially and temporally
//!
//! Target: spatio-temporal range queries > 50K QPS

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Configuration ──

/// Maximum entries per quadtree node.
const MAX_ENTRIES: usize = 50;
/// Maximum depth of the quadtree.
const MAX_DEPTH: usize = 12;

// ── Types ──

/// A spatio-temporal point: location + time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct STPoint {
    /// Entity ID.
    pub id: String,
    /// Longitude.
    pub lon: f64,
    /// Latitude.
    pub lat: f64,
    /// Timestamp (nanoseconds since Unix epoch).
    pub timestamp: i64,
}

/// A spatio-temporal bounding box + time range.
#[derive(Debug, Clone)]
pub struct STQuery {
    /// Spatial bounding box.
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
    /// Time range (nanoseconds).
    pub min_time: i64,
    pub max_time: i64,
}

// ── Quadtree Node ──

/// A node in the spatio-temporal quadtree.
#[derive(Debug)]
struct STNode {
    /// Bounding box of this node.
    min_lon: f64,
    min_lat: f64,
    max_lon: f64,
    max_lat: f64,
    /// Entries in this node (timeline sorted by timestamp).
    entries: BTreeMap<i64, Vec<STPoint>>,
    /// Child quadrants: [SW, SE, NW, NE].
    children: Option<Box<[STNode; 4]>>,
    /// Total entries in this subtree.
    count: usize,
}

impl STNode {
    fn new(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Self {
        Self {
            min_lon,
            min_lat,
            max_lon,
            max_lat,
            entries: BTreeMap::new(),
            children: None,
            count: 0,
        }
    }

    /// Check if a point is within this node's bounds.
    #[allow(dead_code)]
    fn contains(&self, lon: f64, lat: f64) -> bool {
        lon >= self.min_lon && lon <= self.max_lon
            && lat >= self.min_lat && lat <= self.max_lat
    }

    /// Check if this node's bounds overlap with a query box.
    fn overlaps(&self, q: &STQuery) -> bool {
        self.min_lon <= q.max_lon && self.max_lon >= q.min_lon
            && self.min_lat <= q.max_lat && self.max_lat >= q.min_lat
    }

    /// Subdivide this node into 4 children.
    fn subdivide(&mut self) {
        let mid_lon = (self.min_lon + self.max_lon) / 2.0;
        let mid_lat = (self.min_lat + self.max_lat) / 2.0;

        self.children = Some(Box::new([
            STNode::new(self.min_lon, self.min_lat, mid_lon, mid_lat),  // SW
            STNode::new(mid_lon, self.min_lat, self.max_lon, mid_lat),  // SE
            STNode::new(self.min_lon, mid_lat, mid_lon, self.max_lat),  // NW
            STNode::new(mid_lon, mid_lat, self.max_lon, self.max_lat),  // NE
        ]));
    }

    /// Determine which quadrant a point falls into (0-3).
    fn quadrant(&self, lon: f64, lat: f64) -> usize {
        let mid_lon = (self.min_lon + self.max_lon) / 2.0;
        let mid_lat = (self.min_lat + self.max_lat) / 2.0;
        match (lon >= mid_lon, lat >= mid_lat) {
            (false, false) => 0, // SW
            (true, false) => 1,  // SE
            (false, true) => 2,  // NW
            (true, true) => 3,   // NE
        }
    }
}

// ── Spatio-Temporal Index ──

/// Spatio-temporal index combining quadtree + timeline.
pub struct STIndex {
    root: STNode,
    total_count: usize,
}

impl STIndex {
    /// Create a new spatio-temporal index covering the given bounding box.
    pub fn new(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Self {
        Self {
            root: STNode::new(min_lon, min_lat, max_lon, max_lat),
            total_count: 0,
        }
    }

    /// Create a world-covering index (-180 to 180, -90 to 90).
    pub fn world() -> Self {
        Self::new(-180.0, -90.0, 180.0, 90.0)
    }

    /// Number of entries in the index.
    pub fn len(&self) -> usize {
        self.total_count
    }

    pub fn is_empty(&self) -> bool {
        self.total_count == 0
    }

    /// Insert a spatio-temporal point.
    pub fn insert(&mut self, point: STPoint) {
        Self::insert_into(&mut self.root, point, 0);
        self.total_count += 1;
    }

    /// Query: find all points within spatial bounds AND time range.
    pub fn query(&self, query: &STQuery) -> Vec<&STPoint> {
        let mut results = Vec::new();
        Self::query_node(&self.root, query, &mut results);
        results
    }

    /// Query: find all points within radius (meters) of a location AND time range.
    pub fn radius_query(
        &self,
        center_lon: f64,
        center_lat: f64,
        radius_meters: f64,
        min_time: i64,
        max_time: i64,
    ) -> Vec<&STPoint> {
        // Convert radius to approximate degrees
        let radius_lat = radius_meters / 111_000.0;
        let radius_lon = radius_meters / (111_000.0 * center_lat.to_radians().cos());

        let query = STQuery {
            min_lon: center_lon - radius_lon,
            min_lat: center_lat - radius_lat,
            max_lon: center_lon + radius_lon,
            max_lat: center_lat + radius_lat,
            min_time,
            max_time,
        };

        // Get candidates from bounding box
        let candidates = self.query(&query);

        // Filter by exact distance
        candidates
            .into_iter()
            .filter(|p| {
                let d = haversine_distance(center_lon, center_lat, p.lon, p.lat);
                d <= radius_meters
            })
            .collect()
    }

    /// Query: find K nearest points to a location within a time range.
    pub fn knn_query(
        &self,
        center_lon: f64,
        center_lat: f64,
        k: usize,
        min_time: i64,
        max_time: i64,
    ) -> Vec<(&STPoint, f64)> {
        let mut results: Vec<(&STPoint, f64)> = Vec::new();
        Self::knn_node(&self.root, center_lon, center_lat, k, min_time, max_time, &mut results);
        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        results
    }

    // ── Internal Methods ──

    fn insert_into(node: &mut STNode, point: STPoint, depth: usize) {
        // Check if we should subdivide
        if node.children.is_none()
            && node.count >= MAX_ENTRIES
            && depth < MAX_DEPTH
        {
            node.subdivide();

            // Re-insert existing entries into children
            let entries: Vec<_> = node.entries.values().flat_map(|v| v.clone()).collect();
            node.entries.clear();
            for entry in entries {
                let q = node.quadrant(entry.lon, entry.lat);
                // Safety: we just created children above
                let children = node.children.as_mut().expect("should be valid");
                Self::insert_into(&mut children[q], entry, depth + 1);
            }
        }

        // Insert into children if they exist
        if node.children.is_some() {
            let q = node.quadrant(point.lon, point.lat);
            let children = node.children.as_mut().expect("should be valid");
            Self::insert_into(&mut children[q], point, depth + 1);
        } else {
            // Insert into this node's timeline
            node.entries.entry(point.timestamp).or_default().push(point);
        }

        node.count += 1;
    }

    fn query_node<'a>(node: &'a STNode, query: &STQuery, results: &mut Vec<&'a STPoint>) {
        // Check spatial overlap
        if !node.overlaps(query) {
            return;
        }

        // Query this node's timeline
        for (_ts, points) in node.entries.range(query.min_time..=query.max_time) {
            for point in points {
                if point.lon >= query.min_lon
                    && point.lon <= query.max_lon
                    && point.lat >= query.min_lat
                    && point.lat <= query.max_lat
                {
                    results.push(point);
                }
            }
        }

        // Recurse into children
        if let Some(ref children) = node.children {
            for child in children.iter() {
                Self::query_node(child, query, results);
            }
        }
    }

    fn knn_node<'a>(
        node: &'a STNode,
        center_lon: f64,
        center_lat: f64,
        k: usize,
        min_time: i64,
        max_time: i64,
        results: &mut Vec<(&'a STPoint, f64)>,
    ) {
        // Query this node's timeline
        for (_ts, points) in node.entries.range(min_time..=max_time) {
            for point in points {
                let d = haversine_distance(center_lon, center_lat, point.lon, point.lat);
                if results.len() < k {
                    results.push((point, d));
                } else {
                    // Replace farthest if this is closer
                    if let Some(max_pos) = results
                        .iter()
                        .enumerate()
                        .max_by(|a, b| a.1 .1.partial_cmp(&b.1 .1).unwrap_or(std::cmp::Ordering::Equal))
                        .map(|(i, _)| i)
                    {
                        if d < results[max_pos].1 {
                            results[max_pos] = (point, d);
                        }
                    }
                }
            }
        }

        // Recurse into children
        if let Some(ref children) = node.children {
            for child in children.iter() {
                Self::knn_node(child, center_lon, center_lat, k, min_time, max_time, results);
            }
        }
    }
}

// ── Helpers ──

fn haversine_distance(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    const R: f64 = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    R * c
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_query() {
        let mut idx = STIndex::world();

        idx.insert(STPoint {
            id: "a".to_string(),
            lon: 116.4,
            lat: 39.9,
            timestamp: 1000,
        });
        idx.insert(STPoint {
            id: "b".to_string(),
            lon: 121.5,
            lat: 31.2,
            timestamp: 2000,
        });
        idx.insert(STPoint {
            id: "c".to_string(),
            lon: 116.5,
            lat: 40.0,
            timestamp: 1500,
        });

        // Query Beijing area
        let results = idx.query(&STQuery {
            min_lon: 116.0,
            min_lat: 39.0,
            max_lon: 117.0,
            max_lat: 41.0,
            min_time: 0,
            max_time: 9999,
        });
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_time_range_filter() {
        let mut idx = STIndex::world();

        for i in 0..10 {
            idx.insert(STPoint {
                id: format!("p{}", i),
                lon: 116.0 + i as f64 * 0.01,
                lat: 39.0 + i as f64 * 0.01,
                timestamp: 1000 + i * 100,
            });
        }

        // Query with time range
        let results = idx.query(&STQuery {
            min_lon: 115.0,
            min_lat: 38.0,
            max_lon: 118.0,
            max_lat: 42.0,
            min_time: 1200,
            max_time: 1500,
        });
        // Should find p2, p3, p4 (timestamps 1200, 1300, 1400)
        assert!(results.len() >= 1, "expected at least 1 result, got {}", results.len());
    }

    #[test]
    fn test_radius_query() {
        let mut idx = STIndex::world();

        idx.insert(STPoint {
            id: "near".to_string(),
            lon: 116.4,
            lat: 39.9,
            timestamp: 1000,
        });
        idx.insert(STPoint {
            id: "far".to_string(),
            lon: 121.5,
            lat: 31.2,
            timestamp: 1000,
        });

        let results = idx.radius_query(116.4, 39.9, 10_000.0, 0, 9999); // 10km
        assert!(results.len() >= 1);
        assert!(results.iter().any(|p| p.id == "near"));
    }

    #[test]
    fn test_knn_query() {
        let mut idx = STIndex::world();

        for i in 0..100 {
            idx.insert(STPoint {
                id: format!("p{}", i),
                lon: 116.0 + (i % 10) as f64 * 0.01,
                lat: 39.0 + (i / 10) as f64 * 0.01,
                timestamp: 1000 + i as i64,
            });
        }

        let results = idx.knn_query(116.05, 39.05, 5, 0, 999999);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_many_inserts() {
        let mut idx = STIndex::world();

        for i in 0..100 {
            idx.insert(STPoint {
                id: format!("p{}", i),
                lon: -180.0 + (i % 36) as f64 * 10.0,
                lat: -90.0 + (i / 36) as f64 * 10.0,
                timestamp: 1000 + i as i64,
            });
        }

        assert_eq!(idx.len(), 100);

        // Query a large area
        let results = idx.query(&STQuery {
            min_lon: -180.0,
            min_lat: -90.0,
            max_lon: 180.0,
            max_lat: 90.0,
            min_time: 0,
            max_time: 999999,
        });
        assert!(results.len() >= 10, "expected at least 10 results, got {}", results.len());
    }
}
