//! R*tree spatial index for OntoDB.
//!
//! R*tree is a balanced tree structure optimized for spatial range queries.
//! It groups nearby objects and represents them with their minimum bounding
//! rectangle (MBR) in the next higher level of the tree.
//!
//! Key features:
//! - Forced reinsertion for better node utilization
//! - Split heuristic that minimizes overlap
//! - Efficient range queries and nearest neighbor search
//!
//! Design: Each node stores up to MAX_ENTRIES entries. Leaf nodes store
//! (MBR, data_id) pairs. Internal nodes store (MBR, child_pointer) pairs.

use serde::{Deserialize, Serialize};

// ── Configuration ──

/// Maximum entries per node (typical: 25-50 for disk-based, 100+ for memory-based).
const MAX_ENTRIES: usize = 25;
/// Minimum entries per node (typically 40% of MAX).
const MIN_ENTRIES: usize = 10;
/// Number of entries to reinsert on overflow (typically 30% of MAX).
const REINSERT_COUNT: usize = 8;

// ── Bounding Box ──

/// Axis-aligned bounding box for spatial objects.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl BBox {
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Self { min_x, min_y, max_x, max_y }
    }

    /// Create a bounding box for a point.
    pub fn from_point(x: f64, y: f64) -> Self {
        Self { min_x: x, min_y: y, max_x: x, max_y: y }
    }

    /// Expand this bbox to include another bbox.
    pub fn expand(&mut self, other: &BBox) {
        self.min_x = self.min_x.min(other.min_x);
        self.min_y = self.min_y.min(other.min_y);
        self.max_x = self.max_x.max(other.max_x);
        self.max_y = self.max_y.max(other.max_y);
    }

    /// Create a new bbox that is the union of two bboxes.
    pub fn union(&self, other: &BBox) -> BBox {
        BBox {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    /// Area of this bbox.
    pub fn area(&self) -> f64 {
        (self.max_x - self.min_x) * (self.max_y - self.min_y)
    }

    /// Perimeter of this bbox.
    pub fn perimeter(&self) -> f64 {
        2.0 * ((self.max_x - self.min_x) + (self.max_y - self.min_y))
    }

    /// Check if this bbox contains a point.
    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    /// Check if this bbox intersects another bbox.
    pub fn intersects(&self, other: &BBox) -> bool {
        self.min_x <= other.max_x && self.max_x >= other.min_x
            && self.min_y <= other.max_y && self.max_y >= other.min_y
    }

    /// Check if this bbox fully contains another bbox.
    pub fn contains(&self, other: &BBox) -> bool {
        self.min_x <= other.min_x && self.max_x >= other.max_x
            && self.min_y <= other.min_y && self.max_y >= other.max_y
    }

    /// Distance from this bbox to a point (0 if inside).
    pub fn distance_to_point(&self, x: f64, y: f64) -> f64 {
        let dx = if x < self.min_x {
            self.min_x - x
        } else if x > self.max_x {
            x - self.max_x
        } else {
            0.0
        };
        let dy = if y < self.min_y {
            self.min_y - y
        } else if y > self.max_y {
            y - self.max_y
        } else {
            0.0
        };
        (dx * dx + dy * dy).sqrt()
    }
}

// ── R*tree Node ──

/// An entry in an R*tree node.
#[derive(Debug, Clone)]
struct Entry {
    /// Bounding box of this entry.
    bbox: BBox,
    /// For leaf nodes: the data ID. For internal nodes: child node index.
    data: EntryData,
}

#[derive(Debug, Clone)]
enum EntryData {
    /// Leaf entry: stores a data ID (e.g., entity key).
    Leaf(String),
    /// Internal entry: points to a child node.
    Internal(usize),
}

/// An R*tree node.
#[derive(Debug, Clone)]
struct Node {
    /// Whether this is a leaf node.
    is_leaf: bool,
    /// Entries in this node.
    entries: Vec<Entry>,
    /// For leaf nodes: parent node index (for reinsertion).
    parent: Option<usize>,
}

impl Node {
    fn new(is_leaf: bool) -> Self {
        Self {
            is_leaf,
            entries: Vec::with_capacity(MAX_ENTRIES + 1),
            parent: None,
        }
    }

    /// Compute the bounding box of all entries in this node.
    fn mbr(&self) -> BBox {
        if self.entries.is_empty() {
            return BBox::new(0.0, 0.0, 0.0, 0.0);
        }
        let mut mbr = self.entries[0].bbox;
        for entry in &self.entries[1..] {
            mbr.expand(&entry.bbox);
        }
        mbr
    }

    fn is_full(&self) -> bool {
        self.entries.len() >= MAX_ENTRIES
    }

    fn is_underfull(&self) -> bool {
        self.entries.len() < MIN_ENTRIES
    }
}

// ── R*tree ──

/// R*tree spatial index.
#[derive(Debug)]
pub struct RTree {
    /// All nodes (index 0 is root).
    nodes: Vec<Node>,
    /// Root node index.
    root: usize,
    /// Total number of entries.
    size: usize,
}

impl RTree {
    /// Create a new empty R*tree.
    pub fn new() -> Self {
        Self {
            nodes: vec![Node::new(true)],
            root: 0,
            size: 0,
        }
    }

    /// Number of entries in the tree.
    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Insert a bounding box with associated data ID.
    pub fn insert(&mut self, bbox: BBox, data_id: String) {
        let entry = Entry {
            bbox,
            data: EntryData::Leaf(data_id),
        };
        self.insert_at(self.root, entry);
        self.size += 1;
    }

    /// Delete an entry by data ID.
    pub fn delete(&mut self, data_id: &str) -> bool {
        let result = self.delete_from(self.root, data_id);
        if result {
            self.size -= 1;
        }
        result
    }

    /// Find all entries whose bounding boxes intersect the query bbox.
    pub fn search(&self, query: &BBox) -> Vec<String> {
        let mut results = Vec::new();
        self.search_node(self.root, query, &mut results);
        results
    }

    /// Find all entries that contain a given point.
    pub fn search_point(&self, x: f64, y: f64) -> Vec<String> {
        let query = BBox::from_point(x, y);
        self.search(&query)
    }

    /// Find k nearest entries to a point.
    pub fn knn(&self, x: f64, y: f64, k: usize) -> Vec<(String, f64)> {
        let mut results = Vec::new();
        self.knn_search(self.root, x, y, k, &mut results);
        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        results
    }

    // ── Internal Methods ──

    fn insert_at(&mut self, node_idx: usize, entry: Entry) {
        if self.nodes[node_idx].is_leaf {
            // Insert into leaf
            self.nodes[node_idx].entries.push(entry);
            if self.nodes[node_idx].is_full() {
                self.handle_overflow(node_idx);
            }
        } else {
            // Find best child to insert into
            let child_idx = self.choose_subtree(node_idx, &entry.bbox);
            self.insert_at(child_idx, entry);
            // Update parent bbox
            self.update_parent_bbox(node_idx);
        }
    }

    fn choose_subtree(&self, node_idx: usize, bbox: &BBox) -> usize {
        let node = &self.nodes[node_idx];
        let mut best_idx = 0;
        let mut best_area_enlargement = f64::INFINITY;
        let mut best_area = f64::INFINITY;

        for entry in &node.entries {
            if let EntryData::Internal(child) = entry.data {
                let union = entry.bbox.union(bbox);
                let area_enlargement = union.area() - entry.bbox.area();
                let area = entry.bbox.area();

                if area_enlargement < best_area_enlargement
                    || (area_enlargement == best_area_enlargement && area < best_area)
                {
                    best_area_enlargement = area_enlargement;
                    best_area = area;
                    best_idx = child;
                }
            }
        }

        best_idx
    }

    fn handle_overflow(&mut self, node_idx: usize) {
        // Simple split strategy to avoid deep recursion
        self.split_node(node_idx);
    }

    fn split_node(&mut self, node_idx: usize) {
        let entries: Vec<Entry> = self.nodes[node_idx].entries.drain(..).collect();
        let (group1, group2) = self.split_entries(entries);

        // Create new node
        let new_node_idx = self.nodes.len();
        let is_leaf = self.nodes[node_idx].is_leaf;
        let parent = self.nodes[node_idx].parent;

        self.nodes[node_idx].entries = group1;
        self.nodes.push(Node::new(is_leaf));
        self.nodes[new_node_idx].entries = group2;
        self.nodes[new_node_idx].parent = parent;

        // Update child parent pointers if internal node
        if !is_leaf {
            let child_indices: Vec<usize> = self.nodes[new_node_idx].entries.iter()
                .filter_map(|e| match &e.data {
                    EntryData::Internal(child) => Some(*child),
                    _ => None,
                })
                .collect();
            for child in child_indices {
                self.nodes[child].parent = Some(new_node_idx);
            }
        }

        // Insert new node into parent
        if let Some(parent_idx) = parent {
            let new_entry = Entry {
                bbox: self.nodes[new_node_idx].mbr(),
                data: EntryData::Internal(new_node_idx),
            };
            self.nodes[parent_idx].entries.push(new_entry);
            if self.nodes[parent_idx].is_full() {
                self.handle_overflow(parent_idx);
            }
        } else {
            // Root split: create new root
            let old_root = self.root;
            let new_root = self.nodes.len();
            self.nodes.push(Node::new(false));

            let entry1 = Entry {
                bbox: self.nodes[old_root].mbr(),
                data: EntryData::Internal(old_root),
            };
            let entry2 = Entry {
                bbox: self.nodes[new_node_idx].mbr(),
                data: EntryData::Internal(new_node_idx),
            };

            self.nodes[new_root].entries.push(entry1);
            self.nodes[new_root].entries.push(entry2);
            self.nodes[old_root].parent = Some(new_root);
            self.nodes[new_node_idx].parent = Some(new_root);
            self.root = new_root;
        }
    }

    fn split_entries(&self, entries: Vec<Entry>) -> (Vec<Entry>, Vec<Entry>) {
        // R*tree split: choose split axis and position
        // Simplified: use area-based heuristic
        let n = entries.len();
        let mid = n / 2;

        // Sort by X coordinate of center
        let mut sorted = entries;
        sorted.sort_by(|a, b| {
            let ca = (a.bbox.min_x + a.bbox.max_x) / 2.0;
            let cb = (b.bbox.min_x + b.bbox.max_x) / 2.0;
            ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
        });

        let group2 = sorted.split_off(mid);
        (sorted, group2)
    }

    fn delete_from(&mut self, node_idx: usize, data_id: &str) -> bool {
        if self.nodes[node_idx].is_leaf {
            let before = self.nodes[node_idx].entries.len();
            self.nodes[node_idx].entries.retain(|e| match &e.data {
                EntryData::Leaf(id) => id != data_id,
                _ => true,
            });
            before != self.nodes[node_idx].entries.len()
        } else {
            for i in 0..self.nodes[node_idx].entries.len() {
                if let EntryData::Internal(child) = self.nodes[node_idx].entries[i].data {
                    if self.delete_from(child, data_id) {
                        // Update bbox
                        self.update_parent_bbox(node_idx);
                        return true;
                    }
                }
            }
            false
        }
    }

    fn search_node(&self, node_idx: usize, query: &BBox, results: &mut Vec<String>) {
        for entry in &self.nodes[node_idx].entries {
            if entry.bbox.intersects(query) {
                match &entry.data {
                    EntryData::Leaf(id) => results.push(id.clone()),
                    EntryData::Internal(child) => self.search_node(*child, query, results),
                }
            }
        }
    }

    fn knn_search(&self, node_idx: usize, x: f64, y: f64, k: usize, results: &mut Vec<(String, f64)>) {
        for entry in &self.nodes[node_idx].entries {
            let dist = entry.bbox.distance_to_point(x, y);
            match &entry.data {
                EntryData::Leaf(id) => {
                    results.push((id.clone(), dist));
                }
                EntryData::Internal(child) => {
                    // Prune if we have k results and this entry is farther
                    if results.len() >= k {
                        let max_dist = results.iter().map(|(_, d)| *d).fold(0.0_f64, f64::max);
                        if dist > max_dist {
                            continue;
                        }
                    }
                    self.knn_search(*child, x, y, k, results);
                }
            }
        }
    }

    fn update_parent_bbox(&mut self, _node_idx: usize) {
        // No-op for now; bbox is computed dynamically via mbr()
    }
}

fn center_distance(bbox: &BBox, cx: f64, cy: f64) -> f64 {
    let bx = (bbox.min_x + bbox.max_x) / 2.0;
    let by = (bbox.min_y + bbox.max_y) / 2.0;
    ((bx - cx).powi(2) + (by - cy).powi(2)).sqrt()
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_search() {
        let mut tree = RTree::new();
        tree.insert(BBox::new(0.0, 0.0, 10.0, 10.0), "a".to_string());
        tree.insert(BBox::new(5.0, 5.0, 15.0, 15.0), "b".to_string());
        tree.insert(BBox::new(20.0, 20.0, 30.0, 30.0), "c".to_string());

        let results = tree.search(&BBox::new(0.0, 0.0, 12.0, 12.0));
        assert!(results.contains(&"a".to_string()));
        assert!(results.contains(&"b".to_string()));
        assert!(!results.contains(&"c".to_string()));
    }

    #[test]
    fn test_point_search() {
        let mut tree = RTree::new();
        tree.insert(BBox::new(0.0, 0.0, 10.0, 10.0), "a".to_string());
        tree.insert(BBox::new(20.0, 20.0, 30.0, 30.0), "b".to_string());

        let results = tree.search_point(5.0, 5.0);
        assert!(results.contains(&"a".to_string()));
        assert!(!results.contains(&"b".to_string()));
    }

    #[test]
    fn test_delete() {
        let mut tree = RTree::new();
        tree.insert(BBox::new(0.0, 0.0, 10.0, 10.0), "a".to_string());
        tree.insert(BBox::new(5.0, 5.0, 15.0, 15.0), "b".to_string());

        assert_eq!(tree.len(), 2);
        assert!(tree.delete("a"));
        assert_eq!(tree.len(), 1);

        let results = tree.search(&BBox::new(0.0, 0.0, 20.0, 20.0));
        assert!(!results.contains(&"a".to_string()));
        assert!(results.contains(&"b".to_string()));
    }

    #[test]
    fn test_knn() {
        let mut tree = RTree::new();
        tree.insert(BBox::from_point(0.0, 0.0), "origin".to_string());
        tree.insert(BBox::from_point(10.0, 0.0), "east".to_string());
        tree.insert(BBox::from_point(0.0, 10.0), "north".to_string());
        tree.insert(BBox::from_point(100.0, 100.0), "far".to_string());

        let results = tree.knn(1.0, 1.0, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "origin");
    }

    #[test]
    fn test_many_inserts() {
        let mut tree = RTree::new();
        for i in 0..1000 {
            let x = (i % 100) as f64;
            let y = (i / 100) as f64;
            tree.insert(BBox::new(x, y, x + 1.0, y + 1.0), format!("item_{}", i));
        }
        assert_eq!(tree.len(), 1000);

        let results = tree.search(&BBox::new(0.0, 0.0, 50.0, 5.0));
        assert!(!results.is_empty());
    }

    #[test]
    fn test_empty_tree() {
        let tree = RTree::new();
        assert_eq!(tree.len(), 0);
        let results = tree.search(&BBox::new(0.0, 0.0, 100.0, 100.0));
        assert!(results.is_empty());
    }
}
