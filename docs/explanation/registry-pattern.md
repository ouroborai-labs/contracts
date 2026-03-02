# Registry Pattern

All dynamic registries in the ouroborai contracts (lending protocols, perp protocols, DEXes) use the same indexed mapping pattern with soft-delete.

## The Pattern

```rust
sol_storage! {
    uint256 item_count;                      // monotonic counter
    mapping(uint256 => address) item_addrs;  // indexed by counter
    mapping(uint256 => uint256) item_types;  // protocol/DEX type
    mapping(uint256 => bool) item_active;    // soft-delete flag
}
```

### Add

```rust
pub fn add_item(&mut self, addr: Address, item_type: u64) -> U256 {
    let index = self.item_count.get();
    self.item_addrs.setter(index).set(addr);
    self.item_types.setter(index).set(U256::from(item_type));
    self.item_active.setter(index).set(true);
    self.item_count.set(index + U256::from(1));  // counter only increments
    index
}
```

### Remove (Soft-Delete)

```rust
pub fn remove_item(&mut self, index: U256) {
    self.item_active.setter(index).set(false);  // mark inactive
    // counter does NOT decrement
    // storage is NOT cleared
}
```

### Iterate (Skip Inactive)

```rust
for idx in 0..self.item_count.get().as_limbs()[0] {
    let index = U256::from(idx);
    if !self.item_active.get(index) {
        continue;  // skip removed items
    }
    let addr = self.item_addrs.get(index);
    // use addr...
}
```

## Why Not Arrays

Solidity-style dynamic arrays (`address[]`) support removal by swapping the last element and popping. This is used for the `tracked_accounts` list in the liquidation monitor. But arrays have a problem for registries: removal changes indices, which breaks external references.

The indexed mapping pattern preserves indices permanently. If a protocol is registered at index 3, it stays at index 3 forever, even after removal. This matters because:

- Off-chain systems may reference protocols by index
- Events reference indices (`LendingProtocolAdded(uint256 indexed index, ...)`)
- Re-adding a protocol gets a new index, preserving the audit trail

## Why Soft-Delete

True deletion in Ethereum storage (setting to zero) refunds gas but introduces complexity:

- Must handle index gaps in iteration
- No way to distinguish "never existed" from "deleted"
- Breaks referential integrity with emitted events

Soft-delete keeps the data readable for historical queries while excluding it from active iteration. The boolean check (`if !active { continue }`) is a single SLOAD.

## Trade-Offs

**Gas cost of iteration.** Scanning all items (including inactive) costs gas proportional to `item_count`, not the number of active items. If many protocols are added and removed, iteration gas increases. In practice, registry sizes are small (< 20 items) and the iteration is cheap in WASM.

**Storage cost.** Removed items remain in storage permanently. Each item consumes 3 storage slots (address + type + active flag). At ~20,000 gas per slot on Ethereum L1, this would be significant. On Arbitrum L2, storage costs are orders of magnitude lower.

**No compaction.** The counter never decrements. After 100 adds and 99 removes, `item_count` is still 100, and iteration loops 100 times. This is acceptable for the expected registry sizes.

## Contracts Using This Pattern

| Contract | Registry | Type Constants |
|---|---|---|
| `liquidation-monitor` | Lending protocols | `LENDING_TYPE_AAVE_V3 = 0` |
| `liquidation-monitor` | Perp protocols | (future) |
| `route-optimizer` | DEX quoters/routers | `DEX_TYPE_UNIV3 = 0`, `DEX_TYPE_AMM_V2 = 1` |
