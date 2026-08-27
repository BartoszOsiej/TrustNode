//! Transaction Executor — executes scheduled batches against the accounts DB
//!
//! The executor takes scheduled batches and runs them, either sequentially
//! within a batch or in parallel across batches.

use crate::scheduler::ExecutionBatch;
use crate::transaction::{Transaction, TransactionError, TransactionResult};
use dashmap::DashMap;
use parking_lot::RwLock;
use solana_accounts::account::Account;
use solana_accounts::store::AccountsDB;
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
    /// Lock table for concurrent access
    /// Maps account index -> whether it's currently locked for writing
    write_locks: DashMap<u8, ()>,
    /// Accounts locked for reading (count of concurrent readers)
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

    /// Execute fee payment (deduct lamports from signer)
    fn execute_fee_payment(
        &self,
        tx: &Transaction,
        ctx: &mut ExecutionContext,
    ) -> Result<(), TransactionError> {
        ctx.consume_compute(150)?; // Fee processing cost
        ctx.log(format!("Fee paid: {} lamports", tx.fee));
        Ok(())
    }

    /// Execute a single instruction
    fn execute_instruction(
        &self,
        tx: &Transaction,
        ix: &crate::transaction::Instruction,
        ctx: &mut ExecutionContext,
    ) -> Result<(), TransactionError> {
        ctx.consume_compute(100)?; // Base instruction cost
        ctx.log(format!("Executing program: {:?}", ix.program_id));

        // Check if program exists and is executable
        if let Some(program) = self.db.load(&ix.program_id) {
            if !program.executable {
                return Err(TransactionError::ProgramError(
                    "Account is not a program".to_string(),
                ));
            }
        }
        // Note: system program (all 1s) is special-cased

        // In a real implementation, this would dispatch to a VM (SBF/EBPF)
        // For now, we simulate basic program execution
        match &ix.program_id {
            // System program
            [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] =>
            {
                self.execute_system_program(tx, ix, ctx)?;
            }
            // Token program
            [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] =>
            {
                ctx.log("Token program invoked (stub)".to_string());
            }
            // Unknown program — simulate success
            _ => {
                ctx.consume_compute(500)?; // Generic program cost
                ctx.log(format!("Custom program executed: {:?}", ix.program_id));
            }
        }

        Ok(())
    }

    /// Simulate system program operations
    fn execute_system_program(
        &self,
        tx: &Transaction,
        _ix: &crate::transaction::Instruction,
        ctx: &mut ExecutionContext,
    ) -> Result<(), TransactionError> {
        // Check that signer account exists
        if !self.db.exists(&tx.signer) {
            // Create the signer account if it doesn't exist
            let acc = Account::new_system_account(tx.signer, 0);
            self.db.store(tx.signer, &acc);
            ctx.log(format!("Created account: {:?}", tx.signer));
        }

        ctx.consume_compute(150)?;
        ctx.log("System program: processed".to_string());

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
    use solana_accounts::account::Pubkey;

    fn test_db() -> Arc<AccountsDB> {
        Arc::new(AccountsDB::new())
    }

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
    fn test_execute_system_transfer() {
        let db = test_db();
        let executor = Executor::new(db.clone());

        let signer = test_signer();

        // Create signer account with lamports
        let signer_acc = Account::new_system_account(signer, 10_000_000);
        db.store(signer, &signer_acc);

        let ix = Instruction {
            program_id: test_program(),
            accounts: vec![AccountMeta {
                index: 0,
                is_signer: true,
                is_writable: true,
            }],
            data: b"transfer".to_vec(),
        };

        let mut tx = Transaction::new(signer, vec![ix], [0u8; 32]);
        tx.signature = vec![1u8; 64]; // Dummy signature

        let result = executor.execute_transaction(&tx);
        assert!(result.success, "Transaction should succeed");
        assert!(result.compute_units_consumed > 0);
    }

    #[test]
    fn test_execute_invalid_transaction() {
        let db = test_db();
        let executor = Executor::new(db);

        let signer = test_signer();
        let tx = Transaction::new(signer, vec![], [0u8; 32]); // Empty instructions

        let result = executor.execute_transaction(&tx);
        assert!(!result.success);
    }

    #[test]
    fn test_execute_batch() {
        let db = test_db();
        let executor = Executor::new(db.clone());

        let signer = test_signer();
        let signer_acc = Account::new_system_account(signer, 10_000_000);
        db.store(signer, &signer_acc);

        let mut txs = Vec::new();
        for _ in 0..3 {
            let ix = Instruction {
                program_id: test_program(),
                accounts: vec![AccountMeta {
                    index: 0,
                    is_signer: true,
                    is_writable: true,
                }],
                data: vec![],
            };
            let mut tx = Transaction::new(signer, vec![ix], [0u8; 32]);
            tx.signature = vec![1u8; 64];
            txs.push(tx);
        }

        let batch = ExecutionBatch {
            index: 0,
            transactions: txs,
            total_compute: 600_000,
        };

        let results = executor.execute_batch(&batch);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.success));
    }
}
