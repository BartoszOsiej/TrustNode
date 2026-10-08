//! Transaction Executor — executes scheduled batches against the accounts DB
//!
//! The executor takes scheduled batches and runs them, either sequentially
//! within a batch or in parallel across batches.

use crate::scheduler::ExecutionBatch;
use crate::transaction::{Transaction, TransactionError, TransactionResult};
use dashmap::DashMap;
use parking_lot::RwLock;
use solana_accounts::store::AccountsDB;
use solana_program_executor::instruction::AccountMeta as ProgramAccountMeta;
use solana_program_executor::{Instruction as ProgramInstruction, InstructionProcessor};
use std::sync::Arc;

/// Execution context for a single transaction
#[derive(Debug)]
pub struct ExecutionContext {
    /// Compute units consumed so far
    pub compute_units: u64,
    /// Maximum compute units allowed
    pub max_compute_units: u64,
    /// Execution logs
    pub logs: Vec<String>,
}

impl ExecutionContext {
    pub fn new(max_compute: u64) -> Self {
        Self {
            compute_units: 0,
            max_compute_units: max_compute,
            logs: Vec::new(),
        }
    }

    /// Consume compute units
    pub fn consume_compute(&mut self, units: u64) -> Result<(), TransactionError> {
        self.compute_units += units;
        if self.compute_units > self.max_compute_units {
            Err(TransactionError::ComputeBudgetExceeded)
        } else {
            Ok(())
        }
    }

    /// Add a log entry
    pub fn log(&mut self, msg: String) {
        self.logs.push(msg);
    }
}

/// Transaction executor
pub struct Executor {
    /// Reference to the accounts database
    db: Arc<AccountsDB>,
    /// Native program dispatcher (system, token, deployed programs)
    processor: InstructionProcessor,
    /// Lock table for concurrent access
    /// Maps account index -> whether it's currently locked for writing
    #[allow(dead_code)]
    write_locks: DashMap<u8, ()>,
    /// Accounts locked for reading (count of concurrent readers)
    #[allow(dead_code)]
    read_locks: DashMap<u8, u64>,
    /// Total transactions executed
    total_executed: RwLock<u64>,
    /// Total compute units consumed
    total_compute: RwLock<u64>,
}

impl Executor {
    /// Create a new executor
    pub fn new(db: Arc<AccountsDB>) -> Self {
        Self {
            processor: InstructionProcessor::new(db.clone()),
            db,
            write_locks: DashMap::new(),
            read_locks: DashMap::new(),
            total_executed: RwLock::new(0),
            total_compute: RwLock::new(0),
        }
    }

    /// Execute a single transaction
    pub fn execute_transaction(&self, tx: &Transaction) -> TransactionResult {
        let mut ctx = ExecutionContext::new(tx.compute_budget);

        // Basic validation
        if let Err(e) = tx.validate() {
            return TransactionResult::failure(e, 0);
        }

        // Verify signature
        if !tx.verify_signature() {
            return TransactionResult::failure(
                TransactionError::InvalidSignature,
                ctx.compute_units,
            );
        }

        // Check fee payment
        if let Err(e) = self.execute_fee_payment(tx, &mut ctx) {
            return TransactionResult::failure(e, ctx.compute_units);
        }

        // Execute each instruction
        for ix in &tx.instructions {
            match self.execute_instruction(tx, ix, &mut ctx) {
                Ok(()) => {}
                Err(e) => {
                    ctx.log(format!("Instruction failed: {}", e));
                    return TransactionResult::failure(e, ctx.compute_units);
                }
            }
        }

        // Charge compute budget cost
        let _ = ctx.consume_compute(150); // Base cost per instruction

        *self.total_executed.write() += 1;
        *self.total_compute.write() += ctx.compute_units;

        TransactionResult::success(ctx.compute_units, tx.fee)
    }

    /// Charge the transaction fee — deducted from the signer's account
    /// before any instruction runs (the fee is paid even if an instruction
    /// later fails, like on Solana).
    fn execute_fee_payment(
        &self,
        tx: &Transaction,
        ctx: &mut ExecutionContext,
    ) -> Result<(), TransactionError> {
        ctx.consume_compute(150)?; // Fee processing cost

        let mut account =
            self.db
                .load(&tx.signer)
                .ok_or(TransactionError::InsufficientLamports {
                    needed: tx.fee,
                    available: 0,
                })?;
        if account.lamports < tx.fee {
            return Err(TransactionError::InsufficientLamports {
                needed: tx.fee,
                available: account.lamports,
            });
        }
        account.lamports -= tx.fee;
        self.db.store(tx.signer, &account);

        ctx.log(format!("Fee paid: {} lamports", tx.fee));
        Ok(())
    }

    /// Execute a single instruction through the native program dispatcher
    fn execute_instruction(
        &self,
        tx: &Transaction,
        ix: &crate::transaction::Instruction,
        ctx: &mut ExecutionContext,
    ) -> Result<(), TransactionError> {
        ctx.log(format!("Executing program: {:?}", ix.program_id));

        // Single-signer model: every meta flagged as a signer must resolve to
        // the transaction's Ed25519-verified signer key.
        for meta in &ix.accounts {
            if meta.is_signer {
                match tx.account_keys.get(meta.index as usize) {
                    Some(key) if *key == tx.signer => {}
                    _ => return Err(TransactionError::InvalidSignature),
                }
            }
        }

        let program_ix = ProgramInstruction {
            program_id: ix.program_id,
            account_metas: ix
                .accounts
                .iter()
                .map(|m| ProgramAccountMeta::new(m.index as usize, m.is_signer, m.is_writable))
                .collect(),
            data: ix.data.clone(),
        };

        let result = self
            .processor
            .process_instruction(&program_ix, &tx.signer, &tx.account_keys);

        for log in &result.logs {
            ctx.log(log.clone());
        }
        ctx.consume_compute(result.compute_units_consumed)?;

        if !result.success {
            let msg = result
                .error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "program failed".to_string());
            return Err(TransactionError::ProgramError(msg));
        }

        Ok(())
    }

    /// Execute a full batch (sequentially for now)
    pub fn execute_batch(&self, batch: &ExecutionBatch) -> Vec<TransactionResult> {
        batch
            .transactions
            .iter()
            .map(|tx| self.execute_transaction(tx))
            .collect()
    }

    /// Get total transactions executed
    pub fn total_executed(&self) -> u64 {
        *self.total_executed.read()
    }

    /// Get total compute units consumed
    pub fn total_compute(&self) -> u64 {
        *self.total_compute.read()
    }

    /// Get stats
    pub fn stats(&self) -> ExecutorStats {
        ExecutorStats {
            total_executed: self.total_executed(),
            total_compute: self.total_compute(),
        }
    }
}

/// Executor statistics
#[derive(Debug, Clone)]
pub struct ExecutorStats {
    pub total_executed: u64,
    pub total_compute: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{AccountMeta, Instruction};
    use ed25519_dalek::SigningKey;
    use solana_accounts::account::{Account, Pubkey};
    use solana_program_executor::instruction::SystemInstruction;

    fn test_db() -> Arc<AccountsDB> {
        Arc::new(AccountsDB::new())
    }

    fn test_key(seed_byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed_byte; 32])
    }

    fn transfer_tx(sk: &SigningKey, recipient: Pubkey, lamports: u64) -> Transaction {
        let signer = sk.verifying_key().to_bytes();
        let ix = Instruction {
            program_id: [1u8; 32], // native system program
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
            data: SystemInstruction::Transfer { lamports }.to_data(),
        };
        let mut tx = Transaction::new(signer, vec![ix], [0u8; 32]);
        tx.account_keys.push(recipient);
        tx.sign(sk);
        tx
    }

    #[test]
    fn test_execute_system_transfer() {
        let db = test_db();
        let executor = Executor::new(db.clone());

        let sk = test_key(42);
        let signer = sk.verifying_key().to_bytes();
        let recipient = [7u8; 32];

        db.store(signer, &Account::new_system_account(signer, 10_000_000));

        let tx = transfer_tx(&sk, recipient, 500);
        let result = executor.execute_transaction(&tx);

        assert!(result.success, "should succeed: {:?}", result.error);
        assert!(result.compute_units_consumed > 0);

        // Fee + transfer actually moved lamports (no simulated balances)
        assert_eq!(db.load(&signer).unwrap().lamports, 10_000_000 - 5_000 - 500);
        assert_eq!(db.load(&recipient).unwrap().lamports, 500);
    }

    #[test]
    fn test_unsigned_transaction_rejected() {
        let db = test_db();
        let executor = Executor::new(db.clone());

        let sk = test_key(42);
        let signer = sk.verifying_key().to_bytes();
        db.store(signer, &Account::new_system_account(signer, 10_000_000));

        let mut tx = transfer_tx(&sk, [7u8; 32], 500);
        tx.signature = vec![]; // strip the signature

        let result = executor.execute_transaction(&tx);
        assert!(!result.success);
        assert!(matches!(
            result.error,
            Some(TransactionError::InvalidSignature)
        ));
    }

    #[test]
    fn test_signer_flag_must_match_signed_key() {
        let db = test_db();
        let executor = Executor::new(db.clone());

        let sk = test_key(42);
        let signer = sk.verifying_key().to_bytes();
        db.store(signer, &Account::new_system_account(signer, 10_000_000));

        // Index 1 is flagged as a signer, but its key is not the tx signer
        let ix = Instruction {
            program_id: [1u8; 32],
            accounts: vec![
                AccountMeta {
                    index: 0,
                    is_signer: true,
                    is_writable: true,
                },
                AccountMeta {
                    index: 1,
                    is_signer: true,
                    is_writable: true,
                },
            ],
            data: SystemInstruction::Transfer { lamports: 500 }.to_data(),
        };
        let mut tx = Transaction::new(signer, vec![ix], [0u8; 32]);
        tx.account_keys.push([7u8; 32]);
        tx.sign(&sk);

        let result = executor.execute_transaction(&tx);
        assert!(!result.success);
        assert!(matches!(
            result.error,
            Some(TransactionError::InvalidSignature)
        ));
    }

    #[test]
    fn test_unknown_program_is_rejected() {
        let db = test_db();
        let executor = Executor::new(db.clone());

        let sk = test_key(42);
        let signer = sk.verifying_key().to_bytes();
        db.store(signer, &Account::new_system_account(signer, 10_000_000));

        let ix = Instruction {
            program_id: [99u8; 32], // never deployed
            accounts: vec![AccountMeta {
                index: 0,
                is_signer: true,
                is_writable: false,
            }],
            data: vec![],
        };
        let mut tx = Transaction::new(signer, vec![ix], [0u8; 32]);
        tx.sign(&sk);

        let result = executor.execute_transaction(&tx);
        assert!(!result.success);
        let msg = result.error.map(|e| e.to_string()).unwrap_or_default();
        assert!(msg.contains("not found"), "unexpected error: {msg}");
    }

    #[test]
    fn test_execute_invalid_transaction() {
        let db = test_db();
        let executor = Executor::new(db);

        let sk = test_key(42);
        let signer = sk.verifying_key().to_bytes();
        let tx = Transaction::new(signer, vec![], [0u8; 32]); // Empty instructions

        let result = executor.execute_transaction(&tx);
        assert!(!result.success);
    }

    #[test]
    fn test_execute_batch() {
        let db = test_db();
        let executor = Executor::new(db.clone());

        let sk = test_key(43);
        let signer = sk.verifying_key().to_bytes();
        let recipient = [8u8; 32];
        db.store(signer, &Account::new_system_account(signer, 10_000_000));

        let txs: Vec<Transaction> = (0..3).map(|_| transfer_tx(&sk, recipient, 100)).collect();

        let batch = ExecutionBatch {
            index: 0,
            transactions: txs,
            total_compute: 600_000,
        };

        let results = executor.execute_batch(&batch);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.success));

        // 3 fees + 3 transfers deducted for real
        assert_eq!(
            db.load(&signer).unwrap().lamports,
            10_000_000 - 3 * 5_000 - 3 * 100
        );
        assert_eq!(db.load(&recipient).unwrap().lamports, 300);
    }
}
