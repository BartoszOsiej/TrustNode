//! Transaction Executor — executes scheduled batches against the accounts DB
//!
//! The executor takes scheduled batches and runs them, either sequentially
//! within a batch or in parallel across batches.

use crate::scheduler::ExecutionBatch;
use crate::transaction::{Transaction, TransactionError, TransactionResult};
use dashmap::DashMap;
use parking_lot::RwLock;
use solana_accounts::account::{Account, Pubkey};
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

/// A held account lock. Write locks are exclusive, read locks shared — the
/// same discipline Sealevel uses to run a batch's transactions concurrently.
/// The guards are only ever held (dropped to release), never inspected.
#[allow(dead_code)]
enum LockGuard<'a> {
    Read(parking_lot::RwLockReadGuard<'a, ()>),
    Write(parking_lot::RwLockWriteGuard<'a, ()>),
}

/// Transaction executor
pub struct Executor {
    /// Reference to the accounts database
    db: Arc<AccountsDB>,
    /// Native program dispatcher (system, token, deployed programs)
    processor: InstructionProcessor,
    /// Per-account read/write locks, keyed by resolved pubkey. A transaction
    /// holds a write lock on every account it mutates and a read lock on every
    /// account it only reads; a non-conflicting batch can therefore execute in
    /// parallel without two transactions ever racing on the same account.
    account_locks: DashMap<Pubkey, Arc<RwLock<()>>>,
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
            account_locks: DashMap::new(),
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

        // Acquire read/write locks for every account this transaction touches.
        // The guards are held for the rest of the call, so two transactions in
        // the same batch serialize on their shared accounts even when the
        // executor runs that batch in parallel.
        let lock_arcs = self.lock_accounts(tx);
        let mut _guards: Vec<LockGuard> = Vec::with_capacity(lock_arcs.len());
        for (is_write, arc) in &lock_arcs {
            if *is_write {
                _guards.push(LockGuard::Write(arc.write()));
            } else {
                _guards.push(LockGuard::Read(arc.read()));
            }
        }

        // Check fee payment
        if let Err(e) = self.execute_fee_payment(tx, &mut ctx) {
            return TransactionResult::failure(e, ctx.compute_units);
        }

        // Snapshot the mutable accounts *after* the fee has been charged, so a
        // later failure rolls back instruction effects but still keeps the fee.
        let snapshot = self.snapshot_transaction(tx);

        // Execute each instruction
        for ix in &tx.instructions {
            match self.execute_instruction(tx, ix, &mut ctx) {
                Ok(()) => {}
                Err(e) => {
                    self.rollback_transaction(&snapshot);
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

    /// Resolve the accounts a transaction locks: every writable account plus
    /// the fee payer (always debited) as exclusive, every read-only account as
    /// shared. The list is sorted and deduplicated so concurrent transactions
    /// always acquire locks in the same global order and cannot deadlock.
    fn lock_accounts(&self, tx: &Transaction) -> Vec<(bool, Arc<RwLock<()>>)> {
        let mut combined: Vec<(Pubkey, bool)> = Vec::new();
        for key in tx.write_accounts() {
            combined.push((key, true));
        }
        combined.push((tx.signer, true));
        for key in tx.read_accounts() {
            combined.push((key, false));
        }
        // Stable sort keeps the write entry first when a key is both read and
        // written, so the dedup below never downgrades a write lock to a read.
        combined.sort_by_key(|a| a.0);
        combined.dedup_by(|a, b| a.0 == b.0);

        combined
            .into_iter()
            .map(|(key, is_write)| {
                let lock = self
                    .account_locks
                    .entry(key)
                    .or_insert_with(|| Arc::new(RwLock::new(())))
                    .clone();
                (is_write, lock)
            })
            .collect()
    }

    /// Accounts a transaction may mutate: the fee payer and every writable
    /// account referenced by an instruction.
    fn mutable_accounts(tx: &Transaction) -> Vec<Pubkey> {
        let mut keys = vec![tx.signer];
        keys.extend(tx.write_accounts());
        keys.sort();
        keys.dedup();
        keys
    }

    /// Record the pre-execution state of every account the transaction can
    /// mutate, so a failed instruction can be rolled back.
    fn snapshot_transaction(&self, tx: &Transaction) -> Vec<(Pubkey, Option<Account>)> {
        Self::mutable_accounts(tx)
            .into_iter()
            .map(|key| {
                let prev = self.db.load(&key);
                (key, prev)
            })
            .collect()
    }

    /// Restore the state captured by [`Self::snapshot_transaction`]. Accounts
    /// that did not exist before the transaction are removed again.
    fn rollback_transaction(&self, snapshot: &[(Pubkey, Option<Account>)]) {
        for (key, prev) in snapshot {
            match prev {
                Some(account) => self.db.store(*key, account),
                None => {
                    self.db.remove(key);
                }
            }
        }
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

    /// Execute a full batch. Transactions in a batch are non-conflicting by
    /// construction, so they run concurrently; the per-account locks keep that
    /// safe even when the batch shares a fee payer.
    pub fn execute_batch(&self, batch: &ExecutionBatch) -> Vec<TransactionResult> {
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .transactions
                .iter()
                .map(|tx| scope.spawn(|| self.execute_transaction(tx)))
                .collect();

            handles
                .into_iter()
                .map(|handle| handle.join().expect("executor worker panicked"))
                .collect()
        })
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

    #[test]
    fn test_failed_instruction_rolls_back_state() {
        let db = test_db();
        let executor = Executor::new(db.clone());

        let sk = test_key(42);
        let signer = sk.verifying_key().to_bytes();
        let recipient = [7u8; 32];
        db.store(signer, &Account::new_system_account(signer, 10_000_000));

        // First instruction succeeds (a transfer), the second targets an
        // unknown program and fails. The whole instruction set must roll back,
        // leaving only the fee charged.
        let transfer = Instruction {
            program_id: [1u8; 32],
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
            data: SystemInstruction::Transfer { lamports: 500 }.to_data(),
        };
        let bad = Instruction {
            program_id: [99u8; 32],
            accounts: vec![AccountMeta {
                index: 0,
                is_signer: true,
                is_writable: false,
            }],
            data: vec![],
        };

        let mut tx = Transaction::new(signer, vec![transfer, bad], [0u8; 32]);
        tx.account_keys.push(recipient);
        tx.sign(&sk);

        let result = executor.execute_transaction(&tx);
        assert!(!result.success);

        // Fee kept, transfer rolled back.
        assert_eq!(db.load(&signer).unwrap().lamports, 10_000_000 - 5_000);
        assert!(!db.exists(&recipient));
    }
}
