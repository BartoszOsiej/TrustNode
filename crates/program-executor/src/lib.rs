//! # Program Executor
//!
//! A SBF-like instruction processing VM for the validator.
//! Instead of full SBF/EBPF, we implement a register-based VM
//! with a rich instruction set that covers the most common
//! Solana program operations.
//!
//! ## Architecture
//!
//! ```text
//! Transaction
//!     │
//!     ▼
//! InstructionProcessor
//!     │
//!     ├── System Program (11111111...)
//!     ├── Token Program (TokenkegQ...)
//!     ├── Memo Program (MemoSq4...)
//!     └── Custom Programs (loaded from account data)
//! ```

pub mod instruction;
pub mod processor;
pub mod programs;

pub use instruction::{Instruction, InstructionResult};
pub use processor::InstructionProcessor;
pub use programs::system_program;

/// Compute budget per instruction
pub const DEFAULT_COMPUTE_UNITS: u64 = 200;
pub const MAX_COMPUTE_UNITS: u64 = 200_000;

/// Instruction error
#[derive(Debug, Clone)]
pub enum InstructionError {
    /// Insufficient compute units
    InsufficientComputeUnits,
    /// Invalid instruction data
    InvalidInstructionData(String),
    /// Account not found
    AccountNotFound,
    /// Account not writable
    AccountNotWritable,
    /// Insufficient funds
    InsufficientFunds { needed: u64, available: u64 },
    /// Program error
    ProgramError(String),
    /// Invalid account owner
    InvalidAccountOwner,
    /// Arithmetic overflow
    ArithmeticOverflow,
    /// Custom error
    Custom(u32, String),
}

impl std::fmt::Display for InstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientComputeUnits => write!(f, "Insufficient compute units"),
            Self::InvalidInstructionData(msg) => write!(f, "Invalid instruction data: {}", msg),
            Self::AccountNotFound => write!(f, "Account not found"),
            Self::AccountNotWritable => write!(f, "Account not writable"),
            Self::InsufficientFunds { needed, available } => {
                write!(f, "Insufficient funds: need {}, have {}", needed, available)
            }
            Self::ProgramError(msg) => write!(f, "Program error: {}", msg),
            Self::InvalidAccountOwner => write!(f, "Invalid account owner"),
            Self::ArithmeticOverflow => write!(f, "Arithmetic overflow"),
            Self::Custom(code, msg) => write!(f, "Custom error {}: {}", code, msg),
        }
    }
}

impl std::error::Error for InstructionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instruction_error_display() {
        let err = InstructionError::InsufficientFunds {
            needed: 1000,
            available: 500,
        };
        assert!(err.to_string().contains("1000"));
        assert!(err.to_string().contains("500"));
    }

    #[test]
    fn test_compute_budget() {
        assert!(MAX_COMPUTE_UNITS > DEFAULT_COMPUTE_UNITS);
        assert!(DEFAULT_COMPUTE_UNITS > 0);
    }
}
