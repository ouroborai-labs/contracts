# Test Patterns Reference

All contracts use `stylus-test` 0.10.0 with the `TestVM` harness. Tests run with:

```bash
cargo test --features stylus-test
```

## TestVM Setup

Every test module follows the same pattern:

```rust
use stylus_test::TestVM;
use alloy_primitives::{address, Address, U256};

const OWNER: Address = address!("0000000000000000000000000000000000000001");

fn setup() -> (TestVM, MyContract) {
    let vm = TestVM::new();
    vm.set_sender(OWNER);
    let contract = MyContract::from(&vm);
    (vm, contract)
}
```

`TestVM::new()` creates a fresh VM. `MyContract::from(&vm)` instantiates the contract bound to that VM. All storage starts zeroed.

## Setting Transaction Context

```rust
vm.set_sender(ALICE);           // msg.sender
vm.set_value(U256::from(1e18)); // msg.value (for payable)
vm.set_contract_address(addr);  // address(this)
vm.set_balance(addr, amount);   // set ETH balance
vm.set_block_timestamp(ts);     // block.timestamp
```

Always call `vm.set_sender()` before any contract method that checks `self.vm().msg_sender()`.

## Mocking External Calls

Stylus contracts make cross-contract calls via `sol_interface!`. In tests, mock these with `mock_static_call` (for view/pure) or `mock_call` (for state-changing):

### mock_static_call

```rust
use alloy_sol_types::{SolCall, SolType, sol, sol_data};

sol! {
    function getUserAccountData(address user) external view returns (
        uint256 totalCollateralBase,
        uint256 totalDebtBase,
        uint256 availableBorrowsBase,
        uint256 currentLiquidationThreshold,
        uint256 ltv,
        uint256 healthFactor
    );
}

let calldata = getUserAccountDataCall { user: account }.abi_encode();

type AaveReturn = (
    sol_data::Uint<256>,
    sol_data::Uint<256>,
    sol_data::Uint<256>,
    sol_data::Uint<256>,
    sol_data::Uint<256>,
    sol_data::Uint<256>,
);

let return_data = <AaveReturn as SolType>::abi_encode_params(&(
    U256::from(1_000_000),
    U256::from(500_000),
    U256::from(200_000),
    U256::from(8000),
    U256::from(7500),
    health_factor,
));

vm.mock_static_call(POOL_ADDRESS, calldata, Ok(return_data));
```

### mock_call (Mutating)

For ERC20 calls like `transferFrom`:

```rust
sol! {
    function transferFrom(address from, address to, uint256 amount)
        external returns (bool);
}

let calldata = transferFromCall { from, to, amount }.abi_encode();
let return_data = U256::from(1).to_be_bytes_vec(); // true

vm.mock_call(TOKEN_ADDRESS, calldata, U256::ZERO, Ok(return_data));
```

The third argument to `mock_call` is the ETH value sent with the call.

## mock_static_call Ordering

`stylus-test` 0.10.0 has a known behavior: when multiple `mock_static_call` registrations match different calldata on the same target address, the **last registered mock's return data** is used for all calls to that address. To work around this:

- Register "losing" mocks first
- Register the mock whose return data you need last

```rust
// POOL returns high HF, POOL_2 returns low HF
// Register POOL mock first (its return data will be overwritten)
mock_health_factor_on_pool(&vm, POOL, account, hf_high);
// Register POOL_2 mock last (this return data wins)
mock_health_factor_on_pool(&vm, POOL_2, account, hf_low);
```

## Verifying Events

Use `vm.get_emitted_logs()` to inspect emitted events:

```rust
let logs = vm.get_emitted_logs();
let (topics, data) = &logs[logs.len() - 1];

// topics[0] is the event selector (keccak256 of signature)
let selector = MyEvent::SIGNATURE_HASH;
assert_eq!(topics[0], selector);

// Indexed params are in topics[1..], non-indexed are ABI-encoded in data
let indexed_addr = B256::from(some_address.into_word());
assert_eq!(topics[1], indexed_addr);

// Decode non-indexed data
type EventData = (sol_data::Uint<256>, sol_data::Uint<256>);
let decoded = <EventData as SolType>::abi_decode_params(data).unwrap();
```

## sol_storage! in Tests

The `sol_storage!` macro uses Solidity-style types. In test code, import primitives from `alloy_primitives`, not from `stylus_sdk`:

```rust
// Correct
use alloy_primitives::{address, Address, U256};

// Incorrect
use stylus_sdk::alloy_primitives::{Address, U256};
```

## Access Control Testing Pattern

Test both positive and negative cases for owner/governance guards:

```rust
#[test]
fn owner_can_add() {
    let (vm, mut contract) = setup();
    vm.set_sender(OWNER);
    assert!(contract.add_account(addr).is_ok());
}

#[test]
fn stranger_cannot_add() {
    let (vm, mut contract) = setup();
    vm.set_sender(STRANGER);
    let err = contract.add_account(addr).unwrap_err();
    assert_eq!(err, b"not owner".to_vec());
}
```

## Required Imports

```rust
extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;
```

The `no_std` environment requires explicit `alloc` imports. The `sol_storage!` macro internally uses `vec!`, so `use alloc::vec;` is mandatory even if your code does not call `vec!` directly.
