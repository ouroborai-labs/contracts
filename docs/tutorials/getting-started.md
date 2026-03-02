# Getting Started

Build and test the ouroborai Stylus smart contracts locally.

## Prerequisites

- [Rust](https://rustup.rs) (stable toolchain)
- `wasm32-unknown-unknown` target for Stylus compilation
- Cargo (installed with Rust)

```bash
rustup install stable
rustup target add wasm32-unknown-unknown
```

## Clone and Build

```bash
git clone <repo-url> ouro-contracts
cd ouro-contracts
cargo build
```

The workspace contains four contracts:

| Crate | Purpose |
|---|---|
| `agent-registry` | On-chain agent registry with capabilities bitmask |
| `liquidation-monitor` | Multi-protocol health factor scanner |
| `route-optimizer` | Multi-DEX route comparison |
| `timeboost-vault` | TimeBoost express lane bid funding and resale |

## Run Tests

All contracts use `stylus-test` for unit testing with `TestVM`:

```bash
cargo test --features stylus-test
```

This runs 115 tests across all four contracts (7 + 43 + 38 + 27).

To run tests for a single contract:

```bash
cargo test --features stylus-test -p agent-registry
cargo test --features stylus-test -p liquidation-monitor
cargo test --features stylus-test -p route-optimizer
cargo test --features stylus-test -p timeboost-vault
```

## Export ABI

Generate Solidity ABI for any contract:

```bash
cargo build --features export-abi -p agent-registry
```

## Project Structure

```
ouro-contracts/
  Cargo.toml                  # Workspace root
  agent-registry/
    Cargo.toml
    src/lib.rs
  liquidation-monitor/
    Cargo.toml
    src/lib.rs
  route-optimizer/
    Cargo.toml
    src/lib.rs
  timeboost-vault/
    Cargo.toml
    src/lib.rs
```

Each crate produces a `cdylib` (WASM) for deployment and a `lib` for testing.

## Dependencies

All crates share workspace dependencies defined in the root `Cargo.toml`:

- `stylus-sdk` 0.10.0 -- Stylus contract framework
- `alloy-primitives` 1.0 -- `Address`, `U256`, `B256`
- `alloy-sol-types` 1.0 -- `sol!`, `sol_storage!`, `sol_interface!` macros
- `stylus-test` 0.10.0 -- `TestVM` for unit testing (dev-dependency)
