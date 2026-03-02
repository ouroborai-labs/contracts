# Add a Protocol to a Registry

Both the `liquidation-monitor` and `route-optimizer` contracts use dynamic registries for protocol integration. Adding a new protocol is a two-step process: register it on-chain, then add dispatch logic for its interface.

## Liquidation Monitor: Add a Lending Protocol

### 1. Define the Protocol Interface

In `liquidation-monitor/src/lib.rs`, add the new protocol's Solidity interface inside `sol_interface!`:

```rust
sol_interface! {
    interface IAavePool {
        // existing...
    }

    interface ICompoundV3 {
        function getAccountHealth(address account) external view returns (uint256);
    }
}
```

### 2. Add a Type Constant

```rust
const LENDING_TYPE_AAVE_V3: u64 = 0;
const LENDING_TYPE_COMPOUND_V3: u64 = 1;  // new
```

### 3. Add Dispatch Logic

In `get_health_factor`, add a match arm for the new type:

```rust
let hf = match protocol_type {
    LENDING_TYPE_AAVE_V3 => {
        let pool = IAavePool::new(pool_addr);
        let (_c, _d, _b, _lt, _ltv, health_factor) =
            pool.get_user_account_data(self.vm(), Call::new(), account)?;
        health_factor
    }
    LENDING_TYPE_COMPOUND_V3 => {
        let compound = ICompoundV3::new(pool_addr);
        compound.get_account_health(self.vm(), Call::new(), account)?
    }
    _ => continue,
};
```

### 4. Register On-Chain

After deploying, register the new protocol via the owner:

```bash
cast send <monitor-address> \
  "add_lending_protocol(address,uint64)" \
  <compound-pool-address> 1 \
  --rpc-url <rpc-url> --private-key <key>
```

The second argument (`1`) is `LENDING_TYPE_COMPOUND_V3`.

### 5. Add Tests

Use `mock_static_call` to mock the new protocol's external call:

```rust
#[test]
fn test_compound_health_factor() {
    let (vm, mut contract) = setup();
    vm.set_sender(OWNER);

    let compound_addr = address!("0000000000000000000000000000000000000ccc");
    contract.add_lending_protocol(compound_addr, 1).unwrap();

    let account = address!("0000000000000000000000000000000000000020");
    let calldata = getAccountHealthCall { account }.abi_encode();
    let return_data = U256::from(1_500_000_000_000_000_000u128).to_be_bytes_vec();
    vm.mock_static_call(compound_addr, calldata, Ok(return_data));

    let hf = contract.get_health_factor(account).unwrap();
    assert_eq!(hf, U256::from(1_500_000_000_000_000_000u128));
}
```

## Route Optimizer: Add a DEX

### 1. Define the DEX Interface

In `route-optimizer/src/lib.rs`:

```rust
sol_interface! {
    interface IUniswapV3Quoter { /* existing */ }
    interface IAmmRouter { /* existing */ }

    interface ICurvePool {
        function get_dy(int128 i, int128 j, uint256 dx) external view returns (uint256);
    }
}
```

### 2. Add a Type Constant

```rust
const DEX_TYPE_UNIV3: u64 = 0;
const DEX_TYPE_AMM_V2: u64 = 1;
const DEX_TYPE_CURVE: u64 = 2;  // new
```

### 3. Add Quote Logic

In `find_best_route`, add a match arm in the DEX iteration loop:

```rust
DEX_TYPE_CURVE => {
    let pool = ICurvePool::new(dex_addr);
    let out = pool.get_dy(self.vm(), Call::new(), 0, 1, amount_in)?;
    if out > best_out {
        best_out = out;
        best_tokens = vec![token_in, token_out];
        best_fees = vec![0];
    }
}
```

### 4. Register On-Chain

```bash
cast send <optimizer-address> \
  "add_dex(address,uint64)" \
  <curve-pool-address> 2 \
  --rpc-url <rpc-url> --private-key <key>
```

## Removing a Protocol

Both registries use soft-delete. The protocol stays in storage but is skipped during iteration:

```bash
# Liquidation monitor
cast send <address> "remove_lending_protocol(uint256)" <index>

# Route optimizer
cast send <address> "remove_dex(uint256)" <index>
```

The counter does not decrement. See [Registry Pattern](../explanation/registry-pattern.md) for details.
