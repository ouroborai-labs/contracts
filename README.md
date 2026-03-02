# ouroborai / contracts

Stylus smart contracts for Arbitrum. Written in Rust using the Stylus SDK for gas-efficient on-chain execution.

## Contracts

| Contract | Tests | Description |
|----------|-------|-------------|
| `agent-registry` | 7 | On-chain agent registry with capabilities bitmask and reputation |
| `liquidation-monitor` | 43 | Dynamic protocol registry, multi-protocol health scanning |
| `route-optimizer` | 38 | Dynamic DEX registry, multi-DEX route comparison |
| `timeboost-vault` | 27 | TimeBoost bid funding and resale payments |

## Quick Start

```bash
cargo test --features stylus-test
```

## Deploy

See [docs/tutorials/deploy-to-testnet.md](docs/tutorials/deploy-to-testnet.md) for Arbitrum Sepolia deployment.

## Docs

See the [docs/](docs/) directory for tutorials, how-to guides, reference, and explanations.
