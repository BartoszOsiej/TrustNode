//! Fork Choice — determines which fork to follow
//!
//! Solana's fork choice is a combination of:
//! 1. Tower voting (lockout-based)
//! 2. PoH chain weight
//! 3. Latest timestamp (tie-breaker)

use dashmap::DashMap;

/// A block in the fork tree
#[derive(Debug, Clone)]
pub struct ForkBlock {
    pub hash: [u8; 32],
    pub slot: u64,
    pub parent_hash: Option<[u8; 32]>,
    pub timestamp: u64,
    pub height: u64,
    pub voting_power: u64,
}

/// Fork choice rule implementation
pub struct ForkChoice {
    /// All known blocks
    blocks: DashMap<[u8; 32], ForkBlock>,
    /// The confirmed root
    root: parking_lot::RwLock<Option<ForkBlock>>,
    /// The current best head
    best_head: parking_lot::RwLock<Option<ForkBlock>>,
    /// Blocks per slot (for tie-breaking)
    slot_blocks: DashMap<u64, Vec<[u8; 32]>>,
}

impl ForkChoice {
    /// Create a new fork choice with a genesis block
    pub fn new(genesis: ForkBlock) -> Self {
        let hash = genesis.hash;
        let slot = genesis.slot;

        let fc = Self {
            blocks: DashMap::new(),
            root: parking_lot::RwLock::new(Some(genesis.clone())),
            best_head: parking_lot::RwLock::new(Some(genesis.clone())),
            slot_blocks: DashMap::new(),
        };

        fc.blocks.insert(hash, genesis);
        fc.slot_blocks.entry(slot).or_default().push(hash);

        fc
    }

    /// Add a block to the fork tree
    pub fn add_block(&self, block: ForkBlock) {
        let hash = block.hash;
        let slot = block.slot;

        self.blocks.insert(hash, block.clone());
        self.slot_blocks.entry(slot).or_default().push(hash);

        // Update best head if this block has more weight
        let best = self.best_head.read().clone();
        if let Some(current_best) = best {
            if block.voting_power > current_best.voting_power
                || (block.voting_power == current_best.voting_power
                    && block.timestamp > current_best.timestamp)
            {
                *self.best_head.write() = Some(block);
            }
        }
    }

    /// Get the best head (tip of the chain)
    pub fn best_head(&self) -> Option<ForkBlock> {
        self.best_head.read().clone()
    }

    /// Get the confirmed root
    pub fn root(&self) -> Option<ForkBlock> {
        self.root.read().clone()
    }

    /// Get all blocks at a specific slot
    pub fn blocks_at_slot(&self, slot: u64) -> Vec<ForkBlock> {
        self.slot_blocks
            .get(&slot)
            .map(|hashes| {
                hashes
                    .iter()
                    .filter_map(|h| self.blocks.get(h).map(|b| b.value().clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the heaviest fork at a slot (most voting power)
    pub fn heaviest_at_slot(&self, slot: u64) -> Option<ForkBlock> {
        self.blocks_at_slot(slot)
            .into_iter()
            .max_by_key(|b| b.voting_power)
    }

    /// Walk the chain from a block to the root
    pub fn chain_from(&self, block_hash: [u8; 32]) -> Vec<ForkBlock> {
        let mut chain = Vec::new();
        let mut current_hash = Some(block_hash);

        while let Some(hash) = current_hash {
            if let Some(block) = self.blocks.get(&hash) {
                chain.push(block.value().clone());
                current_hash = block.parent_hash;
            } else {
                break;
            }
        }

        chain.reverse();
        chain
    }

    /// Get the total voting power of a fork
    pub fn fork_weight(&self, tip_hash: [u8; 32]) -> u64 {
        self.chain_from(tip_hash)
            .iter()
            .map(|b| b.voting_power)
            .sum()
    }

    /// Prune blocks before the root
    pub fn prune(&self, before_slot: u64) {
        self.blocks.retain(|_, block| block.slot >= before_slot);
    }

    /// Get stats
    pub fn stats(&self) -> ForkChoiceStats {
        ForkChoiceStats {
            total_blocks: self.blocks.len(),
            total_slots: self.slot_blocks.len(),
        }
    }
}

/// Fork choice statistics
#[derive(Debug, Clone)]
pub struct ForkChoiceStats {
    pub total_blocks: usize,
    pub total_slots: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_block(slot: u64, parent: Option<[u8; 32]>, voting_power: u64) -> ForkBlock {
        let mut hash = [0u8; 32];
        hash[0] = slot as u8;
        hash[1] = voting_power as u8;

        ForkBlock {
            hash,
            slot,
            parent_hash: parent,
            timestamp: slot * 400,
            height: slot,
            voting_power,
        }
    }

    #[test]
    fn test_genesis_block() {
        let genesis = make_block(0, None, 0);
        let fc = ForkChoice::new(genesis.clone());

        assert_eq!(fc.best_head().unwrap().hash, genesis.hash);
        assert_eq!(fc.root().unwrap().hash, genesis.hash);
    }

    #[test]
    fn test_fork_choice() {
        let genesis = make_block(0, None, 0);
        let genesis_hash = genesis.hash;
        let fc = ForkChoice::new(genesis);

        // Fork A: lower voting power
        let block_a = make_block(1, Some(genesis_hash), 3);
        fc.add_block(block_a.clone());

        // Fork B: higher voting power
        let block_b = make_block(1, Some(genesis_hash), 5);
        fc.add_block(block_b.clone());

        // Fork B should win
        assert_eq!(fc.best_head().unwrap().hash, block_b.hash);
    }

    #[test]
    fn test_chain_walk() {
        let genesis = make_block(0, None, 0);
        let fc = ForkChoice::new(genesis.clone());

        let b1 = make_block(1, Some(genesis.hash), 1);
        fc.add_block(b1.clone());

        let b2 = make_block(2, Some(b1.hash), 2);
        fc.add_block(b2.clone());

        let chain = fc.chain_from(b2.hash);
        assert_eq!(chain.len(), 3); // genesis, b1, b2
        assert_eq!(chain[0].slot, 0);
        assert_eq!(chain[1].slot, 1);
        assert_eq!(chain[2].slot, 2);
    }
}
