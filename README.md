# programs

On-chain Solana programs for the [Gridlock](https://grid-lock.tech) decentralized AI inference marketplace. Written in **Rust** with the [Anchor](https://www.anchor-lang.com) framework, these programs enforce worker registration, job escrow, latency SLA receipts, penalties, fee distribution, and governance over the **LOCK** token (Token-2022).

**Production API (off-chain router):** [https://api.grid-lock.tech](https://api.grid-lock.tech)

## What it is

The Gridlock **programs** repo contains six Anchor programs that coordinate trust-minimized settlement between customers, GPU workers, and stakers. The [router](https://github.com/Gridlockcompute/router) submits transactions on behalf of the network when `SOLANA_SETTLEMENT_ENABLED=true`.

```
Customer                    Router (off-chain)              Worker
   │                              │                          │
   │  open_job (LOCK escrow)      │                          │
   ├──────────────────────────────►                          │
   │                              │  assign_worker           │
   │                              ├─────────────────────────►│
   │                              │  commit_receipt (SLA)    │
   │                              │◄─────────────────────────┤
   │                              │  settle_or_penalize      │
   │                              │  distribute_fees         │
   ▼                              ▼                          ▼
 JobScheduler              SLARegistry / SLAEnforcer    ProviderRegistry
                                    │
                                    ▼
                              FeeCollector → stakers / worker / burn / treasury
```

When on-chain settlement is disabled, the router performs equivalent logic off-chain using Supabase — the programs remain the canonical settlement layer for production deployment.

## Programs

| Program | ID (devnet) | Purpose |
|---------|-------------|---------|
| **ProviderRegistry** | `GvCMygAV4RNYVgPybMmgEb36AkSKEBQJJw45WfUfSfmu` | Worker registration, heartbeats, reliability scores, SLA tier staking |
| **JobScheduler** | `14ZQ7ubKgrWJRhcuzjmUj733fStgwUpERWXMj6pKuYcT` | Job escrow (LOCK), worker assignment, job lifecycle |
| **SLARegistry** | `5me7JG25p4NH1XCYtxWn9bU5sij8Xos1We5g47TbRxxM` | Latency receipts (TTFT/TPOT), watcher verification, challenge window |
| **SLAEnforcer** | `3H3yLvY7m7TaGkMSvvkvG9NQT5nDhVLNrZTfywiBaoLJ` | SLA settlement — release escrow or penalize worker stake |
| **FeeCollector** | `6GoaeiUQC8DaLXSDjd6CPACZZ1rM4xL4VCzxu1iC5xoU` | Fee split (60% stakers / 20% worker / 10% burn / 10% treasury), passive staking vaults |
| **Governance** | `8d4UT2HPvAkmX4JR367nyNVWNqawiqGjN1bBbwtvvFUB` | On-chain proposals, voting, timelocked execution |

Program IDs are declared in [`Anchor.toml`](./Anchor.toml) and mirrored in [`scripts/program-ids.ts`](./scripts/program-ids.ts).

## Features

- **LOCK escrow** — customers prepay job fees into per-job PDAs
- **Four SLA tiers** — `realtime`, `standard`, `batch`, `confidential` with tier-specific penalties
- **Latency receipts** — on-chain TTFT/TPOT commitments with optional watcher dispute sampling
- **Worker collateral** — SLA misses deduct from worker staked LOCK
- **Revenue split** — 60/20/10/10 to stakers, serving worker, burn, and treasury
- **Passive staking** — per-wallet staker vaults under FeeCollector
- **Governance** — proposal creation, quorum voting, timelocked CPI execution

## Prerequisites

- **Rust** (stable, via [rustup](https://rustup.rs))
- **Solana CLI** 3.x ([install guide](https://docs.anza.xyz/cli/install))
- **Anchor CLI** 1.0.2 (see `Anchor.toml` toolchain pin)
- **Node.js** 20+ (for TypeScript deployment scripts)
- A funded Solana keypair on devnet (or target cluster)

Install Anchor:

```bash
cargo install --git https://github.com/coral-xyz/anchor avm --locked
avm install 1.0.2
avm use 1.0.2
```

## Installation

```bash
git clone https://github.com/Gridlockcompute/programs.git
cd programs
npm install   # optional — for TS scripts
```

Configure your wallet and RPC in `Anchor.toml` (default cluster: **devnet**).

## Build and deploy

```bash
# Build all programs
anchor build

# Deploy to devnet
anchor deploy --provider.cluster devnet

# Deploy a single program
anchor deploy --program-name provider_registry --provider.cluster devnet
```

Verify program IDs after deploy:

```bash
anchor keys list
```

## Deployment scripts

TypeScript helpers for devnet bootstrap (LOCK mint, fee vaults, settlement accounts):

```bash
# Create LOCK Token-2022 mint (writes lock-mint.json)
npm run create-lock-mint

# Initialize fee vault PDAs
npm run init-fee-vaults

# Initialize settlement-related accounts
npm run init-settlement-accounts
```

Scripts read/write `lock-mint.json` and use `@solana/web3.js`. Configure RPC and payer keypair via standard Solana CLI (`~/.config/solana/id.json`).

## Configuration

Key files:

| File | Purpose |
|------|---------|
| `Anchor.toml` | Cluster, program IDs, provider wallet, toolchain versions |
| `Cargo.toml` | Rust workspace members |
| `lock-mint.json` | Devnet LOCK mint metadata |
| `docker-compose.yml` | Local Redis + router + web stack (references sibling repos) |

### Anchor.toml clusters

```toml
[programs.devnet]
provider_registry = "GvCMygAV4RNYVgPybMmgEb36AkSKEBQJJw45WfUfSfmu"
job_scheduler     = "14ZQ7ubKgrWJRhcuzjmUj733fStgwUpERWXMj6pKuYcT"
sla_registry      = "5me7JG25p4NH1XCYtxWn9bU5sij8Xos1We5g47TbRxxM"
sla_enforcer      = "3H3yLvY7m7TaGkMSvvkvG9NQT5nDhVLNrZTfywiBaoLJ"
fee_collector     = "6GoaeiUQC8DaLXSDjd6CPACZZ1rM4xL4VCzxu1iC5xoU"
governance        = "8d4UT2HPvAkmX4JR367nyNVWNqawiqGjN1bBbwtvvFUB"
```

## Program layout

```
programs/
├── programs/
│   ├── provider-registry/src/lib.rs
│   ├── job-scheduler/src/lib.rs
│   ├── sla-registry/src/lib.rs
│   ├── sla-enforcer/src/lib.rs
│   ├── fee-collector/src/lib.rs
│   └── governance/src/lib.rs
├── scripts/
│   ├── program-ids.ts
│   ├── create-lock-mint.ts
│   ├── init-fee-vaults.ts
│   └── init-settlement-accounts.ts
├── Anchor.toml
├── Cargo.toml
└── lock-mint.json
```

## Local development stack

`docker-compose.yml` starts Redis and can wire a local router instance. For full local dev:

1. Deploy programs to devnet (or localnet)
2. Configure [router](https://github.com/Gridlockcompute/router) `.env` with program IDs and vault addresses from `lock-mint.json`
3. Start workers via [worker-desktop](https://github.com/Gridlockcompute/worker-desktop) or [worker-cli](https://github.com/Gridlockcompute/worker-cli)

```bash
# From repo root — requires sibling router/web repos
docker compose up redis
```

## Related repos

| Repo | Role |
|------|------|
| [router](https://github.com/Gridlockcompute/router) | Off-chain Hono API — invokes Anchor instructions, WebSocket hub |
| [worker-desktop](https://github.com/Gridlockcompute/worker-desktop) | Electron GPU worker |
| [worker-cli](https://github.com/Gridlockcompute/worker-cli) | Headless CLI worker |

**Website:** [https://grid-lock.tech](https://grid-lock.tech) · **Explorer:** [https://grid-lock.tech/explorer](https://grid-lock.tech/explorer)

## License

Not specified in the repository.
