//! Token Program — SPL Token-like operations
//!
//! Handles:
//! - Token account initialization
//! - Token transfers
//! - Minting tokens
//! - Burning tokens

use crate::instruction::{resolve_account_key, AccountMeta, TokenInstruction};
use crate::processor::ComputeBudget;
use crate::InstructionError;
use solana_accounts::account::{Account, AccountData, Pubkey};
use solana_accounts::store::AccountsDB;

/// Process a token program instruction
pub fn process_instruction(
    data: &[u8],
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    account_keys: &[Pubkey],
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    let instruction = TokenInstruction::from_data(data)?;

    match instruction {
        TokenInstruction::InitializeMint => {
            process_initialize_mint(account_metas, signer, account_keys, accounts, budget, logs)
        }
        TokenInstruction::InitializeAccount { mint } => process_initialize_account(
            mint,
            account_metas,
            signer,
            account_keys,
            accounts,
            budget,
            logs,
        ),
        TokenInstruction::Transfer { amount } => process_token_transfer(
            amount,
            account_metas,
            signer,
            account_keys,
            accounts,
            budget,
            logs,
        ),
        TokenInstruction::MintTo { amount } => process_mint_to(
            amount,
            account_metas,
            signer,
            account_keys,
            accounts,
            budget,
            logs,
        ),
        TokenInstruction::Burn { amount } => process_burn(
            amount,
            account_metas,
            signer,
            account_keys,
            accounts,
            budget,
            logs,
        ),
    }
}

/// Load an account, verifying it is owned by the token program.
fn load_owned(accounts: &AccountsDB, key: &Pubkey) -> Result<Account, InstructionError> {
    let account = accounts
        .load(key)
        .ok_or(InstructionError::AccountNotFound)?;
    if account.owner != solana_accounts::account::token_program_id() {
        return Err(InstructionError::InvalidAccountOwner);
    }
    Ok(account)
}

/// Process mint creation. Account 0 must be a signer (the mint authority)
/// and must not already exist.
fn process_initialize_mint(
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    account_keys: &[Pubkey],
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(300)?;

    let mint_key = account_metas
        .first()
        .ok_or(InstructionError::InvalidInstructionData(
            "InitializeMint requires 1 account".to_string(),
        ))
        .and_then(|m| resolve_account_key(m.index, account_keys))?;

    // The mint must be created by its own authority.
    if &mint_key != signer {
        return Err(InstructionError::IncorrectAuthority);
    }
    if accounts.exists(&mint_key) {
        return Err(InstructionError::ProgramError(
            "Mint already exists".to_string(),
        ));
    }

    let mint = Account::new_mint(mint_key, *signer);
    accounts.store(mint_key, &mint);

    logs.push(format!(
        "Token: InitializeMint {} authority {}",
        hex::encode(&mint_key[..8]),
        hex::encode(&signer[..8]),
    ));

    Ok(())
}

/// Process token account initialization.
///
/// Accounts: [0] = mint, [1] = new token account, [2] = owner (signer).
/// The mint must already exist, the new account must not, and the owner
/// must be the transaction signer.
fn process_initialize_account(
    mint: Pubkey,
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    account_keys: &[Pubkey],
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(300)?;

    if account_metas.len() < 3 {
        return Err(InstructionError::InvalidInstructionData(
            "InitializeAccount requires 3 accounts (mint, account, owner)".to_string(),
        ));
    }

    let mint_key = resolve_account_key(account_metas[0].index, account_keys)?;
    let new_key = resolve_account_key(account_metas[1].index, account_keys)?;
    let owner_key = resolve_account_key(account_metas[2].index, account_keys)?;

    if mint_key != mint {
        return Err(InstructionError::MintMismatch);
    }

    // Only the owner may open a token account under their own name.
    if &owner_key != signer {
        return Err(InstructionError::IncorrectAuthority);
    }

    // The mint must exist and be a real mint account.
    let mint_account = load_owned(accounts, &mint_key)?;
    if !matches!(mint_account.data, AccountData::Mint { .. }) {
        return Err(InstructionError::MintMismatch);
    }

    if accounts.exists(&new_key) {
        return Err(InstructionError::ProgramError(
            "Token account already exists".to_string(),
        ));
    }

    let token_account = Account::new_token_account(new_key, mint_key, owner_key, 0);
    accounts.store(new_key, &token_account);

    logs.push(format!(
        "Token: InitializeAccount {} for mint {} owner {}",
        hex::encode(&new_key[..8]),
        hex::encode(&mint_key[..8]),
        hex::encode(&owner_key[..8]),
    ));

    Ok(())
}

/// Process token transfer
fn process_token_transfer(
    amount: u64,
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    account_keys: &[Pubkey],
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

    let from_key = resolve_account_key(account_metas[0].index, account_keys)?;
    let to_key = resolve_account_key(account_metas[1].index, account_keys)?;

    if from_key == to_key {
        return Err(InstructionError::InvalidInstructionData(
            "Source and destination must differ".to_string(),
        ));
    }

    // Source and destination must both be real token accounts of the same
    // mint. A transfer may never create accounts or bridge two mints.
    let mut from_account = load_owned(accounts, &from_key)?;
    let mut to_account = load_owned(accounts, &to_key)?;

    let (from_mint, from_authority, from_balance) = match &from_account.data {
        AccountData::Token {
            mint,
            authority,
            amount,
        } => (*mint, *authority, *amount),
        _ => return Err(InstructionError::MintMismatch),
    };

    // AUTHORIZATION: only the token account's authority may move its funds.
    if &from_authority != signer {
        return Err(InstructionError::IncorrectAuthority);
    }

    let (to_mint, to_balance) = match &to_account.data {
        AccountData::Token { mint, amount, .. } => (*mint, *amount),
        _ => return Err(InstructionError::MintMismatch),
    };

    if from_mint != to_mint {
        return Err(InstructionError::MintMismatch);
    }

    if from_balance < amount {
        return Err(InstructionError::InsufficientFunds {
            needed: amount,
            available: from_balance,
        });
    }

    // Apply the debit/credit to loaded copies, then persist. Arithmetic is
    // checked — a release-mode u64 wrap must not become free money.
    let new_from = from_balance
        .checked_sub(amount)
        .ok_or(InstructionError::InsufficientFunds {
            needed: amount,
            available: from_balance,
        })?;
    let new_to = to_balance
        .checked_add(amount)
        .ok_or(InstructionError::ArithmeticOverflow)?;

    if let AccountData::Token {
        amount: ref mut balance,
        ..
    } = from_account.data
    {
        *balance = new_from;
    }
    accounts.store(from_key, &from_account);

    if let AccountData::Token {
        amount: ref mut balance,
        ..
    } = to_account.data
    {
        *balance = new_to;
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
    signer: &Pubkey,
    account_keys: &[Pubkey],
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(200)?;

    if account_metas.len() < 2 {
        return Err(InstructionError::InvalidInstructionData(
            "MintTo requires 2 accounts (mint, destination)".to_string(),
        ));
    }

    let mint_key = resolve_account_key(account_metas[0].index, account_keys)?;
    let dest_key = resolve_account_key(account_metas[1].index, account_keys)?;

    // Load the mint and check the signer holds mint authority.
    let mut mint_account = load_owned(accounts, &mint_key)?;
    let (mint_authority, supply) = match &mint_account.data {
        AccountData::Mint { authority, supply } => (*authority, *supply),
        _ => return Err(InstructionError::MintMismatch),
    };

    if &mint_authority != signer {
        return Err(InstructionError::IncorrectAuthority);
    }

    // The destination must be a token account of THIS mint. Minting must
    // never create an account and must never cross mints.
    let mut dest_account = load_owned(accounts, &dest_key)?;
    let (dest_mint, dest_balance) = match &dest_account.data {
        AccountData::Token { mint, amount, .. } => (*mint, *amount),
        _ => return Err(InstructionError::MintMismatch),
    };
    if dest_mint != mint_key {
        return Err(InstructionError::MintMismatch);
    }

    let new_supply = supply
        .checked_add(amount)
        .ok_or(InstructionError::ArithmeticOverflow)?;
    let new_balance = dest_balance
        .checked_add(amount)
        .ok_or(InstructionError::ArithmeticOverflow)?;

    if let AccountData::Mint {
        supply: ref mut s, ..
    } = mint_account.data
    {
        *s = new_supply;
    }
    accounts.store(mint_key, &mint_account);

    if let AccountData::Token {
        amount: ref mut balance,
        ..
    } = dest_account.data
    {
        *balance = new_balance;
    }
    accounts.store(dest_key, &dest_account);

    logs.push(format!(
        "Token: Mint {} tokens to {} (supply {})",
        amount,
        hex::encode(&dest_key[..8]),
        new_supply,
    ));

    Ok(())
}

/// Process burning tokens
fn process_burn(
    amount: u64,
    account_metas: &[AccountMeta],
    signer: &Pubkey,
    account_keys: &[Pubkey],
    accounts: &AccountsDB,
    budget: &mut ComputeBudget,
    logs: &mut Vec<String>,
) -> Result<(), InstructionError> {
    budget.consume(200)?;

    if account_metas.len() < 2 {
        return Err(InstructionError::InvalidInstructionData(
            "Burn requires 2 accounts (mint, token account)".to_string(),
        ));
    }

    let mint_key = resolve_account_key(account_metas[0].index, account_keys)?;
    let token_key = resolve_account_key(account_metas[1].index, account_keys)?;

    let mut mint_account = load_owned(accounts, &mint_key)?;
    let supply = match &mint_account.data {
        AccountData::Mint { supply, .. } => *supply,
        _ => return Err(InstructionError::MintMismatch),
    };

    let mut token_account = load_owned(accounts, &token_key)?;
    let (token_mint, token_authority, balance) = match &token_account.data {
        AccountData::Token {
            mint,
            authority,
            amount,
        } => (*mint, *authority, *amount),
        _ => return Err(InstructionError::MintMismatch),
    };

    // AUTHORIZATION: only the token account's authority may destroy it.
    if &token_authority != signer {
        return Err(InstructionError::IncorrectAuthority);
    }

    if token_mint != mint_key {
        return Err(InstructionError::MintMismatch);
    }

    if balance < amount {
        return Err(InstructionError::InsufficientFunds {
            needed: amount,
            available: balance,
        });
    }
    if supply < amount {
        return Err(InstructionError::InsufficientFunds {
            needed: amount,
            available: supply,
        });
    }

    let new_supply = supply - amount;
    let new_balance = balance - amount;

    if let AccountData::Mint {
        supply: ref mut s, ..
    } = mint_account.data
    {
        *s = new_supply;
    }
    accounts.store(mint_key, &mint_account);

    if let AccountData::Token {
        amount: ref mut balance,
        ..
    } = token_account.data
    {
        *balance = new_balance;
    }
    accounts.store(token_key, &token_account);

    logs.push(format!(
        "Token: Burn {} tokens from {} (supply {})",
        amount,
        hex::encode(&token_key[..8]),
        new_supply,
    ));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_accounts::account::AccountData;

    /// A mint owned by `authority`, plus a token account under the same mint.
    fn setup_mint_and_account(
        accounts: &AccountsDB,
        mint: Pubkey,
        authority: Pubkey,
        holder: Pubkey,
        amount: u64,
    ) {
        accounts.store(mint, &Account::new_mint(mint, authority));
        accounts.store(
            holder,
            &Account::new_token_account(holder, mint, authority, amount),
        );
    }

    fn budget() -> ComputeBudget {
        ComputeBudget::new(10_000)
    }

    #[test]
    fn test_token_transfer_by_authority_succeeds() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [10u8; 32];
        let owner = [20u8; 32];
        let to_key = [21u8; 32];
        setup_mint_and_account(&accounts, mint, owner, owner, 1000);
        setup_mint_and_account(&accounts, mint, to_key, to_key, 0);

        let keys = [owner, to_key];
        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        process_token_transfer(
            500,
            &metas,
            &owner,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap();

        let from = accounts.load(&owner).unwrap();
        let to = accounts.load(&to_key).unwrap();
        match (from.data, to.data) {
            (AccountData::Token { amount: f, .. }, AccountData::Token { amount: t, .. }) => {
                assert_eq!(f, 500);
                assert_eq!(t, 500);
            }
            _ => panic!("expected token accounts"),
        }
    }

    #[test]
    fn test_token_transfer_by_non_authority_is_rejected() {
        // THE regression test: an attacker with a valid signature must not be
        // able to move funds out of someone else's token account.
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [10u8; 32];
        let victim = [20u8; 32];
        let attacker = [99u8; 32];
        let to_key = [21u8; 32];
        setup_mint_and_account(&accounts, mint, victim, victim, 1000);
        setup_mint_and_account(&accounts, mint, to_key, to_key, 0);

        let keys = [victim, to_key];
        let metas = vec![
            AccountMeta::new(0, false, true),
            AccountMeta::new(1, false, true),
        ];

        let err = process_token_transfer(
            500,
            &metas,
            &attacker,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::IncorrectAuthority));
        // Nothing moved.
        match accounts.load(&victim).unwrap().data {
            AccountData::Token { amount, .. } => assert_eq!(amount, 1000),
            _ => panic!("expected token account"),
        }
    }

    #[test]
    fn test_token_transfer_across_mints_is_rejected() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint_a = [10u8; 32];
        let mint_b = [11u8; 32];
        let owner = [20u8; 32];
        let to_key = [21u8; 32];
        setup_mint_and_account(&accounts, mint_a, owner, owner, 1000);
        setup_mint_and_account(&accounts, mint_b, to_key, to_key, 0);

        let keys = [owner, to_key];
        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        let err = process_token_transfer(
            500,
            &metas,
            &owner,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::MintMismatch));
    }

    #[test]
    fn test_token_transfer_to_missing_account_is_rejected() {
        // A transfer must never create a token account as a side effect.
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [10u8; 32];
        let owner = [20u8; 32];
        let ghost = [21u8; 32];
        setup_mint_and_account(&accounts, mint, owner, owner, 1000);

        let keys = [owner, ghost];
        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        let err = process_token_transfer(
            500,
            &metas,
            &owner,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::AccountNotFound));
        assert!(!accounts.exists(&ghost));
    }

    #[test]
    fn test_token_index_out_of_range_is_rejected() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        // Empty key list: index-based synthetic keys are gone
        let err = process_token_transfer(
            500,
            &metas,
            &[20u8; 32],
            &[],
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::AccountNotFound));
    }

    #[test]
    fn test_token_insufficient_balance() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [10u8; 32];
        let from_key = [20u8; 32];
        let to_key = [21u8; 32];
        setup_mint_and_account(&accounts, mint, from_key, from_key, 100);
        setup_mint_and_account(&accounts, mint, to_key, to_key, 0);

        let keys = [from_key, to_key];
        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        let err = process_token_transfer(
            200,
            &metas,
            &from_key,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::InsufficientFunds { .. }));
    }

    #[test]
    fn test_mint_to_updates_supply() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [30u8; 32];
        let authority = [31u8; 32];
        let dest = [32u8; 32];
        setup_mint_and_account(&accounts, mint, authority, dest, 0);

        let keys = [mint, dest];
        let metas = vec![
            AccountMeta::new(0, true, true),
            AccountMeta::new(1, false, true),
        ];

        process_mint_to(
            1000,
            &metas,
            &authority,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap();

        match accounts.load(&mint).unwrap().data {
            AccountData::Mint { supply, .. } => assert_eq!(supply, 1000),
            _ => panic!("expected mint"),
        }
        match accounts.load(&dest).unwrap().data {
            AccountData::Token { amount, .. } => assert_eq!(amount, 1000),
            _ => panic!("expected token account"),
        }
    }

    #[test]
    fn test_mint_to_by_non_authority_is_rejected() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [30u8; 32];
        let authority = [31u8; 32];
        let dest = [32u8; 32];
        let attacker = [33u8; 32];
        setup_mint_and_account(&accounts, mint, authority, dest, 0);

        let keys = [mint, dest];
        let metas = vec![
            AccountMeta::new(0, false, true),
            AccountMeta::new(1, false, true),
        ];

        let err = process_mint_to(
            1000,
            &metas,
            &attacker,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::IncorrectAuthority));
        match accounts.load(&dest).unwrap().data {
            AccountData::Token { amount, .. } => assert_eq!(amount, 0),
            _ => panic!("expected token account"),
        }
    }

    #[test]
    fn test_burn_reduces_supply() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [30u8; 32];
        let authority = [31u8; 32];
        let token_key = [32u8; 32];
        setup_mint_and_account(&accounts, mint, authority, token_key, 500);
        // Give the mint a supply consistent with the outstanding balance.
        let mut mint_acc = accounts.load(&mint).unwrap();
        if let AccountData::Mint { supply, .. } = &mut mint_acc.data {
            *supply = 500;
        }
        accounts.store(mint, &mint_acc);

        let keys = [mint, token_key];
        let metas = vec![
            AccountMeta::new(0, false, true),
            AccountMeta::new(1, true, true),
        ];

        process_burn(
            200,
            &metas,
            &authority,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap();

        match accounts.load(&token_key).unwrap().data {
            AccountData::Token { amount, .. } => assert_eq!(amount, 300),
            _ => panic!("expected token account"),
        }
        match accounts.load(&mint).unwrap().data {
            AccountData::Mint { supply, .. } => assert_eq!(supply, 300),
            _ => panic!("expected mint"),
        }
    }

    #[test]
    fn test_burn_by_non_authority_is_rejected() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [30u8; 32];
        let authority = [31u8; 32];
        let token_key = [32u8; 32];
        let attacker = [34u8; 32];
        setup_mint_and_account(&accounts, mint, authority, token_key, 500);

        let keys = [mint, token_key];
        let metas = vec![
            AccountMeta::new(0, false, true),
            AccountMeta::new(1, false, true),
        ];

        let err = process_burn(
            200,
            &metas,
            &attacker,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::IncorrectAuthority));
        match accounts.load(&token_key).unwrap().data {
            AccountData::Token { amount, .. } => assert_eq!(amount, 500),
            _ => panic!("expected token account"),
        }
    }

    #[test]
    fn test_initialize_mint_creates_real_account() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [40u8; 32];
        let keys = [mint];
        let metas = vec![AccountMeta::new(0, true, true)];

        process_initialize_mint(&metas, &mint, &keys, &accounts, &mut budget, &mut logs).unwrap();

        match accounts.load(&mint).unwrap().data {
            AccountData::Mint { authority, supply } => {
                assert_eq!(authority, mint);
                assert_eq!(supply, 0);
            }
            _ => panic!("expected a real mint account"),
        }
    }

    #[test]
    fn test_initialize_account_creates_real_account() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [40u8; 32];
        let owner = [41u8; 32];
        let token_account = [42u8; 32];
        accounts.store(mint, &Account::new_mint(mint, [99u8; 32]));

        let keys = [mint, token_account, owner];
        let metas = vec![
            AccountMeta::new(0, false, false),
            AccountMeta::new(1, false, true),
            AccountMeta::new(2, true, false),
        ];

        process_initialize_account(
            mint,
            &metas,
            &owner,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap();

        let created = accounts.load(&token_account).expect("account must exist");
        assert_eq!(created.owner, solana_accounts::account::token_program_id());
        match created.data {
            AccountData::Token {
                mint: m,
                authority,
                amount,
            } => {
                assert_eq!(m, mint);
                assert_eq!(authority, owner);
                assert_eq!(amount, 0);
            }
            _ => panic!("expected a real token account"),
        }
    }

    #[test]
    fn test_initialize_account_for_non_owner_is_rejected() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [40u8; 32];
        let owner = [41u8; 32];
        let token_account = [42u8; 32];
        let attacker = [43u8; 32];
        accounts.store(mint, &Account::new_mint(mint, attacker));

        let keys = [mint, token_account, owner];
        let metas = vec![
            AccountMeta::new(0, false, false),
            AccountMeta::new(1, false, true),
            AccountMeta::new(2, false, false),
        ];

        let err = process_initialize_account(
            mint,
            &metas,
            &attacker,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::IncorrectAuthority));
        assert!(!accounts.exists(&token_account));
    }

    #[test]
    fn test_initialize_account_mint_must_exist() {
        let accounts = AccountsDB::new();
        let mut budget = budget();
        let mut logs = Vec::new();

        let mint = [40u8; 32];
        let owner = [41u8; 32];
        let token_account = [42u8; 32];
        let keys = [mint, token_account, owner];
        let metas = vec![
            AccountMeta::new(0, false, false),
            AccountMeta::new(1, false, true),
            AccountMeta::new(2, true, false),
        ];

        let err = process_initialize_account(
            mint,
            &metas,
            &owner,
            &keys,
            &accounts,
            &mut budget,
            &mut logs,
        )
        .unwrap_err();

        assert!(matches!(err, InstructionError::AccountNotFound));
        assert!(!accounts.exists(&token_account));
    }
}
