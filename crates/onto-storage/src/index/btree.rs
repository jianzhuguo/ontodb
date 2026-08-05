//! Disk-oriented B+Tree for secondary index lookups.
//!
//! Architecture:
//! - Internal nodes: keys + child pointers (no values)
//! - Leaf nodes: keys + primary key lists + next-leaf pointer
//! - Leaf chain: linked list for efficient range scans
//! - Persistence: index entries are also stored in the LSM-Tree
//!
//! The B+Tree provides O(log n) point lookups, efficient range scans,
//! and handles node splitting on overflow.

/// Maximum number of keys per node. Branching factor = MAX_KEYS + 1.
const MAX_KEYS: usize = 128;

/// Internal node: keys separating child subtrees, child node IDs.
struct InternalNode {
    keys: Vec<Vec<u8>>,
    children: Vec<u64>,
}

/// Leaf node: indexed values mapped to primary keys, with next-leaf pointer.
struct LeafNode {
    keys: Vec<Vec<u8>>,
    values: Vec<Vec<Vec<u8>>>,
    next: Option<u64>,
}

/// A node in the B+Tree, either internal or leaf.
enum Node {
    Internal(InternalNode),
    Leaf(LeafNode),
}

impl Node {
    fn is_leaf(&self) -> bool {
        matches!(self, Node::Leaf(_))
    }

    fn key_count(&self) -> usize {
        match self {
            Node::Internal(n) => n.keys.len(),
            Node::Leaf(n) => n.keys.len(),
        }
    }

    fn is_full(&self) -> bool {
        self.key_count() >= MAX_KEYS
    }
}

/// A cursor pointing to a position in the leaf chain for iteration.
struct Cursor {
    leaf_id: u64,
    idx: usize,
}

/// In-memory B+Tree index for a single indexed column.
///
/// Maps column values to sets of primary keys. The tree supports:
/// - O(log n) point lookups via root-to-leaf traversal
/// - Efficient range scans via leaf chain traversal
/// - Insert/delete with automatic node splitting
pub struct BPlusTree {
    pub class: String,
    pub column: String,
    nodes: Vec<(u64, Node)>,
    root: u64,
    next_id: u64,
    len: usize,
}

impl BPlusTree {
    /// Creates a new empty B+Tree index.
    pub fn new(class: &str, column: &str) -> Self {
        let root_id = 0;
        let root_node = Node::Leaf(LeafNode {
            keys: Vec::new(),
            values: Vec::new(),
            next: None,
        });
        Self {
            class: class.to_string(),
            column: column.to_string(),
            nodes: vec![(root_id, root_node)],
            root: root_id,
            next_id: 1,
            len: 0,
        }
    }

    /// Returns the number of distinct indexed values.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn get_node(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find(|(nid, _)| *nid == id).map(|(_, n)| n)
    }

    fn get_node_mut(&mut self, id: u64) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|(nid, _)| *nid == id).map(|(_, n)| n)
    }

    // ═══════════════════════════════════════════════════════════════
    //  Insert
    // ═══════════════════════════════════════════════════════════════

    /// Inserts an index entry: value -> primary_key.
    /// If the value already exists, the primary key is added to the existing set.
    pub fn insert(&mut self, key: Vec<u8>, primary_key: Vec<u8>) {
        let result = self.insert_recursive(self.root, &key, primary_key);
        if let Some((promoted_key, new_sibling_id)) = result {
            let new_root_id = self.alloc_id();
            let old_root = self.root;
            let new_root = Node::Internal(InternalNode {
                keys: vec![promoted_key],
                children: vec![old_root, new_sibling_id],
            });
            self.nodes.push((new_root_id, new_root));
            self.root = new_root_id;
        }
        self.len += 1;
    }

    /// Recursive insert. Returns (promoted_key, new_sibling_id) if a split occurred.
    fn insert_recursive(
        &mut self,
        node_id: u64,
        key: &[u8],
        primary_key: Vec<u8>,
    ) -> Option<(Vec<u8>, u64)> {
        let is_leaf = self.get_node(node_id).unwrap().is_leaf();

        if is_leaf {
            self.insert_into_leaf(node_id, key, primary_key)
        } else {
            self.insert_into_internal(node_id, key, primary_key)
        }
    }

    fn insert_into_leaf(
        &mut self,
        leaf_id: u64,
        key: &[u8],
        primary_key: Vec<u8>,
    ) -> Option<(Vec<u8>, u64)> {
        // Find insertion position
        let idx = match self.get_node(leaf_id).unwrap() {
            Node::Leaf(n) => n.keys.binary_search_by(|k| k.as_slice().cmp(key)),
            _ => unreachable!(),
        };

        match idx {
            Ok(i) => {
                // Key exists — add primary_key if not duplicate
                if let Node::Leaf(n) = self.get_node_mut(leaf_id).unwrap() {
                    if !n.values[i].contains(&primary_key) {
                        n.values[i].push(primary_key);
                    }
                }
                None
            }
            Err(i) => {
                // Insert new key at position i
                if let Node::Leaf(n) = self.get_node_mut(leaf_id).unwrap() {
                    n.keys.insert(i, key.to_vec());
                    n.values.insert(i, vec![primary_key]);
                }
                // Split if over capacity
                if self.get_node(leaf_id).unwrap().is_full() {
                    Some(self.split_leaf(leaf_id))
                } else {
                    None
                }
            }
        }
    }

    fn insert_into_internal(
        &mut self,
        node_id: u64,
        key: &[u8],
        primary_key: Vec<u8>,
    ) -> Option<(Vec<u8>, u64)> {
        // Find child to descend into
        let child_id = match self.get_node(node_id).unwrap() {
            Node::Internal(n) => {
                let idx = n.keys.binary_search_by(|k| k.as_slice().cmp(key));
                let child_idx = match idx {
                    Ok(i) => i + 1,
                    Err(i) => i,
                };
                n.children[child_idx]
            }
            _ => unreachable!(),
        };

        // Recurse into child
        let result = self.insert_recursive(child_id, key, primary_key);

        if let Some((promoted_key, new_child_id)) = result {
            // Find position to insert promoted key
            let insert_pos = match self.get_node(node_id).unwrap() {
                Node::Internal(n) => n
                    .keys
                    .binary_search_by(|k| k.as_slice().cmp(&promoted_key))
                    .unwrap_or_else(|i| i),
                _ => unreachable!(),
            };

            // Insert promoted key and new child pointer
            if let Node::Internal(n) = self.get_node_mut(node_id).unwrap() {
                n.keys.insert(insert_pos, promoted_key);
                n.children.insert(insert_pos + 1, new_child_id);
            }

            // Split if over capacity
            if self.get_node(node_id).unwrap().is_full() {
                Some(self.split_internal(node_id))
            } else {
                None
            }
        } else {
            None
        }
    }

    // ═══════════════════════════════════════════════════════════════
    //  Split
    // ═══════════════════════════════════════════════════════════════

    /// Splits a leaf node. Returns (promoted_key, new_sibling_id).
    fn split_leaf(&mut self, leaf_id: u64) -> (Vec<u8>, u64) {
        let mid = MAX_KEYS / 2;
        let new_id = self.alloc_id();

        let (promoted_key, new_leaf) = match self.get_node_mut(leaf_id).unwrap() {
            Node::Leaf(n) => {
                let new_keys = n.keys.split_off(mid);
                let new_values = n.values.split_off(mid);
                let promoted = new_keys[0].clone();

                let old_next = n.next;
                n.next = Some(new_id);

                (
                    promoted,
                    Node::Leaf(LeafNode {
                        keys: new_keys,
                        values: new_values,
                        next: old_next,
                    }),
                )
            }
            _ => unreachable!(),
        };

        self.nodes.push((new_id, new_leaf));
        (promoted_key, new_id)
    }

    /// Splits an internal node. Returns (promoted_key, new_sibling_id).
    fn split_internal(&mut self, node_id: u64) -> (Vec<u8>, u64) {
        let mid = MAX_KEYS / 2;
        let new_id = self.alloc_id();

        let (promoted_key, new_internal) = match self.get_node_mut(node_id).unwrap() {
            Node::Internal(n) => {
                let promoted = n.keys[mid].clone();
                let new_keys = n.keys.split_off(mid + 1);
                let new_children = n.children.split_off(mid + 1);
                n.keys.pop(); // Remove the promoted key from left side

                (
                    promoted,
                    Node::Internal(InternalNode {
                        keys: new_keys,
                        children: new_children,
                    }),
                )
            }
            _ => unreachable!(),
        };

        self.nodes.push((new_id, new_internal));
        (promoted_key, new_id)
    }

    // ═══════════════════════════════════════════════════════════════
    //  Delete
    // ═══════════════════════════════════════════════════════════════

    /// Removes a primary_key from the set for the given value.
    /// If the set becomes empty, the key entry is removed.
    pub fn remove(&mut self, value: &[u8], primary_key: &[u8]) {
        let leaf_id = self.find_leaf(value);

        if let Node::Leaf(n) = self.get_node_mut(leaf_id).unwrap() {
            if let Ok(i) = n.keys.binary_search_by(|k| k.as_slice().cmp(value)) {
                let was_present = n.values[i].iter().any(|pk| pk.as_slice() == primary_key);
                if was_present {
                    n.values[i].retain(|pk| pk.as_slice() != primary_key);
                    if n.values[i].is_empty() {
                        n.keys.remove(i);
                        n.values.remove(i);
                    }
                    self.len -= 1;
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════
    //  Lookup
    // ═══════════════════════════════════════════════════════════════

    /// Point lookup: returns all primary keys with the given value.
    pub fn lookup(&self, value: &[u8]) -> Vec<Vec<u8>> {
        let leaf_id = self.find_leaf(value);

        if let Node::Leaf(n) = self.get_node(leaf_id).unwrap() {
            if let Ok(i) = n.keys.binary_search_by(|k| k.as_slice().cmp(value)) {
                return n.values[i].clone();
            }
        }

        Vec::new()
    }

    /// Finds the leaf node that should contain the given key.
    fn find_leaf(&self, key: &[u8]) -> u64 {
        let mut current = self.root;
        loop {
            match self.get_node(current).unwrap() {
                Node::Leaf(_) => return current,
                Node::Internal(n) => {
                    let idx = n.keys.binary_search_by(|k| k.as_slice().cmp(key));
                    let child_idx = match idx {
                        Ok(i) => i + 1,
                        Err(i) => i,
                    };
                    current = n.children[child_idx];
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════
    //  Range scans via leaf chain
    // ═══════════════════════════════════════════════════════════════

    /// Range scan: returns all primary keys with values in [low, high].
    /// If low is None, scans from the beginning.
    /// If high is None, scans to the end.
    pub fn range_scan(
        &self,
        low: Option<&[u8]>,
        high: Option<&[u8]>,
    ) -> Vec<Vec<u8>> {
        let mut result = Vec::new();

        let mut cursor = match low {
            Some(k) => self.cursor_lower_bound(k),
            None => self.cursor_first(),
        };

        while cursor.is_valid(self) {
            let leaf = match self.get_node(cursor.leaf_id).unwrap() {
                Node::Leaf(n) => n,
                _ => break,
            };
            if cursor.idx >= leaf.keys.len() {
                cursor.advance(self);
                continue;
            }

            let k = leaf.keys[cursor.idx].as_slice();
            if high.map_or(false, |h| k > h) {
                break;
            }

            result.extend(leaf.values[cursor.idx].iter().cloned());
            cursor.idx += 1;
        }

        result
    }

    /// Greater-than scan: returns all primary keys with values > threshold.
    pub fn gt_scan(&self, threshold: &[u8]) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        let mut cursor = self.cursor_after(threshold);

        while cursor.is_valid(self) {
            let leaf = match self.get_node(cursor.leaf_id).unwrap() {
                Node::Leaf(n) => n,
                _ => break,
            };
            if cursor.idx >= leaf.keys.len() {
                cursor.advance(self);
                continue;
            }

            result.extend(leaf.values[cursor.idx].iter().cloned());
            cursor.idx += 1;
        }

        result
    }

    /// Less-than scan: returns all primary keys with values < threshold.
    pub fn lt_scan(&self, threshold: &[u8]) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        let mut cursor = self.cursor_first();

        while cursor.is_valid(self) {
            let leaf = match self.get_node(cursor.leaf_id).unwrap() {
                Node::Leaf(n) => n,
                _ => break,
            };
            if cursor.idx >= leaf.keys.len() {
                cursor.advance(self);
                continue;
            }

            if leaf.keys[cursor.idx].as_slice() >= threshold {
                break;
            }

            result.extend(leaf.values[cursor.idx].iter().cloned());
            cursor.idx += 1;
        }

        result
    }

    /// Greater-than-or-equal scan.
    pub fn gte_scan(&self, threshold: &[u8]) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        let mut cursor = self.cursor_lower_bound(threshold);

        while cursor.is_valid(self) {
            let leaf = match self.get_node(cursor.leaf_id).unwrap() {
                Node::Leaf(n) => n,
                _ => break,
            };
            if cursor.idx >= leaf.keys.len() {
                cursor.advance(self);
                continue;
            }

            result.extend(leaf.values[cursor.idx].iter().cloned());
            cursor.idx += 1;
        }

        result
    }

    /// Less-than-or-equal scan.
    pub fn lte_scan(&self, threshold: &[u8]) -> Vec<Vec<u8>> {
        let mut result = Vec::new();
        let mut cursor = self.cursor_first();

        while cursor.is_valid(self) {
            let leaf = match self.get_node(cursor.leaf_id).unwrap() {
                Node::Leaf(n) => n,
                _ => break,
            };
            if cursor.idx >= leaf.keys.len() {
                cursor.advance(self);
                continue;
            }

            if leaf.keys[cursor.idx].as_slice() > threshold {
                break;
            }

            result.extend(leaf.values[cursor.idx].iter().cloned());
            cursor.idx += 1;
        }

        result
    }

    // ═══════════════════════════════════════════════════════════════
    //  Cursor helpers
    // ═══════════════════════════════════════════════════════════════

    /// Cursor to the first entry (leftmost leaf).
    fn cursor_first(&self) -> Cursor {
        let mut current = self.root;
        loop {
            match self.get_node(current).unwrap() {
                Node::Leaf(_) => return Cursor { leaf_id: current, idx: 0 },
                Node::Internal(n) => current = n.children[0],
            }
        }
    }

    /// Cursor to the first entry with key >= target.
    fn cursor_lower_bound(&self, target: &[u8]) -> Cursor {
        let leaf_id = self.find_leaf(target);
        if let Node::Leaf(n) = self.get_node(leaf_id).unwrap() {
            let idx = match n.keys.binary_search_by(|k| k.as_slice().cmp(target)) {
                Ok(i) => i,
                Err(i) => i,
            };
            Cursor { leaf_id, idx }
        } else {
            Cursor { leaf_id, idx: 0 }
        }
    }

    /// Cursor to the first entry with key > target.
    fn cursor_after(&self, target: &[u8]) -> Cursor {
        let leaf_id = self.find_leaf(target);
        if let Node::Leaf(n) = self.get_node(leaf_id).unwrap() {
            let idx = match n.keys.binary_search_by(|k| k.as_slice().cmp(target)) {
                Ok(i) => i + 1,
                Err(i) => i,
            };
            Cursor { leaf_id, idx }
        } else {
            Cursor { leaf_id, idx: 0 }
        }
    }
}

impl Cursor {
    fn is_valid(&self, tree: &BPlusTree) -> bool {
        tree.get_node(self.leaf_id).is_some()
    }

    fn advance(&mut self, tree: &BPlusTree) {
        if let Node::Leaf(n) = tree.get_node(self.leaf_id).unwrap() {
            if let Some(next_id) = n.next {
                self.leaf_id = next_id;
                self.idx = 0;
            } else {
                // End of chain — mark invalid by using a non-existent id
                self.leaf_id = u64::MAX;
                self.idx = 0;
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_lookup() {
        let mut tree = BPlusTree::new("Product", "price");
        tree.insert(b"100".to_vec(), b"pk1".to_vec());
        tree.insert(b"100".to_vec(), b"pk2".to_vec());
        tree.insert(b"200".to_vec(), b"pk3".to_vec());

        let keys = tree.lookup(b"100");
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&b"pk1".to_vec()));
        assert!(keys.contains(&b"pk2".to_vec()));

        let keys = tree.lookup(b"200");
        assert_eq!(keys.len(), 1);

        let keys = tree.lookup(b"999");
        assert_eq!(keys.len(), 0);
    }

    #[test]
    fn test_insert_duplicate_pk() {
        let mut tree = BPlusTree::new("Product", "price");
        tree.insert(b"100".to_vec(), b"pk1".to_vec());
        tree.insert(b"100".to_vec(), b"pk1".to_vec()); // duplicate

        let keys = tree.lookup(b"100");
        assert_eq!(keys.len(), 1); // should not duplicate
    }

    #[test]
    fn test_remove() {
        let mut tree = BPlusTree::new("Product", "price");
        tree.insert(b"100".to_vec(), b"pk1".to_vec());
        tree.insert(b"100".to_vec(), b"pk2".to_vec());

        tree.remove(b"100", b"pk1");
        let keys = tree.lookup(b"100");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], b"pk2");

        tree.remove(b"100", b"pk2");
        let keys = tree.lookup(b"100");
        assert_eq!(keys.len(), 0);
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn test_range_scan() {
        let mut tree = BPlusTree::new("Product", "price");
        tree.insert(b"100".to_vec(), b"pk1".to_vec());
        tree.insert(b"200".to_vec(), b"pk2".to_vec());
        tree.insert(b"300".to_vec(), b"pk3".to_vec());
        tree.insert(b"400".to_vec(), b"pk4".to_vec());

        let keys = tree.range_scan(Some(b"150"), Some(b"350"));
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&b"pk2".to_vec()));
        assert!(keys.contains(&b"pk3".to_vec()));
    }

    #[test]
    fn test_range_scan_unbounded() {
        let mut tree = BPlusTree::new("Product", "price");
        tree.insert(b"100".to_vec(), b"pk1".to_vec());
        tree.insert(b"200".to_vec(), b"pk2".to_vec());
        tree.insert(b"300".to_vec(), b"pk3".to_vec());

        // Unbounded high
        let keys = tree.range_scan(Some(b"200"), None);
        assert_eq!(keys.len(), 2);

        // Unbounded low
        let keys = tree.range_scan(None, Some(b"200"));
        assert_eq!(keys.len(), 2);

        // Fully unbounded
        let keys = tree.range_scan(None, None);
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn test_gt_lt_scans() {
        let mut tree = BPlusTree::new("Product", "price");
        tree.insert(b"100".to_vec(), b"pk1".to_vec());
        tree.insert(b"200".to_vec(), b"pk2".to_vec());
        tree.insert(b"300".to_vec(), b"pk3".to_vec());

        let keys = tree.gt_scan(b"150");
        assert_eq!(keys.len(), 2);

        let keys = tree.lt_scan(b"250");
        assert_eq!(keys.len(), 2);

        let keys = tree.gte_scan(b"200");
        assert_eq!(keys.len(), 2);

        let keys = tree.lte_scan(b"200");
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn test_node_splitting() {
        let mut tree = BPlusTree::new("Product", "price");

        // Insert enough entries to trigger leaf splits
        for i in 0..500u32 {
            let key = format!("{:010}", i * 10);
            let pk = format!("pk_{}", i);
            tree.insert(key.into_bytes(), pk.into_bytes());
        }

        assert_eq!(tree.len(), 500);

        // Verify all entries are still accessible
        for i in 0..500u32 {
            let key = format!("{:010}", i * 10);
            let pk = format!("pk_{}", i);
            let keys = tree.lookup(key.as_bytes());
            assert_eq!(keys.len(), 1, "key {} should have 1 pk", key);
            assert_eq!(keys[0], pk.as_bytes());
        }
    }

    #[test]
    fn test_range_scan_after_split() {
        let mut tree = BPlusTree::new("Product", "price");

        for i in 0..300u32 {
            let key = format!("{:010}", i);
            let pk = format!("pk_{}", i);
            tree.insert(key.into_bytes(), pk.into_bytes());
        }

        // Range scan should work across split leaf nodes
        let keys = tree.range_scan(Some(b"0000000050"), Some(b"0000000150"));
        assert_eq!(keys.len(), 101); // 50..=150
    }

    #[test]
    fn test_large_dataset() {
        let mut tree = BPlusTree::new("Product", "price");

        let n = 2000u32;
        for i in 0..n {
            let key = format!("{:012}", i);
            let pk = format!("pk_{:08}", i);
            tree.insert(key.into_bytes(), pk.into_bytes());
        }

        assert_eq!(tree.len(), n as usize);

        // Point lookup
        let keys = tree.lookup(b"000000001000");
        assert_eq!(keys.len(), 1);

        // Range scan
        let keys = tree.range_scan(Some(b"000000000500"), Some(b"000000000599"));
        assert_eq!(keys.len(), 100);

        // GT scan
        let keys = tree.gt_scan(b"000000001990");
        assert_eq!(keys.len(), 9); // 1991..=1999
    }

    #[test]
    fn test_multiple_pks_per_value() {
        let mut tree = BPlusTree::new("Order", "status");

        // Multiple orders with same status
        tree.insert(b"pending".to_vec(), b"order_1".to_vec());
        tree.insert(b"pending".to_vec(), b"order_2".to_vec());
        tree.insert(b"pending".to_vec(), b"order_3".to_vec());
        tree.insert(b"shipped".to_vec(), b"order_4".to_vec());

        let pending = tree.lookup(b"pending");
        assert_eq!(pending.len(), 3);

        let shipped = tree.lookup(b"shipped");
        assert_eq!(shipped.len(), 1);

        // Remove one
        tree.remove(b"pending", b"order_2");
        let pending = tree.lookup(b"pending");
        assert_eq!(pending.len(), 2);
        assert!(!pending.contains(&b"order_2".to_vec()));
    }

    #[test]
    fn test_leaf_chain_integrity() {
        let mut tree = BPlusTree::new("Product", "price");

        for i in 0..500u32 {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }

        // Walk the leaf chain and verify sorted order
        let mut cursor = tree.cursor_first();
        let mut prev_key: Vec<u8> = Vec::new();
        let mut count = 0usize;

        while cursor.is_valid(&tree) {
            if let Node::Leaf(leaf) = tree.get_node(cursor.leaf_id).unwrap() {
                if cursor.idx >= leaf.keys.len() {
                    cursor.advance(&tree);
                    continue;
                }
                let k = &leaf.keys[cursor.idx];
                assert!(k.as_slice() > prev_key.as_slice(), "keys must be sorted");
                prev_key = k.clone();
                count += 1;
                cursor.idx += 1;
            }
        }

        assert_eq!(count, 500);
    }
}
