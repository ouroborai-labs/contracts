# Deploy to Arbitrum Sepolia

Deploy any ouroborai Stylus contract to the Arbitrum Sepolia testnet.

## Prerequisites

- Rust with `wasm32-unknown-unknown` target (see [Getting Started](./getting-started.md))
- [cargo-stylus](https://github.com/OffchainLabs/cargo-stylus) CLI
- An Arbitrum Sepolia RPC endpoint
- A funded wallet (Sepolia ETH)

## Install cargo-stylus

```bash
cargo install cargo-stylus
```

## Get Testnet ETH

1. Get Sepolia ETH from a faucet (e.g., [sepoliafaucet.com](https://sepoliafaucet.com))
2. Bridge to Arbitrum Sepolia via the [Arbitrum Bridge](https://bridge.arbitrum.io)

## Check Contract Validity

Before deploying, verify the contract compiles to valid Stylus WASM:

```bash
cargo stylus check \
  --endpoint https://sepolia-rollup.arbitrum.io/rpc \
  -p agent-registry
```

## Deploy

```bash
cargo stylus deploy \
  --endpoint https://sepolia-rollup.arbitrum.io/rpc \
  --private-key <your-private-key> \
  -p agent-registry
```

The CLI outputs the deployed contract address. Repeat for each contract you want to deploy.

## Initialize After Deployment

Most contracts require an `initialize` call after deployment. Use `cast` from [Foundry](https://getfoundry.sh):

### Agent Registry

No initialization needed. The governance address defaults to `Address::ZERO` and can be set by the first caller via `set_governance`.

### Liquidation Monitor

```bash
cast send <contract-address> \
  "initialize(uint256)" \
  1100000000000000000 \
  --rpc-url https://sepolia-rollup.arbitrum.io/rpc \
  --private-key <your-private-key>
```

The parameter is the risk threshold (1.1e18 = health factor 1.1).

### Route Optimizer

```bash
cast send <contract-address> \
  "initialize()" \
  --rpc-url https://sepolia-rollup.arbitrum.io/rpc \
  --private-key <your-private-key>
```

This sets the caller as owner and registers default routing tokens (WETH, USDC).

### TimeBoost Vault

```bash
cast send <contract-address> \
  "initialize(address,address,uint256)" \
  <usdc-address> <bid-agent-address> 5000000 \
  --rpc-url https://sepolia-rollup.arbitrum.io/rpc \
  --private-key <your-private-key>
```

Parameters: USDC token address, bid agent address, resale price in USDC (5e6 = 5 USDC).

## Verify Deployment

Query a view function to confirm the contract is live:

```bash
cast call <contract-address> \
  "total_agents()(uint256)" \
  --rpc-url https://sepolia-rollup.arbitrum.io/rpc
```

## Deploy All Contracts

Deploy in any order. The contracts are independent and do not reference each other on-chain.
