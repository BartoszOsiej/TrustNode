# Contributing to solana-validator

Thank you for your interest in contributing to solana-validator!

## Development Setup

```bash
git clone https://github.com/BartoszOsiej/solana-validator
cd solana-validator
cargo build
cargo test
```

## Requirements

- Rust 1.75+
- No external system dependencies

## Project Structure

```
solana-validator/
├── crates/
│   ├── poh/            # Proof of History — hash chain + verifier
│   ├── accounts/       # Account store + Merkle state root
│   ├── tx-processor/   # Sealevel-like parallel transaction scheduler
│   ├── consensus/      # Tower BFT — fork choice + leader schedule
│   ├── turbine/        # Block propagation — erasure coding + neighborhoods
│   ├── gossip/         # CRDS gossip protocol
│   ├── rpc/            # JSON-RPC server
│   └── program-executor/ # System + Token program VM
└── validator/          # Binary entry point
```

## Code Style

- `cargo fmt --all` must pass
- `cargo clippy --workspace -- -D warnings` must pass
- All tests must pass: `cargo test --workspace`

## Pull Requests

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## Security

If you find a security vulnerability, please report it privately via GitHub Security Advisories.
