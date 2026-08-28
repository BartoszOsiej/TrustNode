//! Instruction processor — dispatches and executes instructions

use crate::instruction::{Instruction, InstructionResult};
use crate::programs::system_program;
use crate::programs::token_program;
use crate::{InstructionError, DEFAULT_COMPUTE_UNITS};
use solana_accounts::account::{Account, Pubkey};
use solana_accounts::store::AccountsDB;
use std::sync::Arc;

/// Compute budget tracker
#[derive(Debug)]
pub struct ComputeBudget {
    /// Maximum compute units allowed
    pub max: u64,
    /// Compute units consumed so far
    pub consumed: u64,
}

impl ComputeBudget {
    pub fn new(max: u64) -> Self {
        Self { max, consumed: 0 }
    }

    pub fn consume(&mut self, units: u64) -> Result<(), InstructionError> {
        self.consumed += units;
        if self.consumed > self.max {
            return Err(InstructionError::InsufficientComputeUnits);
        }
        Ok(())
    }

    pub fn remaining(&self) -> u64 {
        self.max.saturating_sub(self.consumed)
    }
}

/// Instruction processor — the heart of the VM
pub struct InstructionProcessor {
    /// Reference to accounts DB
    accounts: Arc<AccountsDB>,
}

impl InstructionProcessor {
    pub fn new(accounts: Arc<AccountsDB>) -> Self {
        Self { accounts }
    }

    /// Process a single instruction
    pub fn process_instruction(
        &self,
        instruction: &Instruction,
        signer: &Pubkey,
    ) -> InstructionResult {
        let mut budget = ComputeBudget::new(DEFAULT_COMPUTE_UNITS);
        let mut logs = Vec::new();

        logs.push(format!(
            "Program {}",
            hex::encode(&instruction.program_id[..4])
        ));

        // Dispatch based on program ID
        let result = match instruction.program_id {
            // System program — [1u8; 32]
            [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1] =>
            {
                budget.consume(150).unwrap_or(());
                system_program::process_instruction(
                    &instruction.data,
                    &instruction.account_metas,
                    signer,
                    &self.accounts,
                    &mut budget,
                    &mut logs,
                )
            }
            // Token program — [2u8; 32]
            [2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2] =>
            {
                budget.consume(150).unwrap_or(());
                token_program::process_instruction(
                    &instruction.data,
                    &instruction.account_metas,
                    signer,
                    &self.accounts,
                    &mut budget,
                    &mut logs,
                )
            }
            // Unknown program — check if it's a loaded program
            _ => {
                match self.accounts.load(&instruction.program_id) {
                    Some(account) if account.executable => {
                        budget.consume(500).unwrap_or(());
                        logs.push(format!(
                            "Invoking custom program {}",
                            hex::encode(&instruction.program_id[..8])
                        ));
                        // Custom program execution (stub)
                        Ok(())
                    }
                    Some(_) => Err(InstructionError::InvalidAccountOwner),
                    None => Err(InstructionError::AccountNotFound),
                }
            }
        };

        match result {
            Ok(()) => {
                logs.push(format!(
                    "Program success ({} compute units)",
                    budget.consumed
                ));
                InstructionResult::success(budget.consumed)
                    .with_log("Program completed successfully".to_string())
            }
            Err(e) => {
                logs.push(format!("Program failed: {}", e));
                InstructionResult::failure(e, budget.consumed)
            }
        }
    }

    /// Get accounts DB reference
    pub fn accounts(&self) -> &Arc<AccountsDB> {
        &self.accounts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::{AccountMeta, Instruction};

    fn test_processor() -> InstructionProcessor {
        let accounts = Arc::new(AccountsDB::new());
        // Create system program
        let system_acc = Account::new_system_account([1u8; 32], 0);
        accounts.store([1u8; 32], &system_acc);
        InstructionProcessor::new(accounts)
    }

    #[test]
    fn test_process_instruction_dispatches_system_program() {
        let processor = test_processor();
        let signer = [42u8; 32];
        let signer_acc = Account::new_system_account(signer, 10_000_000);
        processor.accounts().store(signer, &signer_acc);

        // Use system program ID that matches the dispatch arm
        let system_prog = [1u8; 32];
        let ix = Instruction {
            program_id: system_prog,
            account_metas: vec![],
            data: SystemInstruction::Transfer { lamports: 100 }.to_data(),
        };

        let result = processor.process_instruction(&ix, &signer);
        // Transfer fails because only 0 account_metas, but the dispatch succeeded
        // (we didn't hit InvalidAccountOwner or AccountNotFound)
        assert!(!result.success); // expected: InvalidInstructionData for missing accounts
        assert!(result.compute_units_consumed > 0);
    }

    #[test]
    fn test_process_unknown_program() {
        let processor = test_processor();
        let signer = [42u8; 32];

        let ix = Instruction {
            program_id: [99u8; 32], // doesn't exist
            account_metas: vec![],
            data: vec![],
        };

        let result = processor.process_instruction(&ix, &signer);
        assert!(!result.success);
    }

    #[test]
    fn test_compute_budget() {
        let mut budget = ComputeBudget::new(1000);
        assert_eq!(budget.remaining(), 1000);

        budget.consume(300).unwrap();
        assert_eq!(budget.remaining(), 700);

        assert!(budget.consume(800).is_err()); // would exceed
    }
}
