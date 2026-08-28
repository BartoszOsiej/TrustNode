# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in solana-validator, please report it responsibly:

1. **Do NOT** open a public GitHub issue
2. Use [GitHub Security Advisories](https://github.com/BartoszOsiej/solana-validator/security/advisories/new)
3. Include:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

## Response Timeline

- **Acknowledgment**: Within 48 hours
- **Assessment**: Within 1 consensus week
- **Fix**: Depends on severity, typically within 2 weeks

## Scope

The following are in scope:
- Consensus vulnerabilities (Tower BFT, fork choice)
- PoH chain verification bypasses
- Transaction processing exploits
- Account access control issues
- Gossip protocol manipulation

The following are out of scope:
- Denial of service (rate limiting is expected)
- Issues in upstream dependencies (report upstream)
- Academic/theoretical attacks without practical exploit

## Architecture Security Model

This validator implements a Solana-like architecture with:
- **Proof of History** — verifiable clock for transaction ordering
- **Tower BFT** — Byzantine fault tolerant consensus (2/3+ supermajority)
- **Turbine** — erasure-coded block propagation
- **Sealevel** — parallel transaction execution with conflict detection
- **Program Executor** — sandboxed instruction processing VM
