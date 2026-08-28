//! Neighborhoods — validator tree for block propagation
//!
//! Turbine organizes validators into a tree structure (neighborhoods)
//! for efficient block propagation. Each validator only communicates
//! with its neighbors, reducing bandwidth requirements.

use serde::{Deserialize, Serialize};

/// A validator in the Turbine tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurbineNode {
    /// Validator public key
    pub validator: [u8; 32],
    /// Stake weight
    pub stake: u64,
    /// Tree depth (0 = root/leader)
    pub depth: u8,
    /// Index in the tree
    pub index: usize,
    /// Parent index (None for root)
    pub parent: Option<usize>,
    /// Child indices
    pub children: Vec<usize>,
}

/// A neighborhood (subtree) in the Turbine tree
#[derive(Debug, Clone)]
pub struct Neighborhood {
    /// The root of this neighborhood
    pub root: TurbineNode,
    /// All nodes in this neighborhood
    pub nodes: Vec<TurbineNode>,
    /// Neighborhood depth (how many levels)
    pub depth: usize,
}

impl Neighborhood {
    /// Create a Turbine tree from a list of validators
    ///
    /// Validators are arranged in a balanced tree based on stake weight.
    /// Higher-stake validators are placed closer to the root.
    pub fn build_tree(
        mut validators: Vec<([u8; 32], u64)>, // (pubkey, stake)
        max_depth: usize,
    ) -> Self {
        // Sort by stake descending
        validators.sort_by_key(|b| std::cmp::Reverse(b.1));

        let total = validators.len();
        if total == 0 {
            return Self {
                root: TurbineNode {
                    validator: [0u8; 32],
                    stake: 0,
                    depth: 0,
                    index: 0,
                    parent: None,
                    children: vec![],
                },
                nodes: vec![],
                depth: 0,
            };
        }

        // Build tree using BFS
        let mut nodes = Vec::with_capacity(total);
        let mut queue = Vec::new(); // (parent_index, child_validator_indices)

        // Root is the highest-stake validator
        let root_node = TurbineNode {
            validator: validators[0].0,
            stake: validators[0].1,
            depth: 0,
            index: 0,
            parent: None,
            children: Vec::new(),
        };
        nodes.push(root_node);
        queue.push((0, (1..total).collect::<Vec<_>>()));

        let mut depth = 0;

        while !queue.is_empty() {
            let (parent_idx, remaining) = queue.remove(0);
            if remaining.is_empty() || nodes[parent_idx].depth as usize >= max_depth {
                continue;
            }

            let child_depth = nodes[parent_idx].depth + 1;
            depth = depth.max(child_depth as usize);

            // Fan-out: each node gets up to 3 children
            let fan_out = 3;
            let children: Vec<_> = remaining.iter().take(fan_out).cloned().collect();
            let next_remaining: Vec<_> = remaining.into_iter().skip(fan_out).collect();

            for &child_idx in &children {
                let child_node = TurbineNode {
                    validator: validators[child_idx].0,
                    stake: validators[child_idx].1,
                    depth: child_depth,
                    index: nodes.len(),
                    parent: Some(parent_idx),
                    children: Vec::new(),
                };

                let new_idx = nodes.len();
                nodes.push(child_node);

                // Update parent's children list
                if let Some(parent) = nodes.get_mut(parent_idx) {
                    parent.children.push(new_idx);
                }
            }

            // Distribute remaining validators among children
            if !next_remaining.is_empty() && !children.is_empty() {
                let per_child = next_remaining.len().div_ceil(children.len());
                for (i, &child_idx) in children.iter().enumerate() {
                    let start = i * per_child;
                    let end = std::cmp::min(start + per_child, next_remaining.len());
                    if start < next_remaining.len() {
                        queue.push((child_idx, next_remaining[start..end].to_vec()));
                    }
                }
            }
        }

        let root = nodes[0].clone();

        Self { root, nodes, depth }
    }

    /// Get the path from root to a specific validator
    pub fn path_to(&self, validator: &[u8; 32]) -> Option<Vec<&TurbineNode>> {
        let target_idx = self.nodes.iter().position(|n| n.validator == *validator)?;

        let mut path = Vec::new();
        let mut current = Some(target_idx);

        while let Some(idx) = current {
            path.push(&self.nodes[idx]);
            current = self.nodes[idx].parent;
        }

        path.reverse();
        Some(path)
    }

    /// Get neighbors of a validator (parent + children)
    pub fn neighbors(&self, validator: &[u8; 32]) -> Vec<&TurbineNode> {
        let node = self.nodes.iter().find(|n| n.validator == *validator);
        match node {
            Some(node) => {
                let mut neighbors = Vec::new();
                if let Some(parent_idx) = node.parent {
                    neighbors.push(&self.nodes[parent_idx]);
                }
                for &child_idx in &node.children {
                    neighbors.push(&self.nodes[child_idx]);
                }
                neighbors
            }
            None => vec![],
        }
    }

    /// Get validators at a specific depth
    pub fn at_depth(&self, depth: u8) -> Vec<&TurbineNode> {
        self.nodes.iter().filter(|n| n.depth == depth).collect()
    }

    /// Get total stake in the tree
    pub fn total_stake(&self) -> u64 {
        self.nodes.iter().map(|n| n.stake).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_validator(id: u8, stake: u64) -> ([u8; 32], u64) {
        let mut key = [0u8; 32];
        key[0] = id;
        (key, stake)
    }

    #[test]
    fn test_single_validator_tree() {
        let validators = vec![make_validator(1, 1000)];
        let tree = Neighborhood::build_tree(validators, 5);

        assert_eq!(tree.nodes.len(), 1);
        assert_eq!(tree.root.validator[0], 1);
    }

    #[test]
    fn test_multi_validator_tree() {
        let validators = vec![
            make_validator(1, 1000),
            make_validator(2, 800),
            make_validator(3, 600),
            make_validator(4, 400),
            make_validator(5, 200),
        ];

        let tree = Neighborhood::build_tree(validators, 5);

        // Root should be highest stake
        assert_eq!(tree.root.validator[0], 1);
        assert!(tree.nodes.len() >= 2);
    }

    #[test]
    fn test_path_to_validator() {
        let validators = vec![
            make_validator(1, 1000),
            make_validator(2, 800),
            make_validator(3, 600),
            make_validator(4, 400),
        ];

        let tree = Neighborhood::build_tree(validators, 5);
        let v3_key = [3u8; 32];

        if let Some(path) = tree.path_to(&v3_key) {
            assert!(!path.is_empty());
            assert_eq!(path.last().unwrap().validator, v3_key);
        }
    }

    #[test]
    fn test_neighbors() {
        let validators = vec![
            make_validator(1, 1000),
            make_validator(2, 800),
            make_validator(3, 600),
        ];

        let tree = Neighborhood::build_tree(validators, 5);
        // v1_key must match make_validator(1, ...)
        let mut v1_key = [0u8; 32];
        v1_key[0] = 1;

        let neighbors = tree.neighbors(&v1_key);
        // Root should have children as neighbors
        assert!(!neighbors.is_empty());
    }

    #[test]
    fn test_total_stake() {
        let validators = vec![
            make_validator(1, 1000),
            make_validator(2, 800),
            make_validator(3, 600),
        ];

        let tree = Neighborhood::build_tree(validators, 5);
        assert_eq!(tree.total_stake(), 2400);
    }
}
