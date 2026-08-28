//! System Program — the most fundamental Solana program
//!
//! Handles:
//! - SOL transfers between accounts
//! - Account creation with rent
//! - Account assignment to programs
//! - Nonce operations

use crate::instruction::{AccountMeta, SystemInstruction};
use crate::processor::ComputeBudget;
use crate::InstructionError;
use solana_accounts::account::{Account, AccountData, Pubkey};
use solana_accounts::store::AccountsDB;

/// Minimum rent-exempt balance for an account
pub const MINIMUM_RENT_EXEMPT_BALANCE: u64 = 890_880; // ~0.00089 SOL

/// Process a system program instruction
pub fn process_instruction(
    data: &[u8],
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    let instruction = SystemInstruction::from_data(data)?;

    match instruction {
        SystemInstruction::Transfer { lamports } => {
            process_transfer(lamports, account_metas, signer, accounts, budget, logs)
        }
        SystemInstruction::CreateAccount {
            lamports,
            space,
            owner,
        } => process_create_account(
            lamports,
            space,
            owner,
            account_metas,
            signer,
            accounts,
            budget,
            logs,
        ),
        SystemInstruction::Assign { owner } => {
            process_assign(owner, account_metas, signer, accounts, budget, logs)
        }
        SystemInstruction::NonceInitialize => {
            logs.push("NonceInitialize (stub)".to_string());
            budget.consume(100)?;
            Ok(())
        }
    }
}

/// Process a SOL transfer
fn process_transfer(
    lamports: u64,
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(150)?;

    if account_metas.len() < 2 {
        return Err(InstructionError::InvalidInstructionData(
            "Transfer requires 2 accounts".to_string(),
        ));
    }

    let from_meta = &account_metas[0];
    let to_meta = &account_metas[1];

    // Load source account
    let from_pubkey = get_account_key(from_meta.index)?;
    if &from_pubkey != signer && !from_meta.is_signer {
        return Err(InstructionError::ProgramError(
            "Source account not signed".to_string(),
        ));
    }

    let mut from_account = accounts
        .load(&from_pubkey)
        .ok_or(InstructionError::AccountNotFound)?;

    if from_account.lamports < lamports {
        return Err(InstructionError::InsufficientFunds {
            needed: lamports,
            available: from_account.lamports,
        });
    }

    let to_pubkey = get_account_key(to_meta.index)?;

    // Deduct from source
    from_account.lamports -= lamports;
    accounts.store(from_pubkey, &from_account);

    // Credit destination
    let mut to_account = accounts
        .load(&to_pubkey)
        .unwrap_or_else(|| Account::new_system_account(to_pubkey, 0));
    to_account.lamports += lamports;
    accounts.store(to_pubkey, &to_account);

    logs.push(format!(
        "Transfer: {} lamports from {} to {}",
        lamports,
        hex::encode(&from_pubkey[..8]),
        hex::encode(&to_pubkey[..8]),
    ));

    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// Process account creation
fn process_create_account(
    lamports: u64,
    space: u64,
    owner: Pubkey,
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(200)?;

    if account_metas.len() < 2 {
        return Err(InstructionError::InvalidInstructionData(
            "CreateAccount requires 2 accounts".to_string(),
        ));
    }

    let payer_meta = &account_metas[0];
    let new_account_meta = &account_metas[1];

    let payer_pubkey = get_account_key(payer_meta.index)?;
    let new_account_pubkey = get_account_key(new_account_meta.index)?;

    // Payer must be signer
    if &payer_pubkey != signer && !payer_meta.is_signer {
        return Err(InstructionError::ProgramError(
            "Payer not signed".to_string(),
        ));
    }

    // New account must not exist
    if accounts.exists(&new_account_pubkey) {
        return Err(InstructionError::ProgramError(
            "Account already exists".to_string(),
        ));
    }

    // Check payer has enough
    let mut payer = accounts
        .load(&payer_pubkey)
        .ok_or(InstructionError::AccountNotFound)?;

    let total_cost = lamports + MINIMUM_RENT_EXEMPT_BALANCE;
    if payer.lamports < total_cost {
        return Err(InstructionError::InsufficientFunds {
            needed: total_cost,
            available: payer.lamports,
        });
    }

    // Deduct from payer
    payer.lamports -= total_cost;
    accounts.store(payer_pubkey, &payer);

    // Create new account
    let new_account = Account {
        lamports,
        owner,
        executable: false,
        rent_epoch: 0,
        data: AccountData::Data(vec![0u8; space as usize]),
    };
    accounts.store(new_account_pubkey, &new_account);

    logs.push(format!(
        "CreateAccount: {} ({} lamports, {} bytes) owned by {}",
        hex::encode(&new_account_pubkey[..8]),
        lamports,
        space,
        hex::encode(&owner[..8]),
    ));

    Ok(())
}

/// Process account assignment
fn process_assign(
    owner: Pubkey,
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(100)?;

    if account_metas.is_empty() {
        return Err(InstructionError::InvalidInstructionData(
            "Assign requires 1 account".to_string(),
        ));
    }

    let account_pubkey = get_account_key(account_metas[0].index)?;

    // Must be signer
    if &account_pubkey != signer && !account_metas[0].is_signer {
        return Err(InstructionError::ProgramError(
            "Account not signed".to_string(),
        ));
    }

    let mut account = accounts
        .load(&account_pubkey)
        .ok_or(InstructionError::AccountNotFound)?;

    // Only system program can assign
    if account.owner != [1u8; 32] {
        return Err(InstructionError::InvalidAccountOwner);
    }

    account.owner = owner;
    accounts.store(account_pubkey, &account);

    logs.push(format!(
        "Assign: {} to {}",
        hex::encode(&account_pubkey[..8]),
        hex::encode(&owner[..8]),
    ));

    Ok(())
}

/// Helper: get a pubkey from an account index (simplified — uses index as seed)
pub fn get_account_key(index: usize) -> Result<Pubkey, InstructionError> {
    // In a real implementation, this would look up the actual account key
    // from the transaction's account list. For now, derive a deterministic key.
    let mut key = [0u8; 32];
    key[0] = 0xFF; // avoid collision with system program [1u8; 32]
    key[1] = (index + 1) as u8;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_accounts() -> AccountsDB {
        let db = AccountsDB::new();
        // System program
        db.store([1u8; 32], &Account::new_system_account([1u8; 32], 0));
        db
    }

    #[test]
    fn test_transfer() {
        let accounts = test_accounts();
        let mut budget = ComputeBudget::new(10_000);
        let mut logs = Vec::new();

        // Create source account
        let from_key = get_account_key(0).unwrap();
        let from_acc = Account::new_system_account(from_key, 1_000_000);
        accounts.store(from_key, &from_acc);

        // Create dest account
        let to_key = get_account_key(1).unwrap();
        let to_acc = Account::new_system_account(to_key, 0);
        accounts.store(to_key, &to_acc);

        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        let result = process_instruction(
            &SystemInstruction::Transfer { lamports: 500 }.to_data(),
            &metas,
            &from_key,
            &accounts,
            &mut budget,
            &mut logs,
        );

        assert!(result.is_ok());
        assert_eq!(accounts.load(&from_key).unwrap().lamports, 999_500);
        assert_eq!(accounts.load(&to_key).unwrap().lamports, 500);
    }

    #[test]
    fn test_insufficient_funds() {
        let accounts = test_accounts();
        let mut budget = ComputeBudget::new(10_000);
        let mut logs = Vec::new();

        let from_key = get_account_key(0).unwrap();
        let from_acc = Account::new_system_account(from_key, 100);
        accounts.store(from_key, &from_acc);

        let to_key = get_account_key(1).unwrap();
        let to_acc = Account::new_system_account(to_key, 0);
        accounts.store(to_key, &to_acc);

        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        let result = process_instruction(
            &SystemInstruction::Transfer { lamports: 200 }.to_data(),
            &metas,
            &from_key,
            &accounts,
            &mut budget,
            &mut logs,
        );

        assert!(result.is_err());
        match result {
            Err(InstructionError::InsufficientFunds { needed, available }) => {
                assert_eq!(needed, 200);
                assert_eq!(available, 100);
            }
            _ => panic!("Expected InsufficientFunds"),
        }
    }

    #[test]
    fn test_create_account() {
        let accounts = test_accounts();
        let mut budget = ComputeBudget::new(10_000);
        let mut logs = Vec::new();

        let payer_key = get_account_key(0).unwrap();
        let payer_acc = Account::new_system_account(payer_key, 10_000_000);
        accounts.store(payer_key, &payer_acc);

        let new_key = get_account_key(1).unwrap();

        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        let result = process_instruction(
            &SystemInstruction::CreateAccount {
                lamports: 1_000_000,
                space: 1024,
                owner: [42u8; 32],
            }
            .to_data(),
            &metas,
            &payer_key,
            &accounts,
            &mut budget,
            &mut logs,
        );

        assert!(result.is_ok());

        let new_account = accounts.load(&new_key).unwrap();
        assert_eq!(new_account.lamports, 1_000_000);
        assert_eq!(new_account.owner, [42u8; 32]);
    }
}
