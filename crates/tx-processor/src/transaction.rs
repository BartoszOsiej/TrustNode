//! Transaction data structures and validation

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_accounts::account::Pubkey;

/// A transaction in the Solana model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    /// Transaction signature (Ed25519)
    pub signature: Vec<u8>,
    /// Public key of the signer
    pub signer: Pubkey,
    /// Instructions to execute
    pub instructions: Vec<Instruction>,
    /// Recent blockhash (proof the tx is recent)
    pub recent_blockhash: [u8; 32],
    /// Max compute units the transaction can consume
    pub compute_budget: u64,
    /// Fee in lamports
    pub fee: u64,
}

/// A single instruction within a transaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instruction {
    /// Program to invoke
    pub program_id: Pubkey,
    /// Accounts this instruction reads (by index in tx)
    pub accounts: Vec<AccountMeta>,
    /// Instruction data
    pub data: Vec<u8>,
}

/// Account metadata for an instruction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountMeta {
    /// Index of the account in the transaction's account list
    pub index: u8,
    /// Whether this account is a signer
    pub is_signer: bool,
    /// Whether this account is writable
    pub is_writable: bool,
}

impl Transaction {
    /// Create a new transaction
    pub fn new(signer: Pubkey, instructions: Vec<Instruction>, recent_blockhash: [u8; 32]) -> Self {
        let fee = 5000; // Base fee: 5000 lamports (like Solana)
        Self {
            signature: Vec::new(),
            signer,
            instructions,
            recent_blockhash,
            compute_budget: 200_000,
            fee,
        }
    }

    /// Compute the transaction hash
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(&self.signer);
        hasher.update(&self.recent_blockhash);

        for ix in &self.instructions {
            hasher.update(&ix.program_id);
            hasher.update(&ix.data);
        }

        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        bytes
    }

    /// Get all accounts this transaction reads
    pub fn read_accounts(&self) -> Vec<u8> {
        self.instructions
            .iter()
            .flat_map(|ix| {
                ix.accounts
                    .iter()
                    .filter(|a| !a.is_writable)
                    .map(|a| a.index)
            })
            .collect()
    }

    /// Get all accounts this transaction writes
    pub fn write_accounts(&self) -> Vec<u8> {
        self.instructions
            .iter()
            .flat_map(|ix| {
                ix.accounts
                    .iter()
                    .filter(|a| a.is_writable)
                    .map(|a| a.index)
            })
            .collect()
    }

    /// Check if this transaction conflicts with another
    pub fn conflicts_with(&self, other: &Transaction) -> bool {
        let my_writes = self.write_accounts();
        let my_reads = self.read_accounts();
        let other_writes = other.write_accounts();
        let other_reads = other.read_accounts();

        // Conflict if either writes what the other reads or writes
        for w in &my_writes {
            if other_writes.contains(w) || other_reads.contains(w) {
                return true;
            }
        }
        for w in &other_writes {
            if my_writes.contains(w) || my_reads.contains(w) {
                return true;
            }
        }

        false
    }

    /// Validate basic transaction properties
    pub fn validate(&self) -> Result<(), TransactionError> {
        if self.instructions.is_empty() {
            return Err(TransactionError::NoInstructions);
        }

        if self.compute_budget == 0 {
            return Err(TransactionError::ZeroComputeBudget);
        }

        // Verify fee is paid
        if self.fee < 5000 {
            return Err(TransactionError::InsufficientFee);
        }

        Ok(())
    }

    /// Verify the signature
    pub fn verify_signature(&self) -> bool {
        // In a real implementation, we'd verify Ed25519 signature
        // For now, just check it exists
        !self.signature.is_empty()
    }
}

/// Result of executing a transaction
#[derive(Debug, Clone)]
pub struct TransactionResult {
    pub success: bool,
    pub compute_units_consumed: u64,
    pub fee_paid: u64,
    pub error: Option<TransactionError>,
    pub logs: Vec<String>,
}

impl TransactionResult {
    pub fn success(compute_units: u64, fee: u64) -> Self {
        Self {
            success: true,
            compute_units_consumed: compute_units,
            fee_paid: fee,
            error: None,
            logs: vec!["Transaction executed successfully".to_string()],
        }
    }

    pub fn failure(error: TransactionError, compute_units: u64) -> Self {
        let error_msg = format!("Transaction failed: {:?}", error);
        Self {
            success: false,
            compute_units_consumed: compute_units,
            fee_paid: 0,
            error: Some(error),
            logs: vec![error_msg],
        }
    }
}

/// Transaction errors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransactionError {
    NoInstructions,
    ZeroComputeBudget,
    InsufficientFee,
    InvalidSignature,
    AccountNotFound(u8),
    InsufficientLamports { needed: u64, available: u64 },
    ProgramError(String),
    ComputeBudgetExceeded,
    BlockhashNotFound,
    DuplicateTransaction,
}

impl std::fmt::Display for TransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoInstructions => write!(f, "No instructions"),
            Self::ZeroComputeBudget => write!(f, "Zero compute budget"),
            Self::InsufficientFee => write!(f, "Insufficient fee"),
            Self::InvalidSignature => write!(f, "Invalid signature"),
            Self::AccountNotFound(idx) => write!(f, "Account {} not found", idx),
            Self::InsufficientLamports { needed, available } => {
                write!(
                    f,
                    "Insufficient lamports: need {}, have {}",
                    needed, available
                )
            }
            Self::ProgramError(msg) => write!(f, "Program error: {}", msg),
            Self::ComputeBudgetExceeded => write!(f, "Compute budget exceeded"),
            Self::BlockhashNotFound => write!(f, "Blockhash not found"),
            Self::DuplicateTransaction => write!(f, "Duplicate transaction"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_signer() -> Pubkey {
        let mut key = [0u8; 32];
        key[0] = 42;
        key
    }

    fn test_program() -> Pubkey {
        let mut key = [0u8; 32];
        key[0] = 1;
        key
    }

    #[test]
    fn test_create_transaction() {
        let signer = test_signer();
        let program = test_program();

        let ix = Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta {
                    index: 0,
                    is_signer: true,
                    is_writable: true,
                },
                AccountMeta {
                    index: 1,
                    is_signer: false,
                    is_writable: true,
                },
            ],
            data: b"transfer 100".to_vec(),
        };

        let tx = Transaction::new(signer, vec![ix], [0u8; 32]);
        assert_eq!(tx.instructions.len(), 1);
        assert_eq!(tx.fee, 5000);
    }

    #[test]
    fn test_transaction_hash_deterministic() {
        let signer = test_signer();
        let program = test_program();

        let ix = Instruction {
            program_id: program,
            accounts: vec![],
            data: b"test".to_vec(),
        };

        let tx1 = Transaction::new(signer, vec![ix.clone()], [1u8; 32]);
        let tx2 = Transaction::new(signer, vec![ix], [1u8; 32]);

        assert_eq!(tx1.hash(), tx2.hash());
    }

    #[test]
    fn test_conflict_detection() {
        let signer = test_signer();
        let program = test_program();

        let ix1 = Instruction {
            program_id: program,
            accounts: vec![AccountMeta {
                index: 0,
                is_signer: true,
                is_writable: true,
            }],
            data: vec![],
        };

        let ix2 = Instruction {
            program_id: program,
            accounts: vec![AccountMeta {
                index: 0,
                is_signer: false,
                is_writable: false,
            }],
            data: vec![],
        };

        let tx1 = Transaction::new(signer, vec![ix1], [0u8; 32]);
        let tx2 = Transaction::new(signer, vec![ix2], [0u8; 32]);

        // tx1 writes account 0, tx2 reads account 0 — conflict!
        assert!(tx1.conflicts_with(&tx2));
    }

    #[test]
    fn test_no_conflict() {
        let signer = test_signer();
        let program = test_program();

        let ix1 = Instruction {
            program_id: program,
            accounts: vec![AccountMeta {
                index: 0,
                is_signer: true,
                is_writable: true,
            }],
            data: vec![],
        };

        let ix2 = Instruction {
            program_id: program,
            accounts: vec![AccountMeta {
                index: 1,
                is_signer: true,
                is_writable: true,
            }],
            data: vec![],
        };

        let tx1 = Transaction::new(signer, vec![ix1], [0u8; 32]);
        let tx2 = Transaction::new(signer, vec![ix2], [0u8; 32]);

        // Different accounts — no conflict
        assert!(!tx1.conflicts_with(&tx2));
    }

    #[test]
    fn test_validation() {
        let signer = test_signer();

        // Empty instructions
        let tx = Transaction::new(signer, vec![], [0u8; 32]);
        assert!(tx.validate().is_err());

        // Valid transaction
        let ix = Instruction {
            program_id: test_program(),
            accounts: vec![],
            data: vec![],
        };
        let tx = Transaction::new(signer, vec![ix], [0u8; 32]);
        assert!(tx.validate().is_ok());
    }
}
