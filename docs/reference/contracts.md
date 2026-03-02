# Contracts Reference

## Agent Registry

On-chain registry of AI agent instances with capabilities, revenue share, and reputation.

### Storage Layout

```
sol_storage! {
    uint256 next_id;
    address governance;
    mapping(uint256 => address) owners;
    mapping(uint256 => uint256) capabilities;    // bitmask
    mapping(uint256 => uint256) revenue_share_bps;
    mapping(uint256 => uint256) reputation;
    mapping(uint256 => bool) active;
}
```

### Capability Flags

| Flag | Bit | Value |
|---|---|---|
| `CAP_TRADE` | 0 | 1 |
| `CAP_PERPS` | 1 | 2 |
| `CAP_LEND` | 2 | 4 |
| `CAP_YIELD` | 3 | 8 |
| `CAP_OPTIONS` | 4 | 16 |
| `CAP_TIMEBOOST` | 5 | 32 |
| `CAP_RWA` | 6 | 64 |

### Public Methods

| Method | Access | Description |
|---|---|---|
| `register(capabilities: u64, revenue_share_bps: u16) -> U256` | Anyone | Register a new agent. Returns agent ID. |
| `update(agent_id, capabilities, revenue_share_bps) -> bool` | Owner only | Update agent config. Returns false if not owner. |
| `deactivate(agent_id) -> bool` | Owner or governance | Soft-deactivate an agent. |
| `update_reputation(agent_id, new_score) -> bool` | Governance only | Set reputation score. |
| `get_agent(agent_id) -> (Address, U256, U256, U256, bool)` | View | Returns (owner, capabilities, revenue_share_bps, reputation, active). |
| `total_agents() -> U256` | View | Total registered agents (including deactivated). |
| `has_capability(agent_id, cap_flag) -> bool` | View | Check if agent has a specific capability bit. |
| `set_governance(new_governance) -> bool` | Governance or first caller | Set governance address. |

### Events

- `AgentRegistered(address indexed owner, uint256 indexed agentId, uint64 capabilities)`
- `AgentUpdated(uint256 indexed agentId, uint64 capabilities, uint16 revenueShareBps)`
- `AgentDeactivated(uint256 indexed agentId)`
- `ReputationUpdated(uint256 indexed agentId, uint256 newScore)`

---

## Liquidation Monitor

Multi-protocol health factor scanner for lending and perp positions.

### Storage Layout

```
sol_storage! {
    address owner;
    address[] tracked_accounts;
    uint256 risk_threshold;

    uint256 lending_count;
    mapping(uint256 => address) lending_pools;
    mapping(uint256 => uint256) lending_types;
    mapping(uint256 => bool) lending_active;

    uint256 perp_count;
    mapping(uint256 => address) perp_readers;
    mapping(uint256 => uint256) perp_types;
    mapping(uint256 => bool) perp_active;
}
```

### Protocol Type Constants

| Lending Type | Value |
|---|---|
| `LENDING_TYPE_AAVE_V3` | 0 |

### Public Methods

| Method | Access | Description |
|---|---|---|
| `initialize(risk_threshold)` | Once | Set owner and risk threshold. |
| `add_lending_protocol(pool_address, protocol_type) -> U256` | Owner | Register a lending protocol. Returns index. |
| `remove_lending_protocol(index)` | Owner | Soft-delete a lending protocol. |
| `lending_protocol_count() -> U256` | View | Total lending protocols (including removed). |
| `get_lending_protocol(index) -> (Address, U256, bool)` | View | Returns (address, type, active). |
| `add_perp_protocol(reader_address, protocol_type) -> U256` | Owner | Register a perp protocol. Returns index. |
| `remove_perp_protocol(index)` | Owner | Soft-delete a perp protocol. |
| `perp_protocol_count() -> U256` | View | Total perp protocols (including removed). |
| `get_health_factor(account) -> U256` | View | Lowest health factor across all active lending protocols. |
| `scan_accounts(accounts) -> Vec<(Address, U256)>` | View | Returns accounts below risk threshold. |
| `scan_tracked_accounts() -> Vec<(Address, U256)>` | View | Scan all tracked accounts. Emits `AccountAtRisk` events. |
| `add_account(account)` | Owner | Add account to tracked list. |
| `remove_account(account)` | Owner | Remove account from tracked list. |
| `set_threshold(new_threshold)` | Owner | Update risk threshold. |
| `tracked_count() -> U256` | View | Number of tracked accounts. |
| `threshold() -> U256` | View | Current risk threshold. |

### Events

- `AccountAtRisk(address indexed account, uint256 healthFactor, uint256 timestamp)`
- `AccountAdded(address indexed account)`
- `AccountRemoved(address indexed account)`
- `ThresholdUpdated(uint256 oldThreshold, uint256 newThreshold)`
- `LendingProtocolAdded(uint256 indexed index, address poolAddress, uint64 protocolType)`
- `LendingProtocolRemoved(uint256 indexed index, address poolAddress)`
- `PerpProtocolAdded(uint256 indexed index, address readerAddress, uint64 protocolType)`
- `PerpProtocolRemoved(uint256 indexed index, address readerAddress)`

---

## Route Optimizer

Multi-DEX route comparison with Uniswap V3 and AMM V2 dispatch.

### Storage Layout

```
sol_storage! {
    address owner;
    uint256 dex_count;
    mapping(uint256 => address) dex_addresses;
    mapping(uint256 => uint256) dex_types;
    mapping(uint256 => bool) dex_active;
    address[] routing_tokens;
}
```

### DEX Type Constants

| DEX Type | Value |
|---|---|
| `DEX_TYPE_UNIV3` | 0 |
| `DEX_TYPE_AMM_V2` | 1 |

### Public Methods

| Method | Access | Description |
|---|---|---|
| `initialize()` | Once | Set owner, register default routing tokens (WETH, USDC). |
| `add_dex(dex_address, dex_type) -> U256` | Owner | Register a DEX. Returns index. |
| `remove_dex(index)` | Owner | Soft-delete a DEX. |
| `dex_count() -> U256` | View | Total DEXes (including removed). |
| `get_dex(index) -> (Address, U256, bool)` | View | Returns (address, type, active). |
| `add_routing_token(token)` | Owner | Add intermediate routing token. |
| `remove_routing_token(token)` | Owner | Remove routing token. |
| `routing_token_count() -> U256` | View | Number of routing tokens. |
| `find_best_route(token_in, token_out, amount_in) -> (U256, Vec<Address>, Vec<u32>)` | Mutating | Returns (best output amount, token path, fee tiers). |

### Events

- `DexAdded(uint256 indexed index, address dexAddress, uint64 dexType)`
- `DexRemoved(uint256 indexed index, address dexAddress)`

---

## TimeBoost Vault

Manages ETH/USDC funds for TimeBoost express lane bidding and express lane access resale.

### Storage Layout

```
sol_storage! {
    address owner;
    address usdc;
    address bid_agent;
    uint256 resale_price_usdc;
    bool is_express_lane_controller;
    uint256 current_round;
    uint256 total_resale_earnings;
    uint256 total_bid_cost;
    mapping(address => bool) authorized_buyers;
}
```

### Public Methods

| Method | Access | Description |
|---|---|---|
| `initialize(usdc, bid_agent, resale_price_usdc)` | Once | Set owner, USDC address, bid agent, resale price. |
| `deposit_eth()` | Anyone (payable) | Deposit ETH for bidding capital. |
| `deposit_usdc(amount)` | Anyone | Deposit USDC (requires prior approval). |
| `record_round_win(round, bid_cost_wei)` | Owner or agent | Record a winning TimeBoost bid. |
| `end_round()` | Owner or agent | Reset express lane controller status. |
| `purchase_express_lane_access()` | Anyone | Pay resale price in USDC for express lane access. |
| `is_authorized_buyer(buyer) -> bool` | View | Check if address has express lane access. |
| `get_stats() -> (U256, U256, U256, U256, bool)` | View | Returns (ETH balance, USDC balance, total earnings, total bid cost, is controller). |
| `set_resale_price(new_price)` | Owner | Update the resale price. |
| `withdraw_eth(amount)` | Owner | Withdraw ETH. |
| `withdraw_usdc(amount)` | Owner | Withdraw USDC earnings. |

### Events

- `Deposited(address indexed from, uint256 amount, bool isEth)`
- `Withdrawn(address indexed to, uint256 amount, bool isEth)`
- `RoundWon(uint256 indexed round, uint256 bidCost)`
- `ResalePurchased(address indexed buyer, uint256 pricePaid, uint256 round)`
- `ResalePriceUpdated(uint256 oldPrice, uint256 newPrice)`
