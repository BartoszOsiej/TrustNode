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
use solana_blockstore::Blockstore;
use solana_consensus::leader::{LeaderSchedule, ValidatorStake as LeaderStake};
use solana_consensus::tower::Tower;
use solana_gossip::Crds;
use solana_poh::{PohHash, PohHasher};
use solana_rpc::{RpcHandler, RpcServer};
use solana_turbine::ErasureCoder;
use solana_tx_processor::transaction::AccountMeta;
use solana_tx_processor::{Executor, Instruction, Transaction};

/// PoH hashes injected per tick.
const HASHES_PER_TICK: u64 = 6000;
/// Ticks per slot. The validator ticks at ~400ms, i.e. one tick is one slot.
const TICKS_PER_SLOT: u64 = 1;

/// CLI arguments
#[derive(Parser, Debug)]
#[command(name = "solana-validator")]
#[command(about = "Custom Solana-like validator with PoH + Tower BFT")]
struct Args {
    /// Validator identity: path to a keypair file (JSON array of 64 bytes,
    /// Solana keygen format) or a 32-byte hex seed. Empty = generate random.
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

    /// Ledger directory (persistent blockstore)
    #[arg(long, default_value = "ledger")]
    ledger: String,
}

/// Validator state
struct ValidatorState {
    /// PoH hasher
    poh: PohHasher,
    /// Accounts database
    accounts: Arc<AccountsDB>,
    #[allow(dead_code)]
    /// Transaction executor
    executor: Arc<Executor>,
    /// Tower BFT consensus
    tower: Arc<Tower>,
    /// Leader schedule
    leader_schedule: LeaderSchedule,
    /// Current slot
    current_slot: u64,
    /// PoH ticks elapsed within the current slot
    tick_height: u64,
    /// Block height
    block_height: u64,
    /// Identity
    identity: [u8; 32],
    /// Persistent ledger
    ledger: Blockstore,
}

impl ValidatorState {
    fn new(
        identity: [u8; 32],
        ledger_dir: &str,
        accounts: Arc<AccountsDB>,
    ) -> anyhow::Result<Self> {
        let executor = Arc::new(Executor::new(accounts.clone()));
        let tower = Arc::new(Tower::new(0));

        let validators = vec![LeaderStake {
            validator: identity,
            lamports: 10_000_000,
            name: "self".to_string(),
        }];

        let leader_schedule = LeaderSchedule::new(validators, 4, 432_000);

        // Replay the ledger: resume slot/height and reseed the PoH clock
        // from the last persisted block so the hash chain continues cleanly.
        let ledger = Blockstore::open(ledger_dir)?;
        let replayed = ledger.len();
        let (current_slot, block_height, poh_seed) = match ledger.last() {
            Some(last) => (last.slot + 1, last.height + 1, PohHash::new(last.poh_hash)),
            None => (0, 0, PohHash::from_seed("genesis")),
        };
        if replayed > 0 {
            let last = ledger.last().expect("replayed > 0");
            tracing::info!(
                "Ledger: replayed {} blocks from {} (resuming at slot {})",
                replayed,
                ledger.dir().display(),
                current_slot,
            );
            tracing::info!(
                "Last block: height {} hash {}",
                last.height,
                &hex::encode(last.block_hash)[..16],
            );
        } else {
            tracing::info!("Ledger: fresh at {}", ledger.dir().display());
        }

        let poh = PohHasher::new(poh_seed, current_slot);

        Ok(Self {
            poh,
            accounts,
            executor,
            tower,
            leader_schedule,
            current_slot,
            tick_height: 0,
            block_height,
            identity,
            ledger,
        })
    }

    /// Process a tick (PoH heartbeat) and advance the slot once the slot's
    /// ticks are exhausted.
    fn process_tick(&mut self) {
        self.poh.hash_n(HASHES_PER_TICK);
        self.tick_height += 1;

        if self.tick_height >= TICKS_PER_SLOT {
            self.tick_height = 0;
            self.advance_slot();
        }
    }

    /// Move to the next slot: bump the counter and re-seed the PoH clock so
    /// the hash chain is tagged with the new slot.
    fn advance_slot(&mut self) {
        self.current_slot += 1;
        self.poh.new_slot(self.current_slot);

        if self.current_slot.is_multiple_of(100) {
            tracing::info!(
                "Slot {} | Height {} | Leader: {} | Accounts: {}",
                self.current_slot,
                self.block_height,
                self.is_leader(),
                self.accounts.account_count(),
            );
        }
    }

    /// Process a transaction
    #[allow(dead_code)]
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

    // Identity comes from a Solana-style keypair file, or a raw 32-byte hex
    // seed. There is no unkeyed identity — the public key is always derived.
    let signing_key = load_identity(&args.identity)?;
    let identity = signing_key.verifying_key().to_bytes();

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
        run_benchmarks(&signing_key).await;
        return Ok(());
    }

    // A single accounts database backs the executor, the RPC API and the
    // genesis setup — otherwise the validator and its RPC would each hold a
    // different, half-empty world.
    let accounts = Arc::new(AccountsDB::new());
    create_genesis_accounts(&accounts);

    // Initialize state
    let state = Arc::new(parking_lot::RwLock::new(ValidatorState::new(
        identity,
        &args.ledger,
        accounts.clone(),
    )?));

    // Initialize CRDS
    let _crds = Arc::new(Crds::default_store());

    // Initialize RPC on the same accounts database
    let rpc_handler = Arc::new(RpcHandler::new(accounts.clone(), identity));

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

/// Load a validator identity from a keypair file or a hex seed.
///
/// Accepts either a Solana-style keypair file (a JSON array of 64 bytes: a
/// 32-byte seed followed by the 32-byte public key) or a raw 32-byte seed in
/// hex. An empty spec generates a fresh random identity.
fn load_identity(spec: &str) -> anyhow::Result<ed25519_dalek::SigningKey> {
    if spec.is_empty() {
        let mut seed = [0u8; 32];
        rand::RngExt::fill(&mut rand::rng(), &mut seed);
        return Ok(ed25519_dalek::SigningKey::from_bytes(&seed));
    }

    if std::path::Path::new(spec).is_file() {
        let contents = std::fs::read_to_string(spec)
            .map_err(|e| anyhow::anyhow!("failed to read identity file {}: {}", spec, e))?;
        let bytes: Vec<u8> = serde_json::from_str(&contents)
            .map_err(|e| anyhow::anyhow!("identity file {} is not a JSON byte array: {}", spec, e))?;
        let seed: [u8; 32] = bytes
            .get(..32)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| anyhow::anyhow!("identity file {} must hold at least 32 bytes", spec))?;
        return Ok(ed25519_dalek::SigningKey::from_bytes(&seed));
    }

    let bytes = hex::decode(spec).map_err(|e| {
        anyhow::anyhow!("--identity is neither a readable file nor valid hex: {}", e)
    })?;
    let seed: [u8; 32] = bytes
        .get(..32)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| anyhow::anyhow!("--identity hex must be at least 32 bytes"))?;
    Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
}

/// Produce a new block
async fn produce_block(state: &Arc<parking_lot::RwLock<ValidatorState>>) {
    let mut s = state.write();

    // Get current hash
    let block_hash = s.poh.current();
    let poh_hash = *block_hash.as_bytes();

    // Persist to the blockstore before acknowledging the block
    let slot = s.current_slot;
    let height = s.block_height;
    match s.ledger.append(slot, height, 1, 0, poh_hash) {
        Ok(record) => {
            tracing::info!(
                "📦 Produced block at slot {} | height {} | hash {}",
                record.slot,
                record.height,
                &hex::encode(record.block_hash)[..16],
            );
        }
        Err(err) => {
            tracing::error!("Ledger append failed at slot {}: {}", slot, err);
        }
    }

    // The tick loop owns the slot clock; producing a block only raises the
    // block height.
    s.block_height += 1;
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
    tracing::info!("│ Ledger Blocks:{:>28} │", s.ledger.len());
    tracing::info!("└─────────────────────────────────────────────┘");
    tracing::info!("");
}

/// Run performance benchmarks
async fn run_benchmarks(signing_key: &ed25519_dalek::SigningKey) {
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

    // Transaction execution benchmark — real signed transfers through the
    // native dispatcher (signature verification + fee deduction included)
    {
        let db = Arc::new(AccountsDB::new());
        let executor = Arc::new(Executor::new(db.clone()));

        let signer: Pubkey = signing_key.verifying_key().to_bytes();
        let recipient: Pubkey = [7u8; 32];
        // Must cover 10k × (5000 fee + 100 transfer)
        let acc = Account::new_system_account(signer, 1_000_000_000);
        db.store(signer, &acc);

        let start = std::time::Instant::now();
        for i in 0..10_000u64 {
            let ix = Instruction {
                program_id: [1u8; 32], // System program
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
                data: solana_program_executor::instruction::SystemInstruction::Transfer {
                    lamports: 100,
                }
                .to_data(),
            };
            let mut tx = Transaction::new(signer, vec![ix], [0u8; 32]);
            tx.account_keys.push(recipient);
            tx.recent_blockhash = {
                let mut h = [0u8; 32];
                h[..8].copy_from_slice(&i.to_le_bytes());
                h
            };
            tx.sign(signing_key);
            let result = executor.execute_transaction(&tx);
            assert!(result.success, "bench tx {} failed: {:?}", i, result.error);
        }
        let elapsed = start.elapsed();
        tracing::info!("Transaction execution (10k signed txs): {:.2?}", elapsed);
        tracing::info!(
            "Post-bench balances: signer={} recipient={}",
            db.load(&signer).map(|a| a.lamports).unwrap_or(0),
            db.load(&recipient).map(|a| a.lamports).unwrap_or(0),
        );
    }

    tracing::info!("");
    tracing::info!("Benchmarks complete!");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_identity_hex_seed() {
        let loaded = load_identity(&hex::encode([7u8; 32])).unwrap();
        let expected = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        assert_eq!(
            loaded.verifying_key().to_bytes(),
            expected.verifying_key().to_bytes()
        );
    }

    #[test]
    fn test_load_identity_keypair_file() {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let mut bytes = sk.to_bytes().to_vec();
        bytes.extend_from_slice(&sk.verifying_key().to_bytes());

        let path = std::env::temp_dir().join(format!("tn-identity-{}.json", std::process::id()));
        std::fs::write(&path, serde_json::to_string(&bytes).unwrap()).unwrap();

        let loaded = load_identity(path.to_str().unwrap()).unwrap();
        assert_eq!(
            loaded.verifying_key().to_bytes(),
            sk.verifying_key().to_bytes()
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_load_identity_empty_is_random_but_valid() {
        let a = load_identity("").unwrap();
        let b = load_identity("").unwrap();
        assert_ne!(a.verifying_key().to_bytes(), b.verifying_key().to_bytes());
    }
}
