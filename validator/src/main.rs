//! # Solana-like Validator
//!
//! A custom validator implementation inspired by Solana's architecture.
//! Features:
//! - Proof of History (PoH) clock
//! - Tower BFT consensus
//! - Sealevel-like parallel transaction execution
//! - Turbine block propagation with erasure coding
//! - CRDS gossip protocol
//! - JSON-RPC API
//!
//! This is an educational/research project — not a production validator!

use clap::Parser;
use std::sync::Arc;

use solana_accounts::account::{Account, Pubkey};
use solana_accounts::AccountsDB;
use solana_consensus::leader::{LeaderSchedule, ValidatorStake as LeaderStake};
use solana_consensus::tower::Tower;
use solana_gossip::Crds;
use solana_poh::{PohHash, PohHasher};
use solana_rpc::{RpcHandler, RpcServer};
use solana_turbine::ErasureCoder;
use solana_tx_processor::transaction::AccountMeta;
use solana_tx_processor::{Executor, Instruction, Transaction};

/// CLI arguments
#[derive(Parser, Debug)]
#[command(name = "solana-validator")]
#[command(about = "Custom Solana-like validator with PoH + Tower BFT")]
struct Args {
    /// Validator identity keypair (hex)
    #[arg(long, default_value = "")]
    identity: String,

    /// RPC listen port
    #[arg(long, default_value = "8899")]
    rpc_port: u16,

    /// Gossip listen port
    #[arg(long, default_value = "8000")]
    gossip_port: u16,

    /// TPU port
    #[arg(long, default_value = "8001")]
    tpu_port: u16,

    /// Enable mining mode (produce blocks)
    #[arg(long)]
    mining: bool,

    /// Genesis hash (hex)
    #[arg(long, default_value = "")]
    genesis_hash: String,

    /// Enable JSON-RPC API
    #[arg(long, default_value = "true")]
    rpc_enabled: bool,

    /// Run benchmarks
    #[arg(long)]
    bench: bool,
}

/// Validator state
struct ValidatorState {
    /// PoH hasher
    poh: PohHasher,
    /// Accounts database
    accounts: Arc<AccountsDB>,
    /// Transaction executor
    executor: Arc<Executor>,
    /// Tower BFT consensus
    tower: Arc<Tower>,
    /// Leader schedule
    leader_schedule: LeaderSchedule,
    /// Current slot
    current_slot: u64,
    /// Block height
    block_height: u64,
    /// Identity
    identity: [u8; 32],
}

impl ValidatorState {
    fn new(identity: [u8; 32]) -> Self {
        let accounts = Arc::new(AccountsDB::new());
        let executor = Arc::new(Executor::new(accounts.clone()));
        let tower = Arc::new(Tower::new(0));

        let validators = vec![LeaderStake {
            validator: identity,
            lamports: 10_000_000,
            name: "self".to_string(),
        }];

        let leader_schedule = LeaderSchedule::new(validators, 4, 432_000);

        let genesis_hash = PohHash::from_seed("genesis");
        let poh = PohHasher::new(genesis_hash, 0);

        Self {
            poh,
            accounts,
            executor,
            tower,
            leader_schedule,
            current_slot: 0,
            block_height: 0,
            identity,
        }
    }

    /// Process a tick (PoH heartbeat)
    fn process_tick(&mut self) {
        self.poh.hash_n(6000); // hashes_per_tick
    }

    /// Process a transaction
    fn process_transaction(&self, tx: &Transaction) -> solana_tx_processor::TransactionResult {
        self.executor.execute_transaction(tx)
    }

    /// Check if we're the leader for current slot
    fn is_leader(&self) -> bool {
        self.leader_schedule
            .is_leader(&self.identity, self.current_slot)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    // Generate or parse identity
    let identity: [u8; 32] = if args.identity.is_empty() {
        let mut key = [0u8; 32];
        rand::Rng::fill(&mut rand::thread_rng(), &mut key);
        key
    } else {
        let bytes = hex::decode(&args.identity)?;
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        key
    };

    tracing::info!("╔══════════════════════════════════════════════╗");
    tracing::info!("║     🔒 SOLANA-LIKE VALIDATOR v0.1.0         ║");
    tracing::info!("║     PoH + Tower BFT + Sealevel              ║");
    tracing::info!("╚══════════════════════════════════════════════╝");
    tracing::info!("");
    tracing::info!("Identity: {}", hex::encode(identity));
    tracing::info!("RPC port: {}", args.rpc_port);
    tracing::info!("Gossip port: {}", args.gossip_port);

    // Run benchmarks if requested
    if args.bench {
        run_benchmarks().await;
        return Ok(());
    }

    // Initialize state
    let state = Arc::new(parking_lot::RwLock::new(ValidatorState::new(identity)));

    // Initialize CRDS
    let _crds = Arc::new(Crds::default_store());

    // Initialize RPC
    let accounts_db = Arc::new(AccountsDB::new());
    let rpc_handler = Arc::new(RpcHandler::new(accounts_db.clone(), identity));

    // Create some test accounts
    create_genesis_accounts(&accounts_db);

    // Start tasks
    let mut handles = Vec::new();

    // PoH loop
    {
        let state = state.clone();
        handles.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                std::time::Duration::from_millis(400), // ~400ms slot time
            );

            loop {
                interval.tick().await;
                let mut s = state.write();
                s.process_tick();

                if s.current_slot % 100 == 0 {
                    tracing::info!(
                        "Slot {} | Height {} | Leader: {} | Accounts: {}",
                        s.current_slot,
                        s.block_height,
                        s.is_leader(),
                        s.accounts.account_count(),
                    );
                }
            }
        }));
    }

    // RPC server
    if args.rpc_enabled {
        let handler = rpc_handler.clone();
        let _state = state.clone();
        let rpc_addr: std::net::SocketAddr = format!("127.0.0.1:{}", args.rpc_port).parse()?;
        let server = RpcServer::new(handler, rpc_addr);

        handles.push(tokio::spawn(async move {
            if let Err(e) = server.start().await {
                tracing::error!("RPC server error: {}", e);
            }
        }));
    }

    // Block production loop (if mining)
    if args.mining {
        let state = state.clone();
        handles.push(tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(400));

            loop {
                interval.tick().await;

                // Check leadership and produce block in one scope
                let should_produce = {
                    let s = state.read();
                    s.is_leader()
                };
                if should_produce {
                    produce_block(&state).await;
                }
            }
        }));
    }

    // Print startup info
    print_validator_info(&state);

    // Wait for all tasks
    for handle in handles {
        handle.await?;
    }

    Ok(())
}

/// Create genesis accounts for testing
fn create_genesis_accounts(db: &AccountsDB) {
    // System program
    let system_program = Account::new_system_account([1u8; 32], 0);
    db.store([1u8; 32], &system_program);

    // Token program
    let token_program = Account::new_system_account([2u8; 32], 0);
    db.store([2u8; 32], &token_program);

    // Faucet account (for testing)
    let faucet = Account::new_system_account([3u8; 32], 1_000_000_000_000); // 1000 SOL
    db.store([3u8; 32], &faucet);

    tracing::info!("Genesis accounts created: system, token, faucet");
}

/// Produce a new block
async fn produce_block(state: &Arc<parking_lot::RwLock<ValidatorState>>) {
    let mut s = state.write();

    // Record tick
    s.process_tick();

    // Get current hash
    let block_hash = s.poh.current();

    tracing::info!(
        "📦 Producing block at slot {} | hash: {}",
        s.current_slot,
        &block_hash.to_hex()[..16],
    );

    // Advance slot
    s.current_slot += 1;
    s.block_height += 1;

    // Record a tick in the PoH
    s.process_tick();
}

/// Print validator information
fn print_validator_info(state: &Arc<parking_lot::RwLock<ValidatorState>>) {
    let s = state.read();

    tracing::info!("");
    tracing::info!("┌─────────────────────────────────────────────┐");
    tracing::info!("│              Validator Status                │");
    tracing::info!("├─────────────────────────────────────────────┤");
    tracing::info!("│ Slot:         {:>28} │", s.current_slot);
    tracing::info!("│ Block Height: {:>28} │", s.block_height);
    tracing::info!("│ Leader:       {:>28} │", s.is_leader());
    tracing::info!("│ Accounts:     {:>28} │", s.accounts.account_count());
    tracing::info!("│ PoH Hashes:   {:>28} │", s.poh.count());
    tracing::info!("│ Tower Nodes:  {:>28} │", s.tower.stats().total_nodes);
    tracing::info!("└─────────────────────────────────────────────┘");
    tracing::info!("");
}

/// Run performance benchmarks
async fn run_benchmarks() {
    tracing::info!("Running benchmarks...");
    tracing::info!("");

    // PoH benchmark
    {
        let start = std::time::Instant::now();
        let seed = PohHash::from_seed("benchmark");
        let mut hasher = PohHasher::new(seed, 0);

        let iterations = 1_000_000;
        hasher.hash_n(iterations);

        let elapsed = start.elapsed();
        let rate = iterations as f64 / elapsed.as_secs_f64();
        tracing::info!("PoH ({} iterations): {:.2} hashes/sec", iterations, rate);
    }

    // Accounts benchmark
    {
        let start = std::time::Instant::now();
        let db = AccountsDB::new();

        for i in 0..10_000 {
            let mut key = [0u8; 32];
            key[0] = (i % 256) as u8;
            key[1] = (i / 256) as u8;
            let acc = Account::new_system_account(key, i as u64);
            db.store(key, &acc);
        }

        let elapsed = start.elapsed();
        tracing::info!("Accounts DB (10k inserts): {:.2?}", elapsed);
    }

    // State root benchmark
    {
        let db = AccountsDB::new();
        for i in 0..1_000 {
            let mut key = [0u8; 32];
            key[0] = (i % 256) as u8;
            key[1] = (i / 256) as u8;
            let acc = Account::new_system_account(key, i as u64);
            db.store(key, &acc);
        }

        let start = std::time::Instant::now();
        let _root = db.compute_state_root();
        let elapsed = start.elapsed();
        tracing::info!("State root (1k accounts): {:.2?}", elapsed);
    }

    // Erasure coding benchmark
    {
        let start = std::time::Instant::now();
        let coder = ErasureCoder::default_solana();
        let data = vec![42u8; 1228 * 16]; // 16 shreds worth

        for _ in 0..100 {
            let shreds = coder.encode(&data, 1);
            let _decoded = coder.decode(&shreds);
        }

        let elapsed = start.elapsed();
        tracing::info!("Erasure coding (100 encode/decode): {:.2?}", elapsed);
    }

    // Transaction execution benchmark
    {
        let db = Arc::new(AccountsDB::new());
        let executor = Arc::new(Executor::new(db.clone()));

        let signer: Pubkey = {
            let mut key = [0u8; 32];
            key[0] = 42;
            key
        };
        let acc = Account::new_system_account(signer, 10_000_000);
        db.store(signer, &acc);

        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let ix = Instruction {
                program_id: [1u8; 32], // System program
                accounts: vec![AccountMeta {
                    index: 0,
                    is_signer: true,
                    is_writable: true,
                }],
                data: vec![],
            };
            let mut tx = Transaction::new(signer, vec![ix], [0u8; 32]);
            tx.signature = vec![1u8; 64];
            let _result = executor.execute_transaction(&tx);
        }
        let elapsed = start.elapsed();
        tracing::info!("Transaction execution (10k txs): {:.2?}", elapsed);
    }

    tracing::info!("");
    tracing::info!("Benchmarks complete!");
}
