//! Account data structures

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Account public key (32 bytes, like Ed25519)
pub type Pubkey = [u8; 32];

/// Account state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// Lamports (SOL balance, 1 SOL = 10^9 lamports)
    pub lamports: u64,
    /// Owner program
    pub owner: Pubkey,
    /// Whether the account is executable (a program)
    pub executable: bool,
    /// Epoch at which rent was last calculated
    pub rent_epoch: u64,
    /// Account data
    pub data: AccountData,
}

/// Account data payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccountData {
    /// System account (wallet) — just balance
    System,
    /// Program account (executable)
    Program(Vec<u8>),
    /// Token account (spl-token like)
    Token {
        mint: Pubkey,
        authority: Pubkey,
        amount: u64,
    },
    /// Generic data account
    Data(Vec<u8>),
}

impl Account {
    /// Create a new system account with SOL
    pub fn new_system_account(_pubkey: Pubkey, lamports: u64) -> Self {
        Self {
            lamports,
            owner: system_program_id(),
            executable: false,
            rent_epoch: 0,
            data: AccountData::System,
        }
    }

    /// Create a new program account
    pub fn new_program_account(_pubkey: Pubkey, owner: Pubkey, code: Vec<u8>) -> Self {
        Self {
            lamports: 0,
            owner,
            executable: true,
            rent_epoch: 0,
            data: AccountData::Program(code),
        }
    }

    /// Create a token account
    pub fn new_token_account(
        _pubkey: Pubkey,
        mint: Pubkey,
        authority: Pubkey,
        amount: u64,
    ) -> Self {
        Self {
            lamports: 0,
            owner: token_program_id(),
            executable: false,
            rent_epoch: 0,
            data: AccountData::Token {
                mint,
                authority,
                amount,
            },
        }
    }

    /// Compute hash of this account (for Merkle tree)
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.lamports.to_le_bytes());
        hasher.update(self.owner);
        hasher.update([self.executable as u8]);
        hasher.update(self.rent_epoch.to_le_bytes());

        match &self.data {
            AccountData::System => hasher.update(b"system"),
            AccountData::Program(code) => {
                hasher.update(b"program");
                hasher.update(code);
            }
            AccountData::Token {
                mint,
                authority,
                amount,
            } => {
                hasher.update(b"token");
                hasher.update(mint);
                hasher.update(authority);
                hasher.update(amount.to_le_bytes());
            }
            AccountData::Data(data) => {
                hasher.update(b"data");
                hasher.update(data);
            }
        }

        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        bytes
    }

    /// Get size in bytes
    pub fn size(&self) -> usize {
        std::mem::size_of::<u64>()  // lamports
            + 32                       // owner
            + 1                        // executable
            + 8                        // rent_epoch
            + match &self.data {
                AccountData::System => 0,
                AccountData::Program(code) => code.len(),
                AccountData::Token { .. } => 32 + 32 + 8,
                AccountData::Data(d) => d.len(),
            }
    }
}

/// Get the system program ID
pub fn system_program_id() -> Pubkey {
    // 11111111111111111111111111111111 (Solana system program)
    [1u8; 32]
}

/// Get the token program ID
pub fn token_program_id() -> Pubkey {
    // TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
    [2u8; 32]
}

/// Generate a random pubkey (for testing)
pub fn random_pubkey() -> Pubkey {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut key = [0u8; 32];
    rng.fill(&mut key);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_account() {
        let key = random_pubkey();
        let acc = Account::new_system_account(key, 1_000_000_000);
        assert_eq!(acc.lamports, 1_000_000_000);
        assert!(!acc.executable);
        assert_eq!(acc.owner, system_program_id());
    }

    #[test]
    fn test_account_hash_deterministic() {
        let key = random_pubkey();
        let acc1 = Account::new_system_account(key, 500);
        let acc2 = Account::new_system_account(key, 500);
        assert_eq!(acc1.hash(), acc2.hash());
    }

    #[test]
    fn test_account_hash_changes_with_balance() {
        let key = random_pubkey();
        let acc1 = Account::new_system_account(key, 100);
        let acc2 = Account::new_system_account(key, 200);
        assert_ne!(acc1.hash(), acc2.hash());
    }

    #[test]
    fn test_token_account() {
        let key = random_pubkey();
        let mint = random_pubkey();
        let authority = random_pubkey();
        let acc = Account::new_token_account(key, mint, authority, 1000);

        if let AccountData::Token {
            mint: m,
            authority: a,
            amount,
        } = &acc.data
        {
            assert_eq!(*m, mint);
            assert_eq!(*a, authority);
            assert_eq!(*amount, 1000);
        } else {
            panic!("Expected token account data");
        }
    }
}
