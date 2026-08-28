//! Token Program — SPL Token-like operations
//!
//! Handles:
//! - Token account initialization
//! - Token transfers
//! - Minting tokens
//! - Burning tokens

use crate::instruction::{AccountMeta, TokenInstruction};
use crate::processor::ComputeBudget;
use crate::InstructionError;
use solana_accounts::account::{Account, AccountData, Pubkey};
use solana_accounts::store::AccountsDB;

/// Process a token program instruction
pub fn process_instruction(
    data: &[u8],
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    let instruction = TokenInstruction::from_data(data)?;

    match instruction {
        TokenInstruction::InitializeAccount => {
            process_initialize_account(account_metas, accounts, budget, logs)
        }
        TokenInstruction::Transfer { amount } => {
            process_token_transfer(amount, account_metas, signer, accounts, budget, logs)
        }
        TokenInstruction::MintTo { amount } => {
            process_mint_to(amount, account_metas, signer, accounts, budget, logs)
        }
        TokenInstruction::Burn { amount } => {
            process_burn(amount, account_metas, signer, accounts, budget, logs)
        }
    }
}

/// Process token account initialization
fn process_initialize_account(
    _account_metas: &[AccountMeta],
    _accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(200)?;
    logs.push("Token: InitializeAccount".to_string());
    Ok(())
}

/// Process token transfer
fn process_token_transfer(
    amount: u64,
    account_metas: &[AccountMeta],
    _signer: &Pubkey,
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(200)?;

    if account_metas.len() < 2 {
        return Err(InstructionError::InvalidInstructionData(
            "Transfer requires 2 accounts".to_string(),
        ));
    }

    let from_key = get_token_account_key(account_metas[0].index)?;
    let to_key = get_token_account_key(account_metas[1].index)?;

    // Load source token account
    let mut from_account = accounts
        .load(&from_key)
        .ok_or(InstructionError::AccountNotFound)?;

    // Check it's a token account
    match &from_account.data {
        AccountData::Token {
            amount: balance, ..
        } => {
            if *balance < amount {
                return Err(InstructionError::InsufficientFunds {
                    needed: amount,
                    available: *balance,
                });
            }
        }
        _ => {
            return Err(InstructionError::ProgramError(
                "Source is not a token account".to_string(),
            ))
        }
    }

    // Deduct from source
    if let AccountData::Token {
        amount: ref mut balance,
        ..
    } = from_account.data
    {
        *balance -= amount;
    }
    accounts.store(from_key, &from_account);

    // Credit destination
    let mut to_account = accounts
        .load(&to_key)
        .unwrap_or_else(|| Account::new_system_account(to_key, 0));

    if let AccountData::Token {
        amount: ref mut balance,
        ..
    } = to_account.data
    {
        *balance += amount;
    } else {
        to_account.data = AccountData::Token {
            mint: [0u8; 32],
            authority: to_key,
            amount,
        };
    }
    accounts.store(to_key, &to_account);

    logs.push(format!(
        "Token: Transfer {} tokens from {} to {}",
        amount,
        hex::encode(&from_key[..8]),
        hex::encode(&to_key[..8]),
    ));

    Ok(())
}

/// Process minting tokens
fn process_mint_to(
    amount: u64,
    account_metas: &[AccountMeta],
    _signer: &Pubkey,
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(200)?;

    if account_metas.is_empty() {
        return Err(InstructionError::InvalidInstructionData(
            "MintTo requires 1 account".to_string(),
        ));
    }

    let mint_key = get_token_account_key(account_metas[0].index)?;
    let mut mint_account = accounts
        .load(&mint_key)
        .ok_or(InstructionError::AccountNotFound)?;

    // Credit tokens
    if let AccountData::Token {
        amount: ref mut balance,
        ..
    } = mint_account.data
    {
        *balance = balance
            .checked_add(amount)
            .ok_or(InstructionError::ArithmeticOverflow)?;
    } else {
        mint_account.data = AccountData::Token {
            mint: mint_key,
            authority: mint_key,
            amount,
        };
    }
    accounts.store(mint_key, &mint_account);

    logs.push(format!(
        "Token: Mint {} tokens to {}",
        amount,
        hex::encode(&mint_key[..8]),
    ));

    Ok(())
}

/// Process burning tokens
fn process_burn(
    amount: u64,
    account_metas: &[AccountMeta],
    _signer: &Pubkey,
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(200)?;

    if account_metas.is_empty() {
        return Err(InstructionError::InvalidInstructionData(
            "Burn requires 1 account".to_string(),
        ));
    }

    let token_key = get_token_account_key(account_metas[0].index)?;
    let mut token_account = accounts
        .load(&token_key)
        .ok_or(InstructionError::AccountNotFound)?;

    match &token_account.data {
        AccountData::Token {
            amount: balance, ..
        } => {
            if *balance < amount {
                return Err(InstructionError::InsufficientFunds {
                    needed: amount,
                    available: *balance,
                });
            }
        }
        _ => {
            return Err(InstructionError::ProgramError(
                "Account is not a token account".to_string(),
            ))
        }
    }

    if let AccountData::Token {
        amount: ref mut balance,
        ..
    } = token_account.data
    {
        *balance -= amount;
    }
    accounts.store(token_key, &token_account);

    logs.push(format!(
        "Token: Burn {} tokens from {}",
        amount,
        hex::encode(&token_key[..8]),
    ));

    Ok(())
}

/// Helper: get token account key from index
fn get_token_account_key(index: usize) -> Result<Pubkey, InstructionError> {
    let mut key = [0u8; 32];
    key[0] = 0xFE; // avoid collision with programs
    key[1] = (index + 1) as u8;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_accounts::account::AccountData;

    fn setup_token_account(accounts: &AccountsDB, index: usize, amount: u64) -> Pubkey {
        let key = get_token_account_key(index).unwrap();
        let acc = Account {
            lamports: 1_000_000,
            owner: [2u8; 32], // Token program
            executable: false,
            rent_epoch: 0,
            data: AccountData::Token {
                mint: [10u8; 32],
                authority: key,
                amount,
            },
        };
        accounts.store(key, &acc);
        key
    }

    #[test]
    fn test_token_transfer() {
        let accounts = AccountsDB::new();
        let mut budget = ComputeBudget::new(10_000);
        let mut logs = Vec::new();

        let from_key = setup_token_account(&accounts, 0, 1000);
        let to_key = setup_token_account(&accounts, 1, 0);

        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        let result =
            process_token_transfer(500, &metas, &from_key, &accounts, &mut budget, &mut logs);

        assert!(result.is_ok());

        let from = accounts.load(&from_key).unwrap();
        let to = accounts.load(&to_key).unwrap();

        if let AccountData::Token { amount, .. } = from.data {
            assert_eq!(amount, 500);
        }
        if let AccountData::Token { amount, .. } = to.data {
            assert_eq!(amount, 500);
        }
    }

    #[test]
    fn test_token_insufficient_balance() {
        let accounts = AccountsDB::new();
        let mut budget = ComputeBudget::new(10_000);
        let mut logs = Vec::new();

        let from_key = setup_token_account(&accounts, 0, 100);
        let _to_key = setup_token_account(&accounts, 1, 0);

        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        let result =
            process_token_transfer(200, &metas, &from_key, &accounts, &mut budget, &mut logs);

        assert!(result.is_err());
    }

    #[test]
    fn test_mint_to() {
        let accounts = AccountsDB::new();
        let mut budget = ComputeBudget::new(10_000);
        let mut logs = Vec::new();

        let mint_key = setup_token_account(&accounts, 0, 0);

        let metas = vec![AccountMeta::new(0, true, true)];

        let result = process_mint_to(1000, &metas, &mint_key, &accounts, &mut budget, &mut logs);

        assert!(result.is_ok());

        let mint = accounts.load(&mint_key).unwrap();
        if let AccountData::Token { amount, .. } = mint.data {
            assert_eq!(amount, 1000);
        }
    }

    #[test]
    fn test_burn() {
        let accounts = AccountsDB::new();
        let mut budget = ComputeBudget::new(10_000);
        let mut logs = Vec::new();

        let token_key = setup_token_account(&accounts, 0, 500);

        let metas = vec![AccountMeta::new(0, true, true)];

        let result = process_burn(200, &metas, &token_key, &accounts, &mut budget, &mut logs);

        assert!(result.is_ok());

        let token = accounts.load(&token_key).unwrap();
        if let AccountData::Token { amount, .. } = token.data {
            assert_eq!(amount, 300);
        }
    }
}
