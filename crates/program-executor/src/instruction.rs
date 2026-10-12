//! Instruction types and results for the program executor

use solana_accounts::account::Pubkey;

/// A processed instruction
#[derive(Debug, Clone)]
pub struct Instruction {
    /// Program to invoke
    pub program_id: Pubkey,
    /// Account indices this instruction operates on
    pub account_metas: Vec<AccountMeta>,
    /// Instruction data
    pub data: Vec<u8>,
}

/// Account metadata for an instruction
#[derive(Debug, Clone)]
pub struct AccountMeta {
    /// Index into the transaction's account list
    pub index: usize,
    /// Is this account a signer
    pub is_signer: bool,
    /// Is this account writable
    pub is_writable: bool,
}

impl AccountMeta {
    pub fn new(index: usize, is_signer: bool, is_writable: bool) -> Self {
        Self {
            index,
            is_signer,
            is_writable,
        }
    }

    pub fn readonly(index: usize) -> Self {
        Self {
            index,
            is_signer: false,
            is_writable: false,
        }
    }

    pub fn writable(index: usize) -> Self {
        Self {
            index,
            is_signer: false,
            is_writable: true,
        }
    }

    pub fn signer_writable(index: usize) -> Self {
        Self {
            index,
            is_signer: true,
            is_writable: true,
        }
    }
}

/// Resolve an instruction's account index against the transaction's account
/// key list. There is no synthetic fallback: an index outside the list is an
/// error, exactly like a malformed transaction on Solana.
pub fn resolve_account_key(
    index: usize,
    account_keys: &[Pubkey],
) -> Result<Pubkey, crate::InstructionError> {
    account_keys
        .get(index)
        .copied()
        .ok_or(crate::InstructionError::AccountNotFound)
}

/// Require that an account meta resolves to the transaction's signer.
///
/// The previous check was `resolved != signer && !meta.is_signer` — an AND of
/// two negations, so a meta flagged `is_signer` that resolved to a *different*
/// key sailed through. The `is_signer` flag is a claim made by the transaction;
/// the binding to the actually-verified signer is the claim that matters, so
/// that is the only thing enforced here.
pub fn require_signer(
    meta: &AccountMeta,
    signer: &Pubkey,
    account_keys: &[Pubkey],
) -> Result<Pubkey, crate::InstructionError> {
    let key = resolve_account_key(meta.index, account_keys)?;
    if &key != signer {
        return Err(crate::InstructionError::IncorrectAuthority);
    }
    Ok(key)
}

/// Result of executing an instruction
#[derive(Debug, Clone)]
pub struct InstructionResult {
    /// Did the instruction succeed
    pub success: bool,
    /// Compute units consumed
    pub compute_units_consumed: u64,
    /// Return data (if any)
    pub return_data: Option<Vec<u8>>,
    /// Error (if any)
    pub error: Option<crate::InstructionError>,
    /// Execution logs
    pub logs: Vec<String>,
}

impl InstructionResult {
    /// Build a result carrying the full program log. The previous
    /// `success`/`failure` constructors hardcoded `logs: vec![]`, which meant
    /// every `logs.push` inside a program was discarded at this boundary and
    /// clients never saw a single log line.
    pub fn with_logs(
        success: bool,
        compute_units: u64,
        error: Option<crate::InstructionError>,
        logs: Vec<String>,
    ) -> Self {
        Self {
            success,
            compute_units_consumed: compute_units,
            return_data: None,
            error,
            logs,
        }
    }

    pub fn success(compute_units: u64) -> Self {
        Self {
            success: true,
            compute_units_consumed: compute_units,
            return_data: None,
            error: None,
            logs: vec![],
        }
    }

    pub fn with_log(mut self, msg: String) -> Self {
        self.logs.push(msg);
        self
    }

    pub fn with_return_data(mut self, data: Vec<u8>) -> Self {
        self.return_data = Some(data);
        self
    }

    pub fn failure(error: crate::InstructionError, compute_units: u64) -> Self {
        Self {
            success: false,
            compute_units_consumed: compute_units,
            return_data: None,
            error: Some(error),
            logs: vec![],
        }
    }

    pub fn add_log(&mut self, msg: String) {
        self.logs.push(msg);
    }
}

/// System program instructions
#[derive(Debug, Clone)]
pub enum SystemInstruction {
    /// Transfer SOL between accounts
    Transfer { lamports: u64 },
    /// Create a new account
    CreateAccount {
        lamports: u64,
        space: u64,
        owner: Pubkey,
    },
    /// Assign account to a program
    Assign { owner: Pubkey },
    /// Create a new nonce account
    NonceInitialize,
}

impl SystemInstruction {
    /// Deserialize from instruction data
    pub fn from_data(data: &[u8]) -> Result<Self, crate::InstructionError> {
        if data.is_empty() {
            return Err(crate::InstructionError::InvalidInstructionData(
                "Empty instruction data".to_string(),
            ));
        }

        match data[0] {
            // Transfer instruction (system program instruction 2)
            2 => {
                if data.len() < 9 {
                    return Err(crate::InstructionError::InvalidInstructionData(
                        "Transfer requires 8 bytes".to_string(),
                    ));
                }
                let lamports = u64::from_le_bytes(data[1..9].try_into().unwrap());
                Ok(Self::Transfer { lamports })
            }
            // CreateAccount instruction (system program instruction 0)
            0 => {
                if data.len() < 49 {
                    return Err(crate::InstructionError::InvalidInstructionData(
                        "CreateAccount requires 49 bytes".to_string(),
                    ));
                }
                let lamports = u64::from_le_bytes(data[1..9].try_into().unwrap());
                let space = u64::from_le_bytes(data[9..17].try_into().unwrap());
                let mut owner = [0u8; 32];
                owner.copy_from_slice(&data[17..49]);
                Ok(Self::CreateAccount {
                    lamports,
                    space,
                    owner,
                })
            }
            // Assign instruction (system program instruction 1)
            1 => {
                if data.len() < 33 {
                    return Err(crate::InstructionError::InvalidInstructionData(
                        "Assign requires 32 bytes".to_string(),
                    ));
                }
                let mut owner = [0u8; 32];
                owner.copy_from_slice(&data[1..33]);
                Ok(Self::Assign { owner })
            }
            // NonceInitialize instruction (system program instruction 7)
            7 => Ok(Self::NonceInitialize),
            other => Err(crate::InstructionError::InvalidInstructionData(format!(
                "Unknown system instruction: {}",
                other
            ))),
        }
    }

    /// Serialize to instruction data
    pub fn to_data(&self) -> Vec<u8> {
        match self {
            Self::Transfer { lamports } => {
                let mut data = vec![2u8]; // instruction index
                data.extend_from_slice(&lamports.to_le_bytes());
                data
            }
            Self::CreateAccount {
                lamports,
                space,
                owner,
            } => {
                let mut data = vec![0u8]; // instruction index
                data.extend_from_slice(&lamports.to_le_bytes());
                data.extend_from_slice(&space.to_le_bytes());
                data.extend_from_slice(owner);
                data
            }
            Self::Assign { owner } => {
                let mut data = vec![1u8]; // instruction index
                data.extend_from_slice(owner);
                data
            }
            Self::NonceInitialize => vec![7u8],
        }
    }
}

/// Token program instructions (simplified SPL Token)
#[derive(Debug, Clone)]
pub enum TokenInstruction {
    /// Create a new mint account. Account 0 (signer) becomes the mint
    /// authority. One-time, mint must not already exist.
    InitializeMint,
    /// Create a token account holding `mint`.
    /// Accounts: `[0]` = mint, `[1]` = new token account, `[2]` = owner (signer).
    InitializeAccount { mint: Pubkey },
    /// Transfer tokens
    Transfer { amount: u64 },
    /// Mint tokens
    MintTo { amount: u64 },
    /// Burn tokens
    Burn { amount: u64 },
}

impl TokenInstruction {
    pub fn from_data(data: &[u8]) -> Result<Self, crate::InstructionError> {
        if data.is_empty() {
            return Err(crate::InstructionError::InvalidInstructionData(
                "Empty token instruction".to_string(),
            ));
        }

        match data[0] {
            0 => {
                if data.len() < 33 {
                    return Err(crate::InstructionError::InvalidInstructionData(
                        "InitializeAccount requires 32-byte mint".to_string(),
                    ));
                }
                let mut mint = [0u8; 32];
                mint.copy_from_slice(&data[1..33]);
                Ok(Self::InitializeAccount { mint })
            }
            1 => Ok(Self::InitializeMint),
            3 => {
                if data.len() < 9 {
                    return Err(crate::InstructionError::InvalidInstructionData(
                        "Transfer requires 8 bytes".to_string(),
                    ));
                }
                let amount = u64::from_le_bytes(data[1..9].try_into().unwrap());
                Ok(Self::Transfer { amount })
            }
            7 => {
                if data.len() < 9 {
                    return Err(crate::InstructionError::InvalidInstructionData(
                        "MintTo requires 8 bytes".to_string(),
                    ));
                }
                let amount = u64::from_le_bytes(data[1..9].try_into().unwrap());
                Ok(Self::MintTo { amount })
            }
            8 => {
                if data.len() < 9 {
                    return Err(crate::InstructionError::InvalidInstructionData(
                        "Burn requires 8 bytes".to_string(),
                    ));
                }
                let amount = u64::from_le_bytes(data[1..9].try_into().unwrap());
                Ok(Self::Burn { amount })
            }
            other => Err(crate::InstructionError::InvalidInstructionData(format!(
                "Unknown token instruction: {}",
                other
            ))),
        }
    }

    /// Serialize to instruction data
    pub fn to_data(&self) -> Vec<u8> {
        match self {
            Self::InitializeMint => vec![1u8],
            Self::InitializeAccount { mint } => {
                let mut data = vec![0u8];
                data.extend_from_slice(mint);
                data
            }
            Self::Transfer { amount } => {
                let mut data = vec![3u8];
                data.extend_from_slice(&amount.to_le_bytes());
                data
            }
            Self::MintTo { amount } => {
                let mut data = vec![7u8];
                data.extend_from_slice(&amount.to_le_bytes());
                data
            }
            Self::Burn { amount } => {
                let mut data = vec![8u8];
                data.extend_from_slice(&amount.to_le_bytes());
                data
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_system_transfer_roundtrip() {
        let ix = SystemInstruction::Transfer {
            lamports: 1_000_000,
        };
        let data = ix.to_data();
        let decoded = SystemInstruction::from_data(&data).unwrap();
        match decoded {
            SystemInstruction::Transfer { lamports } => assert_eq!(lamports, 1_000_000),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_create_account_roundtrip() {
        let owner = [42u8; 32];
        let ix = SystemInstruction::CreateAccount {
            lamports: 500,
            space: 1024,
            owner,
        };
        let data = ix.to_data();
        let decoded = SystemInstruction::from_data(&data).unwrap();
        match decoded {
            SystemInstruction::CreateAccount {
                lamports,
                space,
                owner: o,
            } => {
                assert_eq!(lamports, 500);
                assert_eq!(space, 1024);
                assert_eq!(o, owner);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_token_transfer_roundtrip() {
        let ix = TokenInstruction::Transfer { amount: 42 };
        let data = ix.to_data();
        let decoded = TokenInstruction::from_data(&data).unwrap();
        match decoded {
            TokenInstruction::Transfer { amount } => assert_eq!(amount, 42),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_invalid_instruction_data() {
        let result = SystemInstruction::from_data(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_account_meta_constructors() {
        let m = AccountMeta::new(0, true, true);
        assert!(m.is_signer);
        assert!(m.is_writable);

        let r = AccountMeta::readonly(1);
        assert!(!r.is_signer);
        assert!(!r.is_writable);
    }

    #[test]
    fn test_truncated_create_account_is_an_error_not_a_panic() {
        // Regression: `from_data` checked `len() < 33` and then read
        // `data[17..49]`, so any payload of 33..48 bytes — trivially
        // reachable over RPC — panicked the node with a slice-index panic.
        for len in 1..49usize {
            let mut data = vec![0u8; len];
            data[0] = 0; // CreateAccount
            let result = SystemInstruction::from_data(&data);
            assert!(result.is_err(), "payload of {} bytes must not parse", len);
        }
    }

    #[test]
    fn test_truncated_assign_is_an_error_not_a_panic() {
        for len in 1..33usize {
            let mut data = vec![0u8; len];
            data[0] = 1; // Assign
            assert!(SystemInstruction::from_data(&data).is_err());
        }
    }

    #[test]
    fn test_truncated_token_initialize_account_is_an_error_not_a_panic() {
        for len in 1..33usize {
            let mut data = vec![0u8; len];
            data[0] = 0; // InitializeAccount
            assert!(TokenInstruction::from_data(&data).is_err());
        }
    }

    #[test]
    fn test_nonce_initialize_roundtrip() {
        // `to_data` emitted `vec![7]` but `from_data` had no arm for 7, so the
        // round trip always failed.
        let data = SystemInstruction::NonceInitialize.to_data();
        assert_eq!(
            data,
            vec![7u8],
            "durable nonces are advertised but not implemented"
        );
        match SystemInstruction::from_data(&data) {
            Ok(SystemInstruction::NonceInitialize) => {}
            other => panic!("expected NonceInitialize, got {:?}", other),
        }
    }

    #[test]
    fn test_token_initialize_account_roundtrip() {
        let mint = [77u8; 32];
        let data = TokenInstruction::InitializeAccount { mint }.to_data();
        match TokenInstruction::from_data(&data).unwrap() {
            TokenInstruction::InitializeAccount { mint: m } => assert_eq!(m, mint),
            other => panic!("wrong variant: {:?}", other),
        }
    }

    #[test]
    fn test_token_initialize_mint_roundtrip() {
        let data = TokenInstruction::InitializeMint.to_data();
        match TokenInstruction::from_data(&data).unwrap() {
            TokenInstruction::InitializeMint => {}
            other => panic!("wrong variant: {:?}", other),
        }
    }

    #[test]
    fn test_require_signer_rejects_mismatched_key() {
        let keys = [[1u8; 32], [2u8; 32]];
        let meta = AccountMeta::new(0, true, true);

        assert_eq!(require_signer(&meta, &[1u8; 32], &keys).unwrap(), [1u8; 32]);
        // The `is_signer` flag is a claim by the transaction; a meta that
        // resolves to a different key must never be accepted.
        assert!(matches!(
            require_signer(&meta, &[2u8; 32], &keys),
            Err(crate::InstructionError::IncorrectAuthority)
        ));
    }
}
