# Stylus Architecture

## Why Stylus

Arbitrum Stylus allows smart contracts written in Rust (compiled to WASM) to run alongside EVM contracts on Arbitrum. The ouroborai contracts use Stylus for three reasons:

**Gas efficiency.** Computation-heavy operations like multi-protocol health factor scanning and multi-DEX route comparison cost 10-100x less gas in WASM than equivalent Solidity. The route-optimizer iterates over multiple DEXes and fee tiers, quotes each path, and returns the best route -- all in a single call. In Solidity, this would be prohibitively expensive.

**Rust safety.** Rust's ownership model, pattern matching, and `Result` types provide compile-time guarantees that prevent common smart contract bugs: reentrancy (no recursive mutable borrows), integer overflow (checked by default), and unhandled errors (must unwrap or propagate `Result`).

**EVM interoperability.** Stylus contracts can call and be called by Solidity contracts using standard ABI encoding. The `sol_interface!` macro generates type-safe Rust bindings for any Solidity interface (Aave, Uniswap, ERC20). From the caller's perspective, a Stylus contract is indistinguishable from a Solidity contract.

## Stylus SDK 0.10.0

The contracts use Stylus SDK 0.10.0, which introduced several breaking changes from earlier versions.

### Storage Declaration

`sol_storage!` uses Solidity type syntax, not Rust wrapper types:

```rust
sol_storage! {
    #[entrypoint]
    pub struct MyContract {
        address owner;                        // not StorageAddress
        uint256 count;                        // not StorageU256
        mapping(uint256 => address) items;    // not StorageMap<>
        bool active;                          // not StorageBool
    }
}
```

### VM Access

All environment access goes through `self.vm()`:

```rust
let sender = self.vm().msg_sender();      // not msg::sender()
let value = self.vm().msg_value();
let ts = self.vm().block_timestamp();
let addr = self.vm().contract_address();
```

### Events

Events are emitted via `self.vm().log()`:

```rust
self.vm().log(MyEvent { field: value });  // not evm::log(MyEvent { .. })
```

### External Calls

Cross-contract calls use the generated interface types:

```rust
let pool = IAavePool::new(pool_address);
let result = pool.get_user_account_data(self.vm(), Call::new(), account)?;
```

`Call::new()` is for view/pure calls. `Call::new_mutating(self)` is for state-changing calls.

## Contract Lifecycle

1. **Compilation.** `cargo build` produces a WASM binary per crate.
2. **Validation.** `cargo stylus check` verifies the WASM is Stylus-compatible.
3. **Deployment.** `cargo stylus deploy` uploads and activates the contract on Arbitrum.
4. **Initialization.** The deployer calls `initialize()` to set the owner and initial parameters. This pattern replaces Solidity constructors (WASM contracts have no constructor mechanism).
5. **Operation.** The contract is called via standard EVM transactions. ABI encoding/decoding happens transparently.

## no_std Environment

Stylus contracts run in a `no_std` WASM environment. There is no standard library, heap allocator is provided by the runtime. Key implications:

- Must declare `extern crate alloc` and `use alloc::vec::Vec`
- No `println!`, `std::collections`, or filesystem access
- `#[cfg_attr(not(any(feature = "export-abi", test)), no_main)]` gates the entry point
- Tests run with `std` enabled (the `test` feature flag)
