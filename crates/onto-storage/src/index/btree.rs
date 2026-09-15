// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
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

use std::collections::HashMap;

/// Maximum number of keys per node. Branching factor = MAX_KEYS + 1.
const MAX_KEYS: usize = 128;

/// Minimum number of keys per non-root node. Nodes below this threshold need rebalancing.
const MIN_KEYS: usize = MAX_KEYS / 2;

/// Internal node: keys separating child subtrees, child node IDs, parent pointer.
struct InternalNode {
    keys: Vec<Vec<u8>>,
    children: Vec<u64>,
    parent: Option<u64>,
}

/// Leaf node: indexed values mapped to primary keys, with next-leaf pointer and parent pointer.
struct LeafNode {
    keys: Vec<Vec<u8>>,
    values: Vec<Vec<Vec<u8>>>,
    next: Option<u64>,
    parent: Option<u64>,
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

    fn parent_id(&self) -> Option<u64> {
        match self {
            Node::Internal(n) => n.parent,
            Node::Leaf(n) => n.parent,
        }
    }

    fn set_parent(&mut self, pid: Option<u64>) {
        match self {
            Node::Internal(n) => n.parent = pid,
            Node::Leaf(n) => n.parent = pid,
        }
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
/// - O(1) node access via HashMap storage
/// - O(1) parent lookup via parent pointers
pub struct BPlusTree {
    pub class: String,
    pub column: String,
    nodes: HashMap<u64, Node>,
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
            parent: None,
        });
        let mut nodes = HashMap::new();
        nodes.insert(root_id, root_node);
        Self {
            class: class.to_string(),
            column: column.to_string(),
            nodes,
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
        self.nodes.get(&id)
    }

    #[allow(dead_code)]
    fn get_node_mut(&mut self, id: u64) -> Option<&mut Node> {
        self.nodes.get_mut(&id)
    }

    /// Get node reference with error instead of panic.
    fn node(&self, id: u64) -> Result<&Node, String> {
        self.nodes
            .get(&id)
            .ok_or_else(|| format!("B+Tree node {} not found", id))
    }

    /// Get mutable node reference with error instead of panic.
    fn node_mut(&mut self, id: u64) -> Result<&mut Node, String> {
        self.nodes
            .get_mut(&id)
            .ok_or_else(|| format!("B+Tree node {} not found", id))
    }

    // ══════════════════════════════════════════════════════════════�?
    //  Insert
    // ══════════════════════════════════════════════════════════════�?

    /// Inserts an index entry: value -> primary_key.
    /// If the value already exists, the primary key is added to the existing set.
    pub fn insert(&mut self, key: Vec<u8>, primary_key: Vec<u8>) {
        let result = self.insert_recursive(self.root, &key, primary_key);
        if let Some((promoted_key, new_sibling_id)) = result {
            let new_root_id = self.alloc_id();
            let old_root = self.root;
            // Update children's parent pointers
            self.node_mut(old_root)
                .expect("node should exist")
                .set_parent(Some(new_root_id));
            self.node_mut(new_sibling_id)
                .expect("node should exist")
                .set_parent(Some(new_root_id));
            let new_root = Node::Internal(InternalNode {
                keys: vec![promoted_key],
                children: vec![old_root, new_sibling_id],
                parent: None,
            });
            self.nodes.insert(new_root_id, new_root);
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
        let is_leaf = self.node(node_id).expect("node should exist").is_leaf();

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
        let idx = match self.node(leaf_id).expect("node should exist") {
            Node::Leaf(n) => n.keys.binary_search_by(|k| k.as_slice().cmp(key)),
            _ => unreachable!(),
        };

        match idx {
            Ok(i) => {
                // Key exists �?add primary_key if not duplicate
                if let Node::Leaf(n) = self.node_mut(leaf_id).expect("node should exist") {
                    if !n.values[i].contains(&primary_key) {
                        n.values[i].push(primary_key);
                    }
                }
                None
            }
            Err(i) => {
                // Insert new key at position i
                if let Node::Leaf(n) = self.node_mut(leaf_id).expect("node should exist") {
                    n.keys.insert(i, key.to_vec());
                    n.values.insert(i, vec![primary_key]);
                }
                // Split if over capacity
                if self.node(leaf_id).expect("node should exist").is_full() {
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
        let child_id = match self.node(node_id).expect("node should exist") {
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
            let insert_pos = match self.node(node_id).expect("node should exist") {
                Node::Internal(n) => n
                    .keys
                    .binary_search_by(|k| k.as_slice().cmp(&promoted_key))
                    .unwrap_or_else(|i| i),
                _ => unreachable!(),
            };

            // Insert promoted key and new child pointer
            if let Node::Internal(n) = self.node_mut(node_id).expect("node should exist") {
                n.keys.insert(insert_pos, promoted_key);
                n.children.insert(insert_pos + 1, new_child_id);
            }

            // Set parent pointer for new child
            self.node_mut(new_child_id)
                .expect("node should exist")
                .set_parent(Some(node_id));

            // Split if over capacity
            if self.node(node_id).expect("node should exist").is_full() {
                Some(self.split_internal(node_id))
            } else {
                None
            }
        } else {
            None
        }
    }

    // ══════════════════════════════════════════════════════════════�?
    //  Split
    // ══════════════════════════════════════════════════════════════�?

    /// Splits a leaf node. Returns (promoted_key, new_sibling_id).
    fn split_leaf(&mut self, leaf_id: u64) -> (Vec<u8>, u64) {
        let mid = MAX_KEYS / 2;
        let new_id = self.alloc_id();
        let parent = self.node(leaf_id).expect("node should exist").parent_id();

        let (promoted_key, new_leaf) = match self.node_mut(leaf_id).expect("node should exist") {
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
                        parent,
                    }),
                )
            }
            _ => unreachable!(),
        };

        self.nodes.insert(new_id, new_leaf);
        (promoted_key, new_id)
    }

    /// Splits an internal node. Returns (promoted_key, new_sibling_id).
    fn split_internal(&mut self, node_id: u64) -> (Vec<u8>, u64) {
        let mid = MAX_KEYS / 2;
        let new_id = self.alloc_id();
        let parent = self.node(node_id).expect("node should exist").parent_id();

        let (promoted_key, new_internal) = match self.node_mut(node_id).expect("node should exist")
        {
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
                        parent,
                    }),
                )
            }
            _ => unreachable!(),
        };

        // Update parent pointers for children of the new internal node
        let new_children: Vec<u64> = match self.nodes.get(&new_id).expect("node should exist") {
            Node::Internal(n) => n.children.clone(),
            _ => unreachable!(),
        };
        for &child_id in &new_children {
            self.node_mut(child_id)
                .expect("node should exist")
                .set_parent(Some(new_id));
        }

        self.nodes.insert(new_id, new_internal);
        (promoted_key, new_id)
    }

    // ══════════════════════════════════════════════════════════════�?
    //  Delete
    // ══════════════════════════════════════════════════════════════�?

    /// Removes a primary_key from the set for the given value.
    /// If the set becomes empty, the key entry is removed.
    /// Handles underflow by redistributing or merging with siblings.
    pub fn remove(&mut self, value: &[u8], primary_key: &[u8]) {
        let leaf_id = self.find_leaf(value);
        let mut key_removed = false;

        // Remove the primary key from the leaf
        if let Node::Leaf(n) = self.node_mut(leaf_id).expect("node should exist") {
            if let Ok(i) = n.keys.binary_search_by(|k| k.as_slice().cmp(value)) {
                let was_present = n.values[i].iter().any(|pk| pk.as_slice() == primary_key);
                if was_present {
                    n.values[i].retain(|pk| pk.as_slice() != primary_key);
                    if n.values[i].is_empty() {
                        n.keys.remove(i);
                        n.values.remove(i);
                        key_removed = true;
                    }
                    self.len -= 1;
                }
            }
        }

        // If a key was removed from the leaf, check for underflow
        if key_removed && leaf_id != self.root {
            let key_count = self.node(leaf_id).expect("node should exist").key_count();
            if key_count < MIN_KEYS {
                self.handle_leaf_underflow(leaf_id);
            }
        }

        // If root is an internal node with no keys, make its only child the new root
        if !self.node(self.root).expect("node should exist").is_leaf() {
            if let Node::Internal(n) = self.node(self.root).expect("node should exist") {
                if n.keys.is_empty() && !n.children.is_empty() {
                    let new_root = n.children[0];
                    self.root = new_root;
                    self.node_mut(self.root)
                        .expect("node should exist")
                        .set_parent(None);
                }
            }
        }
    }

    /// Handles underflow in a leaf node by borrowing from siblings or merging.
    fn handle_leaf_underflow(&mut self, leaf_id: u64) {
        // O(1) parent lookup via parent pointer
        let (parent_id, child_idx) =
            match self.node(leaf_id).expect("node should exist").parent_id() {
                Some(pid) => {
                    let idx = match self.node(pid).expect("node should exist") {
                        Node::Internal(n) => {
                            n.children.iter().position(|&c| c == leaf_id).unwrap_or(0)
                        }
                        _ => return,
                    };
                    (pid, idx)
                }
                None => return,
            };

        let num_children = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => n.children.len(),
            _ => return,
        };

        // Try to borrow from left sibling
        if child_idx > 0 {
            let left_sibling_id = match self.node(parent_id).expect("node should exist") {
                Node::Internal(n) => n.children[child_idx - 1],
                _ => return,
            };
            let left_key_count = self
                .node(left_sibling_id)
                .expect("node should exist")
                .key_count();
            if left_key_count > MIN_KEYS {
                self.redistribute_leaf_from_left(parent_id, child_idx);
                return;
            }
        }

        // Try to borrow from right sibling
        if child_idx < num_children - 1 {
            let right_sibling_id = match self.node(parent_id).expect("node should exist") {
                Node::Internal(n) => n.children[child_idx + 1],
                _ => return,
            };
            let right_key_count = self
                .node(right_sibling_id)
                .expect("node should exist")
                .key_count();
            if right_key_count > MIN_KEYS {
                self.redistribute_leaf_from_right(parent_id, child_idx);
                return;
            }
        }

        // Merge with a sibling
        if child_idx > 0 {
            // Merge with left sibling
            self.merge_leaves(parent_id, child_idx - 1, child_idx);
        } else if num_children > 1 {
            // Merge with right sibling
            self.merge_leaves(parent_id, child_idx, child_idx + 1);
        }
    }

    /// Redistributes keys from left sibling to the underflowing leaf.
    fn redistribute_leaf_from_left(&mut self, parent_id: u64, child_idx: usize) {
        
        
        let (left_sibling_id, leaf_id) = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => {
                (n.children[child_idx - 1], n.children[child_idx])
            }
            _ => return,
        };

        // Move last key from left sibling to front of current leaf
        if let Node::Leaf(left) = self.node_mut(left_sibling_id).expect("node should exist") {
            let moved_key = left.keys.pop().expect("node should exist");
            let moved_values = left.values.pop().expect("node should exist");

            if let Node::Leaf(leaf) = self.node_mut(leaf_id).expect("node should exist") {
                leaf.keys.insert(0, moved_key);
                leaf.values.insert(0, moved_values);
            }
        }

        // Update parent separator key to the new first key of the current leaf
        if let Node::Leaf(leaf) = self.node(leaf_id).expect("node should exist") {
            let new_separator = leaf.keys.first().cloned().unwrap_or_default();
            if let Node::Internal(parent) = self.node_mut(parent_id).expect("node should exist") {
                parent.keys[child_idx - 1] = new_separator;
            }
        }
    }

    /// Redistributes keys from right sibling to the underflowing leaf.
    fn redistribute_leaf_from_right(&mut self, parent_id: u64, child_idx: usize) {
        
        
        let (leaf_id, right_sibling_id) = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => {
                (n.children[child_idx], n.children[child_idx + 1])
            }
            _ => return,
        };

        // Move first key from right sibling to end of current leaf
        if let Node::Leaf(right) = self.node_mut(right_sibling_id).expect("node should exist") {
            let moved_key = right.keys.remove(0);
            let moved_values = right.values.remove(0);

            if let Node::Leaf(leaf) = self.node_mut(leaf_id).expect("node should exist") {
                leaf.keys.push(moved_key);
                leaf.values.push(moved_values);
            }
        }

        // Update parent separator key to the new first key of right sibling
        if let Node::Leaf(right) = self.node(right_sibling_id).expect("node should exist") {
            let new_separator = right.keys.first().cloned().unwrap_or_default();
            if let Node::Internal(parent) = self.node_mut(parent_id).expect("node should exist") {
                parent.keys[child_idx] = new_separator;
            }
        }
    }

    /// Merges two adjacent leaf nodes. The left leaf absorbs the right leaf.
    fn merge_leaves(&mut self, parent_id: u64, left_idx: usize, right_idx: usize) {
        
        
        let (left_id, right_id) = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => {
                (n.children[left_idx], n.children[right_idx])
            }
            _ => return,
        };

        // Remove right node from storage and take its data (no clone needed)
        if let Some(Node::Leaf(right)) = self.nodes.remove(&right_id) {
            if let Node::Leaf(left) = self.node_mut(left_id).expect("node should exist") {
                left.keys.extend(right.keys);
                left.values.extend(right.values);
                left.next = right.next;
            }
        } else {
            return;
        }

        // Remove right sibling from parent
        if let Node::Internal(parent) = self.node_mut(parent_id).expect("node should exist") {
            parent.keys.remove(left_idx);
            parent.children.remove(right_idx);
        }

        // Check if parent now underflows
        if parent_id != self.root {
            let parent_key_count = self.node(parent_id).expect("node should exist").key_count();
            if parent_key_count < MIN_KEYS {
                self.handle_internal_underflow(parent_id);
            }
        }
    }

    /// Handles underflow in an internal node.
    fn handle_internal_underflow(&mut self, node_id: u64) {
        // O(1) parent lookup via parent pointer
        let (parent_id, child_idx) =
            match self.node(node_id).expect("node should exist").parent_id() {
                Some(pid) => {
                    let idx = match self.node(pid).expect("node should exist") {
                        Node::Internal(n) => {
                            n.children.iter().position(|&c| c == node_id).unwrap_or(0)
                        }
                        _ => return,
                    };
                    (pid, idx)
                }
                None => return,
            };

        let num_children = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => n.children.len(),
            _ => return,
        };

        // Try to borrow from left sibling
        if child_idx > 0 {
            let left_sibling_id = match self.node(parent_id).expect("node should exist") {
                Node::Internal(n) => n.children[child_idx - 1],
                _ => return,
            };
            let left_key_count = self
                .node(left_sibling_id)
                .expect("node should exist")
                .key_count();
            if left_key_count > MIN_KEYS {
                self.redistribute_internal_from_left(parent_id, child_idx);
                return;
            }
        }

        // Try to borrow from right sibling
        if child_idx < num_children - 1 {
            let right_sibling_id = match self.node(parent_id).expect("node should exist") {
                Node::Internal(n) => n.children[child_idx + 1],
                _ => return,
            };
            let right_key_count = self
                .node(right_sibling_id)
                .expect("node should exist")
                .key_count();
            if right_key_count > MIN_KEYS {
                self.redistribute_internal_from_right(parent_id, child_idx);
                return;
            }
        }

        // Merge with a sibling
        if child_idx > 0 {
            self.merge_internals(parent_id, child_idx - 1, child_idx);
        } else if num_children > 1 {
            self.merge_internals(parent_id, child_idx, child_idx + 1);
        }
    }

    /// Redistributes keys from left internal sibling.
    fn redistribute_internal_from_left(&mut self, parent_id: u64, child_idx: usize) {
        
        
        let (left_id, node_id) = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => {
                (n.children[child_idx - 1], n.children[child_idx])
            }
            _ => return,
        };

        // Get separator from parent
        let separator = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => n.keys[child_idx - 1].clone(),
            _ => return,
        };

        // Move last child and key from left sibling
        if let Node::Internal(left) = self.node_mut(left_id).expect("node should exist") {
            let moved_key = left.keys.pop().expect("node should exist");
            let moved_child = left.children.pop().expect("node should exist");

            if let Node::Internal(node) = self.node_mut(node_id).expect("node should exist") {
                node.keys.insert(0, separator);
                node.children.insert(0, moved_child);
            }

            // Update moved child's parent pointer
            self.node_mut(moved_child)
                .expect("node should exist")
                .set_parent(Some(node_id));

            // Update parent separator
            if let Node::Internal(parent) = self.node_mut(parent_id).expect("node should exist") {
                parent.keys[child_idx - 1] = moved_key;
            }
        }
    }

    /// Redistributes keys from right internal sibling.
    fn redistribute_internal_from_right(&mut self, parent_id: u64, child_idx: usize) {
        
        
        let (node_id, right_id) = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => {
                (n.children[child_idx], n.children[child_idx + 1])
            }
            _ => return,
        };

        // Get separator from parent
        let separator = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => n.keys[child_idx].clone(),
            _ => return,
        };

        // Move first child and key from right sibling
        if let Node::Internal(right) = self.node_mut(right_id).expect("node should exist") {
            let moved_key = right.keys.remove(0);
            let moved_child = right.children.remove(0);

            if let Node::Internal(node) = self.node_mut(node_id).expect("node should exist") {
                node.keys.push(separator);
                node.children.push(moved_child);
            }

            // Update moved child's parent pointer
            self.node_mut(moved_child)
                .expect("node should exist")
                .set_parent(Some(node_id));

            // Update parent separator
            if let Node::Internal(parent) = self.node_mut(parent_id).expect("node should exist") {
                parent.keys[child_idx] = moved_key;
            }
        }
    }

    /// Merges two adjacent internal nodes. The left node absorbs the right node.
    fn merge_internals(&mut self, parent_id: u64, left_idx: usize, right_idx: usize) {
        
        
        let (left_id, right_id) = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => {
                (n.children[left_idx], n.children[right_idx])
            }
            _ => return,
        };

        // Get separator from parent
        let separator = match self.node(parent_id).expect("node should exist") {
            Node::Internal(n) => n.keys[left_idx].clone(),
            _ => return,
        };

        // Remove right node from storage and take its data (no clone needed)
        if let Some(Node::Internal(right)) = self.nodes.remove(&right_id) {
            // Update parent pointers for right's children to point to left
            for &child_id in &right.children {
                self.node_mut(child_id)
                    .expect("node should exist")
                    .set_parent(Some(left_id));
            }

            if let Node::Internal(left) = self.node_mut(left_id).expect("node should exist") {
                left.keys.push(separator);
                left.keys.extend(right.keys);
                left.children.extend(right.children);
            }
        } else {
            return;
        }

        // Remove right node from parent
        if let Node::Internal(parent) = self.node_mut(parent_id).expect("node should exist") {
            parent.keys.remove(left_idx);
            parent.children.remove(right_idx);
        }

        // Check if parent now underflows
        if parent_id != self.root {
            let parent_key_count = self.node(parent_id).expect("node should exist").key_count();
            if parent_key_count < MIN_KEYS {
                self.handle_internal_underflow(parent_id);
            }
        }
    }

    // ══════════════════════════════════════════════════════════════�?
    //  Lookup
    // ══════════════════════════════════════════════════════════════�?

    /// Point lookup: returns all primary keys with the given value.
    pub fn lookup(&self, value: &[u8]) -> Vec<Vec<u8>> {
        let leaf_id = self.find_leaf(value);

        if let Node::Leaf(n) = self.node(leaf_id).expect("node should exist") {
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
            match self.node(current).expect("node should exist") {
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

    // ══════════════════════════════════════════════════════════════�?
    //  Range scans via leaf chain
    // ══════════════════════════════════════════════════════════════�?

    /// Range scan: returns all primary keys with values in [low, high].
    /// If low is None, scans from the beginning.
    /// If high is None, scans to the end.
    pub fn range_scan(&self, low: Option<&[u8]>, high: Option<&[u8]>) -> Vec<Vec<u8>> {
        let mut result = Vec::new();

        let mut cursor = match low {
            Some(k) => self.cursor_lower_bound(k),
            None => self.cursor_first(),
        };

        while cursor.is_valid(self) {
            let leaf = match self.node(cursor.leaf_id).expect("node should exist") {
                Node::Leaf(n) => n,
                _ => break,
            };
            if cursor.idx >= leaf.keys.len() {
                cursor.advance(self);
                continue;
            }

            let k = leaf.keys[cursor.idx].as_slice();
            if high.is_some_and(|h| k > h) {
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
            let leaf = match self.node(cursor.leaf_id).expect("node should exist") {
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
            let leaf = match self.node(cursor.leaf_id).expect("node should exist") {
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
            let leaf = match self.node(cursor.leaf_id).expect("node should exist") {
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
            let leaf = match self.node(cursor.leaf_id).expect("node should exist") {
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

    // ══════════════════════════════════════════════════════════════�?
    //  Cursor helpers
    // ══════════════════════════════════════════════════════════════�?

    /// Cursor to the first entry (leftmost leaf).
    fn cursor_first(&self) -> Cursor {
        let mut current = self.root;
        loop {
            match self.node(current).expect("node should exist") {
                Node::Leaf(_) => {
                    return Cursor {
                        leaf_id: current,
                        idx: 0,
                    }
                }
                Node::Internal(n) => current = n.children[0],
            }
        }
    }

    /// Cursor to the first entry with key >= target.
    fn cursor_lower_bound(&self, target: &[u8]) -> Cursor {
        let leaf_id = self.find_leaf(target);
        if let Node::Leaf(n) = self.node(leaf_id).expect("node should exist") {
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
        if let Node::Leaf(n) = self.node(leaf_id).expect("node should exist") {
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
        if let Node::Leaf(n) = tree.get_node(self.leaf_id).expect("node should exist") {
            if let Some(next_id) = n.next {
                self.leaf_id = next_id;
                self.idx = 0;
            } else {
                // End of chain �?mark invalid by using a non-existent id
                self.leaf_id = u64::MAX;
                self.idx = 0;
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════�?
//  Tests
// ══════════════════════════════════════════════════════════════�?

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
    fn test_remove_nonexistent_key() {
        let mut tree = BPlusTree::new("Product", "price");
        tree.insert(b"100".to_vec(), b"pk1".to_vec());
        tree.remove(b"999", b"pk999"); // does not exist
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.lookup(b"100").len(), 1);
    }

    #[test]
    fn test_remove_leaf_underflow_merge() {
        let mut tree = BPlusTree::new("Product", "price");

        let n = 400u32;
        for i in 0..n {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }
        assert_eq!(tree.len(), n as usize);

        // Remove most entries to trigger underflow and merges
        for i in 0..(n - 5) {
            let key = format!("{:010}", i);
            tree.remove(key.as_bytes(), format!("pk_{}", i).as_bytes());
        }
        assert_eq!(tree.len(), 5);

        // Verify remaining entries
        for i in (n - 5)..n {
            let key = format!("{:010}", i);
            let keys = tree.lookup(key.as_bytes());
            assert_eq!(keys.len(), 1, "key {} should still exist", key);
        }

        // Verify leaf chain integrity after merges
        let mut cursor = tree.cursor_first();
        let mut prev_key: Vec<u8> = Vec::new();
        let mut count = 0usize;
        while cursor.is_valid(&tree) {
            if let Node::Leaf(leaf) = tree.get_node(cursor.leaf_id).expect("node should exist") {
                if cursor.idx >= leaf.keys.len() {
                    cursor.advance(&tree);
                    continue;
                }
                let k = &leaf.keys[cursor.idx];
                assert!(
                    k.as_slice() > prev_key.as_slice(),
                    "keys must be sorted after merge"
                );
                prev_key = k.clone();
                count += 1;
                cursor.idx += 1;
            }
        }
        assert_eq!(count, 5);
    }

    #[test]
    fn test_remove_triggers_redistribution() {
        let mut tree = BPlusTree::new("Product", "price");

        let n = 500u32;
        for i in 0..n {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }

        // Remove a contiguous range to underflow one leaf
        for i in 100..150 {
            let key = format!("{:010}", i);
            tree.remove(key.as_bytes(), format!("pk_{}", i).as_bytes());
        }

        // Verify remaining entries
        for i in 0..100u32 {
            let key = format!("{:010}", i);
            assert_eq!(tree.lookup(key.as_bytes()).len(), 1, "key {} missing", key);
        }
        for i in 150..n {
            let key = format!("{:010}", i);
            assert_eq!(tree.lookup(key.as_bytes()).len(), 1, "key {} missing", key);
        }
        assert_eq!(tree.len(), 450);
    }

    #[test]
    fn test_remove_root_collapse() {
        let mut tree = BPlusTree::new("Product", "price");

        let n = 600u32;
        for i in 0..n {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }

        // Root should be internal
        assert!(!tree
            .get_node(tree.root)
            .expect("node should exist")
            .is_leaf());

        // Remove all but a few entries �?root should collapse back to leaf
        for i in 0..(n - 3) {
            let key = format!("{:010}", i);
            tree.remove(key.as_bytes(), format!("pk_{}", i).as_bytes());
        }

        assert_eq!(tree.len(), 3);
        // Root should now be a leaf after collapse
        assert!(tree
            .get_node(tree.root)
            .expect("node should exist")
            .is_leaf());

        // Verify remaining
        for i in (n - 3)..n {
            let key = format!("{:010}", i);
            assert_eq!(tree.lookup(key.as_bytes()).len(), 1);
        }
    }

    #[test]
    fn test_remove_all_entries() {
        let mut tree = BPlusTree::new("Product", "price");

        for i in 0..300u32 {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }

        // Remove everything
        for i in 0..300u32 {
            let key = format!("{:010}", i);
            tree.remove(key.as_bytes(), format!("pk_{}", i).as_bytes());
        }

        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
        // Root should be a leaf (empty tree)
        assert!(tree
            .get_node(tree.root)
            .expect("node should exist")
            .is_leaf());
    }

    #[test]
    fn test_remove_cascading_underflow() {
        let mut tree = BPlusTree::new("Product", "price");

        let n = 1000u32;
        for i in 0..n {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }

        // Remove a large contiguous block to trigger cascading underflows
        for i in 200..800 {
            let key = format!("{:010}", i);
            tree.remove(key.as_bytes(), format!("pk_{}", i).as_bytes());
        }

        assert_eq!(tree.len(), 400);

        // Verify all remaining entries via point lookup
        for i in 0..200u32 {
            let key = format!("{:010}", i);
            assert_eq!(tree.lookup(key.as_bytes()).len(), 1, "key {} missing", key);
        }
        for i in 800..n {
            let key = format!("{:010}", i);
            assert_eq!(tree.lookup(key.as_bytes()).len(), 1, "key {} missing", key);
        }

        // Range scan should still work
        let keys = tree.range_scan(Some(b"0000000050"), Some(b"0000000149"));
        assert_eq!(keys.len(), 100, "range [50,149] failed, got {}", keys.len());

        let keys = tree.range_scan(Some(b"0000000850"), Some(b"0000000949"));
        assert_eq!(
            keys.len(),
            100,
            "range [850,949] failed, got {}",
            keys.len()
        );
    }

    #[test]
    fn test_remove_interleaved_insert_delete() {
        let mut tree = BPlusTree::new("Product", "price");

        for i in 0..200u32 {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }

        // Remove even keys
        for i in (0..200u32).step_by(2) {
            let key = format!("{:010}", i);
            tree.remove(key.as_bytes(), format!("pk_{}", i).as_bytes());
        }
        assert_eq!(tree.len(), 100);

        // Re-insert them with different pks
        for i in (0..200u32).step_by(2) {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("new_pk_{}", i).into_bytes());
        }
        assert_eq!(tree.len(), 200);

        // Verify all entries
        for i in 0..200u32 {
            let key = format!("{:010}", i);
            let pks = tree.lookup(key.as_bytes());
            assert_eq!(pks.len(), 1, "key {} should have 1 pk", key);
            if i % 2 == 0 {
                assert_eq!(pks[0], format!("new_pk_{}", i).as_bytes());
            } else {
                assert_eq!(pks[0], format!("pk_{}", i).as_bytes());
            }
        }
    }

    #[test]
    fn test_remove_multiple_pks_then_key() {
        let mut tree = BPlusTree::new("Order", "status");
        tree.insert(b"active".to_vec(), b"order_1".to_vec());
        tree.insert(b"active".to_vec(), b"order_2".to_vec());
        tree.insert(b"active".to_vec(), b"order_3".to_vec());

        // Remove one pk �?key still present
        tree.remove(b"active", b"order_2");
        assert_eq!(tree.lookup(b"active").len(), 2);

        // Remove another �?key still present
        tree.remove(b"active", b"order_1");
        assert_eq!(tree.lookup(b"active").len(), 1);

        // Remove last pk �?key entry should be removed
        tree.remove(b"active", b"order_3");
        assert_eq!(tree.lookup(b"active").len(), 0);
        assert_eq!(tree.len(), 0);
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
            if let Node::Leaf(leaf) = tree.get_node(cursor.leaf_id).expect("node should exist") {
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

    #[test]
    fn test_parent_pointers_after_split() {
        let mut tree = BPlusTree::new("Product", "price");

        // Insert enough to trigger multiple splits
        for i in 0..500u32 {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }

        // Verify root has no parent
        assert_eq!(
            tree.get_node(tree.root)
                .expect("node should exist")
                .parent_id(),
            None
        );

        // Verify all non-root nodes have valid parent pointers
        let root = tree.root;
        fn verify_parents(tree: &BPlusTree, node_id: u64, expected_parent: Option<u64>) {
            let node = tree.get_node(node_id).expect("node should exist");
            assert_eq!(
                node.parent_id(),
                expected_parent,
                "node {} has wrong parent",
                node_id
            );
            if let Node::Internal(n) = node {
                for &child_id in &n.children {
                    verify_parents(tree, child_id, Some(node_id));
                }
            }
        }
        verify_parents(&tree, root, None);
    }

    #[test]
    fn test_parent_pointers_after_merge() {
        let mut tree = BPlusTree::new("Product", "price");

        let n = 400u32;
        for i in 0..n {
            let key = format!("{:010}", i);
            tree.insert(key.into_bytes(), format!("pk_{}", i).into_bytes());
        }

        // Remove most entries to trigger merges
        for i in 0..(n - 5) {
            let key = format!("{:010}", i);
            tree.remove(key.as_bytes(), format!("pk_{}", i).as_bytes());
        }

        // Verify parent pointers still correct
        let root = tree.root;
        fn verify_parents(tree: &BPlusTree, node_id: u64, expected_parent: Option<u64>) {
            let node = tree.get_node(node_id).expect("node should exist");
            assert_eq!(
                node.parent_id(),
                expected_parent,
                "node {} has wrong parent",
                node_id
            );
            if let Node::Internal(n) = node {
                for &child_id in &n.children {
                    verify_parents(tree, child_id, Some(node_id));
                }
            }
        }
        verify_parents(&tree, root, None);
    }
}
