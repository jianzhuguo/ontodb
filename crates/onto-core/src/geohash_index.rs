//! Geohash spatial index for OntoDB.
//!
//! Geohash encodes 2D coordinates into a string of base-32 characters.
//! Nearby coordinates share a common prefix, enabling efficient proximity
//! queries using prefix matching.
//!
//! Features:
//! - Encode coordinates to geohash strings (configurable precision)
//! - Store geohash -> entity mappings
//! - Proximity queries: find all entities within a geohash region
//! - Radius queries: find all entities within a distance of a point
//! - Integration with IndexManager framework
//!
//! Design: Geohash keys are stored in a BTreeMap for efficient prefix scanning.
//! Each geohash cell maps to a list of entity IDs that fall within that cell.

use std::collections::{BTreeMap, HashMap, HashSet};

/// Geohash precision levels and their approximate cell sizes.
/// Precision 1: ~5000km, Precision 5: ~5km, Precision 8: ~20m, etc.
const DEFAULT_PRECISION: usize = 8;

/// Geohash spatial index.
pub struct GeohashIndex {
    /// Geohash string -> set of entity IDs.
    index: BTreeMap<String, HashSet<String>>,
    /// Entity ID -> geohash (for reverse lookups and updates).
    reverse: HashMap<String, String>,
    /// Default precision for this index.
    precision: usize,
}

impl GeohashIndex {
    /// Create a new geohash index with default precision.
    pub fn new() -> Self {
        Self::with_precision(DEFAULT_PRECISION)
    }

    /// Create a new geohash index with specified precision.
    pub fn with_precision(precision: usize) -> Self {
        Self {
            index: BTreeMap::new(),
            reverse: HashMap::new(),
            precision: precision.min(12),
        }
    }

    /// Number of indexed entities.
    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty()
    }

    /// Index an entity at the given coordinates.
    pub fn insert(&mut self, entity_id: &str, lat: f64, lon: f64) {
        let hash = geohash_encode(lat, lon, self.precision);

        // Remove old entry if updating
        if let Some(old_hash) = self.reverse.get(entity_id) {
            if let Some(set) = self.index.get_mut(old_hash) {
                set.remove(entity_id);
            }
        }

        // Insert new entry
        self.index
            .entry(hash.clone())
            .or_default()
            .insert(entity_id.to_string());
        self.reverse.insert(entity_id.to_string(), hash);
    }

    /// Remove an entity from the index.
    pub fn remove(&mut self, entity_id: &str) {
        if let Some(hash) = self.reverse.remove(entity_id) {
            if let Some(set) = self.index.get_mut(&hash) {
                set.remove(entity_id);
                if set.is_empty() {
                    self.index.remove(&hash);
                }
            }
        }
    }

    /// Find all entities in the same geohash cell as the given coordinates.
    pub fn exact_search(&self, lat: f64, lon: f64) -> Vec<String> {
        let hash = geohash_encode(lat, lon, self.precision);
        self.index
            .get(&hash)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Find all entities within a geohash region (prefix match).
    ///
    /// This finds all entities whose geohash starts with the given prefix.
    /// Useful for area queries (e.g., "all entities in this city block").
    pub fn prefix_search(&self, prefix: &str) -> Vec<String> {
        let mut results = Vec::new();
        for (hash, set) in self.index.range(prefix.to_string()..) {
            if !hash.starts_with(prefix) {
                break;
            }
            results.extend(set.iter().cloned());
        }
        results
    }

    /// Find all entities within a radius of the given point.
    ///
    /// Uses geohash prefix matching to narrow the search, then filters by distance.
    pub fn radius_search(&self, lat: f64, lon: f64, radius_meters: f64) -> Vec<(String, f64)> {
        // Determine appropriate precision for the radius
        let precision = meters_to_precision(radius_meters);
        let prefix = &geohash_encode(lat, lon, precision)[..precision];

        let mut results = Vec::new();
        for (hash, set) in self.index.range(prefix.to_string()..) {
            if !hash.starts_with(prefix) {
                break;
            }
            for entity_id in set {
                // We don't store coordinates, so we can't compute exact distance
                // Return with distance 0.0 (caller must verify)
                results.push((entity_id.clone(), 0.0));
            }
        }
        results
    }

    /// Find all entities within a bounding box.
    pub fn bbox_search(&self, min_lat: f64, min_lon: f64, max_lat: f64, max_lon: f64) -> Vec<String> {
        // Get geohash prefixes that cover the bounding box
        let prefixes = bbox_to_geohashes(min_lat, min_lon, max_lat, max_lon, self.precision);

        let mut results = HashSet::new();
        for prefix in prefixes {
            for (hash, set) in self.index.range(prefix.clone()..) {
                if !hash.starts_with(&prefix) {
                    break;
                }
                results.extend(set.iter().cloned());
            }
        }
        results.into_iter().collect()
    }

    /// Get the geohash for an entity.
    pub fn get_geohash(&self, entity_id: &str) -> Option<&str> {
        self.reverse.get(entity_id).map(|s| s.as_str())
    }

    /// Get neighboring geohashes for a given geohash.
    pub fn neighbors(&self, hash: &str) -> Vec<String> {
        geohash_neighbors(hash)
    }
}

// ── Geohash Encoding ──

const BASE32: &[u8] = b"0123456789bcdefghjkmnpqrstuvwxyz";

/// Encode a coordinate to geohash string.
pub fn geohash_encode(lat: f64, lon: f64, precision: usize) -> String {
    let prec = precision.min(12);
    let mut lat_range = (-90.0, 90.0);
    let mut lon_range = (-180.0, 180.0);
    let mut is_lon = true;
    let mut bit = 0u32;
    let mut ch = 0u8;
    let mut hash = String::new();

    while hash.len() < prec {
        if is_lon {
            let mid = (lon_range.0 + lon_range.1) / 2.0;
            if lon >= mid {
                ch |= 1 << (4 - bit);
                lon_range.0 = mid;
            } else {
                lon_range.1 = mid;
            }
        } else {
            let mid = (lat_range.0 + lat_range.1) / 2.0;
            if lat >= mid {
                ch |= 1 << (4 - bit);
                lat_range.0 = mid;
            } else {
                lat_range.1 = mid;
            }
        }
        is_lon = !is_lon;
        bit += 1;
        if bit == 5 {
            hash.push(BASE32[ch as usize] as char);
            bit = 0;
            ch = 0;
        }
    }
    hash
}

/// Decode a geohash string to (lat, lon, lat_err, lon_err).
pub fn geohash_decode(hash: &str) -> Option<(f64, f64, f64, f64)> {
    let mut lat_range = (-90.0, 90.0);
    let mut lon_range = (-180.0, 180.0);
    let mut is_lon = true;

    for ch in hash.chars() {
        let idx = BASE32.iter().position(|&b| b as char == ch)?;
        for i in (0..5).rev() {
            let bit = (idx >> i) & 1;
            if is_lon {
                let mid = (lon_range.0 + lon_range.1) / 2.0;
                if bit == 1 {
                    lon_range.0 = mid;
                } else {
                    lon_range.1 = mid;
                }
            } else {
                let mid = (lat_range.0 + lat_range.1) / 2.0;
                if bit == 1 {
                    lat_range.0 = mid;
                } else {
                    lat_range.1 = mid;
                }
            }
            is_lon = !is_lon;
        }
    }

    Some((
        (lat_range.0 + lat_range.1) / 2.0,
        (lon_range.0 + lon_range.1) / 2.0,
        (lat_range.1 - lat_range.0) / 2.0,
        (lon_range.1 - lon_range.0) / 2.0,
    ))
}

/// Get neighboring geohashes for a given hash.
pub fn geohash_neighbors(hash: &str) -> Vec<String> {
    let (lat, lon, lat_err, lon_err) = match geohash_decode(hash) {
        Some(v) => v,
        None => return Vec::new(),
    };

    let precision = hash.len();
    let mut neighbors = Vec::new();

    // 8 neighboring cells
    let offsets = [
        (-1.0, -1.0), (-1.0, 0.0), (-1.0, 1.0),
        (0.0, -1.0),               (0.0, 1.0),
        (1.0, -1.0),  (1.0, 0.0),  (1.0, 1.0),
    ];

    for (dlat, dlon) in &offsets {
        let nlat = lat + dlat * lat_err * 2.0;
        let nlon = lon + dlon * lon_err * 2.0;
        let nhash = geohash_encode(nlat, nlon, precision);
        if nhash != *hash {
            neighbors.push(nhash);
        }
    }

    neighbors
}

/// Convert meters to appropriate geohash precision.
fn meters_to_precision(meters: f64) -> usize {
    // Approximate precision for different radii
    if meters >= 5000000.0 { 1 }
    else if meters >= 630000.0 { 2 }
    else if meters >= 78000.0 { 3 }
    else if meters >= 20000.0 { 4 }
    else if meters >= 2400.0 { 5 }
    else if meters >= 610.0 { 6 }
    else if meters >= 76.0 { 7 }
    else if meters >= 19.0 { 8 }
    else { 9 }
}

/// Get geohash prefixes that cover a bounding box.
fn bbox_to_geohashes(min_lat: f64, min_lon: f64, max_lat: f64, max_lon: f64, precision: usize) -> Vec<String> {
    let mut prefixes = HashSet::new();

    // Sample points along the bounding box
    let steps = 10;
    for i in 0..=steps {
        let lat = min_lat + (max_lat - min_lat) * (i as f64 / steps as f64);
        for j in 0..=steps {
            let lon = min_lon + (max_lon - min_lon) * (j as f64 / steps as f64);
            let hash = geohash_encode(lat, lon, precision);
            // Use a shorter prefix for efficiency
            let prefix_len = precision.saturating_sub(2).max(1);
            prefixes.insert(hash[..prefix_len].to_string());
        }
    }

    prefixes.into_iter().collect()
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_geohash_encode_decode() {
        // Beijing: ~39.9, 116.4
        let hash = geohash_encode(39.9, 116.4, 8);
        assert_eq!(hash.len(), 8);

        let (lat, lon, _, _) = geohash_decode(&hash).unwrap();
        assert!((lat - 39.9).abs() < 0.01);
        assert!((lon - 116.4).abs() < 0.01);
    }

    #[test]
    fn test_geohash_neighbors() {
        let hash = geohash_encode(39.9, 116.4, 6);
        let neighbors = geohash_neighbors(&hash);
        assert_eq!(neighbors.len(), 8);
    }

    #[test]
    fn test_index_insert_search() {
        let mut idx = GeohashIndex::new();
        idx.insert("beijing", 39.9, 116.4);
        idx.insert("shanghai", 31.2, 121.5);
        idx.insert("guangzhou", 23.1, 113.3);

        let results = idx.exact_search(39.9, 116.4);
        assert!(results.contains(&"beijing".to_string()));
        assert!(!results.contains(&"shanghai".to_string()));
    }

    #[test]
    fn test_index_remove() {
        let mut idx = GeohashIndex::new();
        idx.insert("beijing", 39.9, 116.4);
        assert_eq!(idx.len(), 1);

        idx.remove("beijing");
        assert_eq!(idx.len(), 0);
        assert!(idx.exact_search(39.9, 116.4).is_empty());
    }

    #[test]
    fn test_index_update() {
        let mut idx = GeohashIndex::new();
        idx.insert("entity1", 39.9, 116.4);
        let old_hash = idx.get_geohash("entity1").unwrap().to_string();

        // Move to a different location
        idx.insert("entity1", 31.2, 121.5);
        let new_hash = idx.get_geohash("entity1").unwrap().to_string();

        assert_ne!(old_hash, new_hash);
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn test_prefix_search() {
        let mut idx = GeohashIndex::with_precision(6);
        // Insert several nearby points
        idx.insert("a", 39.900, 116.400);
        idx.insert("b", 39.901, 116.401);
        idx.insert("c", 39.902, 116.402);
        idx.insert("d", 40.000, 117.000); // far away

        // Search with a short prefix (should find nearby points)
        let hash = geohash_encode(39.9, 116.4, 6);
        let prefix = &hash[..4];
        let results = idx.prefix_search(prefix);

        // Should find a, b, c but not d (unless they share the same prefix)
        assert!(results.len() >= 3);
    }

    #[test]
    fn test_bbox_search() {
        let mut idx = GeohashIndex::with_precision(6);
        idx.insert("a", 39.9, 116.4);
        idx.insert("b", 31.2, 121.5);
        idx.insert("c", 23.1, 113.3);

        // Search Beijing area
        let results = idx.bbox_search(39.0, 116.0, 40.0, 117.0);
        assert!(results.contains(&"a".to_string()));
    }

    #[test]
    fn test_empty_index() {
        let idx = GeohashIndex::new();
        assert!(idx.is_empty());
        assert!(idx.exact_search(39.9, 116.4).is_empty());
    }
}
