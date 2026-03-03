//! TimeBoost Vault — Stylus (Rust) contract for Arbitrum One
//!
//! Manages ETH/USDC funds for TimeBoost express lane bidding
//! and handles payments for express lane resale via x402.
//!
//! Functionality:
//! - Agent deposits ETH/USDC for bidding capital
//! - Contract auto-bids in TimeBoost rounds when signaled
//! - Authorized buyers pay USDC to receive express lane tx rights
//! - Earnings accumulate and can be withdrawn by owner
//!
//! Revenue model: bid for express lane at cost X, resell access at X+Y
//!
//! Note (TV-8): Multiple purchases per buyer per round are intentional.
//! This allows a single buyer to purchase additional capacity or
//! re-purchase for different transactions within the same round.

#![cfg_attr(not(any(feature = "export-abi", test)), no_main)]
#![cfg_attr(not(any(feature = "export-abi", test)), no_std)]
#![allow(unexpected_cfgs)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use alloy_sol_types::sol;
use stylus_sdk::{
    alloy_primitives::{Address, U256},
    call::transfer::transfer_eth,
    prelude::*,
};

// ─── USDC interface ───────────────────────────────────────────────────────────

sol_interface! {
    interface IERC20 {
        function transfer(address to, uint256 amount) external returns (bool);
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
        function approve(address spender, uint256 amount) external returns (bool);
    }
}

// ─── Contract storage ─────────────────────────────────────────────────────────

sol_storage! {
    #[entrypoint]
    pub struct TimeBoostVault {
        /// Vault owner (can withdraw, configure)
        address owner;
        /// Pending owner for two-step transfer (TV-7)
        address pending_owner;
        /// USDC token address on Arbitrum One
        address usdc;
        /// Bidding agent address (authorized to trigger bids)
        address bid_agent;
        /// Resale price per express lane slot (USDC, 6 decimals)
        uint256 resale_price_usdc;
        /// Current round winning status
        bool is_express_lane_controller;
        /// Current round number
        uint256 current_round;
        /// Total USDC earned from resales
        uint256 total_resale_earnings;
        /// Total ETH spent on bids
        uint256 total_bid_cost;
        /// Round-scoped authorized buyers (TV-1): round => buyer => bool
        mapping(uint256 => mapping(address => bool)) round_authorized_buyers;
        /// Reentrancy guard (TV-2)
        bool locked;
        /// Pausable (CC-2)
        bool paused;
    }
}

// ─── Events ───────────────────────────────────────────────────────────────────

sol! {
    event Deposited(address indexed from, uint256 amount, bool isEth);
    event Withdrawn(address indexed to, uint256 amount, bool isEth);
    event RoundWon(uint256 indexed round, uint256 bidCost);
    event ResalePurchased(address indexed buyer, uint256 pricePaid, uint256 round);
    event ResalePriceUpdated(uint256 oldPrice, uint256 newPrice);
    event OwnershipTransferStarted(address indexed currentOwner, address indexed pendingOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event Paused(address indexed by);
    event Unpaused(address indexed by);
}

// ─── Private helpers (separate impl block for &mut params) ────────────────────

impl TimeBoostVault {
    fn only_owner(&self) -> Result<(), Vec<u8>> {
        if self.vm().msg_sender() != self.owner.get() {
            return Err(b"not owner".to_vec());
        }
        Ok(())
    }

    fn only_agent(&self) -> Result<(), Vec<u8>> {
        let sender = self.vm().msg_sender();
        if sender != self.owner.get() && sender != self.bid_agent.get() {
            return Err(b"not authorized".to_vec());
        }
        Ok(())
    }

    fn when_not_paused(&self) -> Result<(), Vec<u8>> {
        if self.paused.get() {
            return Err(b"paused".to_vec());
        }
        Ok(())
    }

    fn reentrancy_lock(&mut self) -> Result<(), Vec<u8>> {
        if self.locked.get() {
            return Err(b"reentrancy".to_vec());
        }
        self.locked.set(true);
        Ok(())
    }

    fn reentrancy_unlock(&mut self) {
        self.locked.set(false);
    }
}

// ─── Implementation ───────────────────────────────────────────────────────────

#[public]
impl TimeBoostVault {
    /// Initialize the vault.
    pub fn initialize(
        &mut self,
        usdc: Address,
        bid_agent: Address,
        resale_price_usdc: U256,
    ) -> Result<(), Vec<u8>> {
        if self.owner.get() != Address::ZERO {
            return Err(b"already initialized".to_vec());
        }
        self.owner.set(self.vm().msg_sender());
        self.usdc.set(usdc);
        self.bid_agent.set(bid_agent);
        self.resale_price_usdc.set(resale_price_usdc);
        Ok(())
    }

    /// Deposit ETH for bidding capital.
    #[payable]
    pub fn deposit_eth(&mut self) -> Result<(), Vec<u8>> {
        self.when_not_paused()?;
        self.vm().log(Deposited {
            from: self.vm().msg_sender(),
            amount: self.vm().msg_value(),
            isEth: true,
        });
        Ok(())
    }

    /// Deposit USDC for bid payments.
    pub fn deposit_usdc(&mut self, amount: U256) -> Result<(), Vec<u8>> {
        self.when_not_paused()?;
        self.reentrancy_lock()?;
        let usdc = IERC20::new(self.usdc.get());
        let sender = self.vm().msg_sender();
        let contract_addr = self.vm().contract_address();
        let vm = self.vm().clone();
        usdc.transfer_from(&vm, Call::new_mutating(self), sender, contract_addr, amount)?;
        self.reentrancy_unlock();
        self.vm().log(Deposited {
            from: sender,
            amount,
            isEth: false,
        });
        Ok(())
    }

    /// Called by bid_agent when a round is won.
    /// Records the win and sets is_express_lane_controller = true.
    pub fn record_round_win(&mut self, round: U256, bid_cost_wei: U256) -> Result<(), Vec<u8>> {
        self.only_agent()?;
        self.is_express_lane_controller.set(true);
        self.current_round.set(round);
        let prev_cost = self.total_bid_cost.get();
        self.total_bid_cost.set(prev_cost + bid_cost_wei);
        self.vm().log(RoundWon {
            round,
            bidCost: bid_cost_wei,
        });
        Ok(())
    }

    /// Called at end of round to reset controller status.
    /// No need to clear authorized buyers — they are round-scoped via
    /// `round_authorized_buyers[round][buyer]`, so a new round number
    /// naturally starts with a clean mapping space.
    pub fn end_round(&mut self) -> Result<(), Vec<u8>> {
        self.only_agent()?;
        self.is_express_lane_controller.set(false);
        Ok(())
    }

    /// Purchase express lane access for the current round.
    /// Buyer pays resale_price_usdc USDC, gets added to round_authorized_buyers.
    ///
    /// TV-8 note: Multiple purchases per buyer per round are intentional —
    /// buyers may need additional capacity or purchase for different txs.
    pub fn purchase_express_lane_access(&mut self) -> Result<(), Vec<u8>> {
        self.when_not_paused()?;
        if !self.is_express_lane_controller.get() {
            return Err(b"not controlling express lane".to_vec());
        }
        self.reentrancy_lock()?;

        let price = self.resale_price_usdc.get();
        let sender = self.vm().msg_sender();
        let round = self.current_round.get();

        // TV-2: Checks-Effects-Interactions — update state BEFORE external call
        self.round_authorized_buyers
            .setter(round)
            .setter(sender)
            .set(true);
        let prev_earnings = self.total_resale_earnings.get();
        self.total_resale_earnings.set(prev_earnings + price);

        // External call (interaction) happens AFTER state updates
        let usdc = IERC20::new(self.usdc.get());
        let contract_addr = self.vm().contract_address();
        let vm = self.vm().clone();
        let result =
            usdc.transfer_from(&vm, Call::new_mutating(self), sender, contract_addr, price);

        // If the external call fails, revert state changes
        if result.is_err() {
            self.round_authorized_buyers
                .setter(round)
                .setter(sender)
                .set(false);
            self.total_resale_earnings.set(prev_earnings);
            self.reentrancy_unlock();
            return Err(b"usdc transfer failed".to_vec());
        }

        self.reentrancy_unlock();

        self.vm().log(ResalePurchased {
            buyer: sender,
            pricePaid: price,
            round,
        });

        Ok(())
    }

    /// Check if an address is authorized to use the express lane in the current round.
    pub fn is_authorized_buyer(&self, buyer: Address) -> bool {
        let round = self.current_round.get();
        self.round_authorized_buyers.getter(round).get(buyer)
    }

    /// Returns vault balances and stats.
    pub fn get_stats(&self) -> Result<(U256, U256, U256, U256, bool), Vec<u8>> {
        let usdc = IERC20::new(self.usdc.get());
        let contract_addr = self.vm().contract_address();
        let usdc_balance = usdc.balance_of(self.vm(), Call::new(), contract_addr)?;
        let eth_balance = self.vm().balance(contract_addr);

        Ok((
            eth_balance,
            usdc_balance,
            self.total_resale_earnings.get(),
            self.total_bid_cost.get(),
            self.is_express_lane_controller.get(),
        ))
    }

    /// Update resale price. Owner only.
    pub fn set_resale_price(&mut self, new_price: U256) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if new_price == U256::ZERO {
            return Err(b"price cannot be zero".to_vec());
        }
        let old = self.resale_price_usdc.get();
        self.resale_price_usdc.set(new_price);
        self.vm().log(ResalePriceUpdated {
            oldPrice: old,
            newPrice: new_price,
        });
        Ok(())
    }

    /// Withdraw ETH. Owner only.
    pub fn withdraw_eth(&mut self, amount: U256) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        let recipient = self.vm().msg_sender();
        transfer_eth(self.vm(), recipient, amount)?;
        self.vm().log(Withdrawn {
            to: recipient,
            amount,
            isEth: true,
        });
        Ok(())
    }

    /// Withdraw USDC earnings. Owner only.
    pub fn withdraw_usdc(&mut self, amount: U256) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        self.reentrancy_lock()?;
        let usdc = IERC20::new(self.usdc.get());
        let recipient = self.vm().msg_sender();
        let vm = self.vm().clone();
        usdc.transfer(&vm, Call::new_mutating(self), recipient, amount)?;
        self.reentrancy_unlock();
        self.vm().log(Withdrawn {
            to: recipient,
            amount,
            isEth: false,
        });
        Ok(())
    }

    /// Step 1 of two-step ownership transfer (TV-7).
    /// Sets pending_owner; must be accepted by the new owner.
    pub fn transfer_ownership(&mut self, new_owner: Address) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if new_owner == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        self.pending_owner.set(new_owner);
        self.vm().log(OwnershipTransferStarted {
            currentOwner: self.owner.get(),
            pendingOwner: new_owner,
        });
        Ok(())
    }

    /// Step 2 of two-step ownership transfer (TV-7).
    /// Must be called by pending_owner to complete the transfer.
    pub fn accept_ownership(&mut self) -> Result<(), Vec<u8>> {
        let sender = self.vm().msg_sender();
        if sender != self.pending_owner.get() {
            return Err(b"not pending owner".to_vec());
        }
        let previous = self.owner.get();
        self.owner.set(sender);
        self.pending_owner.set(Address::ZERO);
        self.vm().log(OwnershipTransferred {
            previousOwner: previous,
            newOwner: sender,
        });
        Ok(())
    }

    /// Pause the contract. Owner only (CC-2).
    pub fn pause(&mut self) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if self.paused.get() {
            return Err(b"already paused".to_vec());
        }
        self.paused.set(true);
        self.vm().log(Paused {
            by: self.vm().msg_sender(),
        });
        Ok(())
    }

    /// Unpause the contract. Owner only (CC-2).
    pub fn unpause(&mut self) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if !self.paused.get() {
            return Err(b"not paused".to_vec());
        }
        self.paused.set(false);
        self.vm().log(Unpaused {
            by: self.vm().msg_sender(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, B256, U256};
    use alloy_sol_types::SolCall;
    use stylus_test::TestVM;

    // Re-declare function signatures for building calldata in mocks.
    sol! {
        function transfer(address to, uint256 amount) external returns (bool);
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }

    const OWNER: Address = address!("0000000000000000000000000000000000000001");
    const USDC_ADDR: Address = address!("af88d065e77c8cc2239327c5edb3a432268e5831");
    const BID_AGENT: Address = address!("0000000000000000000000000000000000000002");
    const STRANGER: Address = address!("0000000000000000000000000000000000000bad");
    const BUYER: Address = address!("0000000000000000000000000000000000000b01");
    const CONTRACT_ADDR: Address = address!("0000000000000000000000000000000000C0FFEE");
    const RESALE_PRICE: u64 = 5_000_000; // 5 USDC (6 decimals)
    const NEW_OWNER: Address = address!("0000000000000000000000000000000000000099");

    /// Shared setup: creates a TestVM, instantiates the contract, and initializes it.
    fn setup() -> (TestVM, TimeBoostVault) {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        vm.set_contract_address(CONTRACT_ADDR);
        let mut contract = TimeBoostVault::from(&vm);
        contract
            .initialize(USDC_ADDR, BID_AGENT, U256::from(RESALE_PRICE))
            .unwrap();
        (vm, contract)
    }

    /// Encode a successful `bool` return (true) for ERC20 calls.
    fn encode_bool_true() -> Vec<u8> {
        U256::from(1).to_be_bytes_vec()
    }

    /// Build the calldata for `transferFrom(from, to, amount)`.
    fn transfer_from_calldata(from: Address, to: Address, amount: U256) -> Vec<u8> {
        transferFromCall { from, to, amount }.abi_encode()
    }

    /// Build the calldata for `transfer(to, amount)`.
    fn transfer_calldata(to: Address, amount: U256) -> Vec<u8> {
        transferCall { to, amount }.abi_encode()
    }

    /// Build the calldata for `balanceOf(account)`.
    fn balance_of_calldata(account: Address) -> Vec<u8> {
        balanceOfCall { account }.abi_encode()
    }

    /// Helper: win a round so the vault is express lane controller.
    fn win_round(vm: &TestVM, contract: &mut TimeBoostVault, round: u64) {
        vm.set_sender(BID_AGENT);
        contract
            .record_round_win(U256::from(round), U256::from(100u64))
            .unwrap();
    }

    /// Helper: mock a successful transferFrom for a buyer purchase.
    fn mock_purchase(vm: &TestVM, buyer: Address, price: U256) {
        let calldata = transfer_from_calldata(buyer, CONTRACT_ADDR, price);
        vm.mock_call(USDC_ADDR, calldata, U256::ZERO, Ok(encode_bool_true()));
    }

    // ────────────────────────────── Initialize ──────────────────────────────

    #[test]
    fn test_initialize_sets_state() {
        let (_vm, contract) = setup();
        assert_eq!(contract.owner.get(), OWNER);
        assert_eq!(contract.usdc.get(), USDC_ADDR);
        assert_eq!(contract.bid_agent.get(), BID_AGENT);
        assert_eq!(contract.resale_price_usdc.get(), U256::from(RESALE_PRICE));
    }

    #[test]
    fn test_initialize_twice_fails() {
        let (_vm, mut contract) = setup();
        let err = contract
            .initialize(USDC_ADDR, BID_AGENT, U256::from(RESALE_PRICE))
            .unwrap_err();
        assert_eq!(err, b"already initialized".to_vec());
    }

    // ────────────────────────────── Access control ──────────────────────────

    #[test]
    fn test_only_owner_passes_for_owner() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.set_resale_price(U256::from(10u64)).unwrap();
    }

    #[test]
    fn test_only_owner_fails_for_stranger() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.set_resale_price(U256::from(10u64)).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn test_only_agent_passes_for_owner() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract
            .record_round_win(U256::from(1u64), U256::from(100u64))
            .unwrap();
    }

    #[test]
    fn test_only_agent_passes_for_bid_agent() {
        let (vm, mut contract) = setup();
        vm.set_sender(BID_AGENT);
        contract
            .record_round_win(U256::from(1u64), U256::from(100u64))
            .unwrap();
    }

    #[test]
    fn test_only_agent_fails_for_stranger() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract
            .record_round_win(U256::from(1u64), U256::from(100u64))
            .unwrap_err();
        assert_eq!(err, b"not authorized".to_vec());
    }

    // ────────────────────────────── Deposits ────────────────────────────────

    #[test]
    fn test_deposit_eth_emits_event() {
        let (vm, mut contract) = setup();
        let deposit_amount = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        vm.set_sender(OWNER);
        vm.set_value(deposit_amount);
        contract.deposit_eth().unwrap();

        let logs = vm.get_emitted_logs();
        assert!(!logs.is_empty(), "expected at least one log");
        let (topics, data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 2);
        let from_topic = B256::left_padding_from(OWNER.as_slice());
        assert_eq!(topics[1], from_topic);
        assert_eq!(data.len(), 64);
        let amount_decoded = U256::from_be_slice(&data[0..32]);
        let is_eth = U256::from_be_slice(&data[32..64]);
        assert_eq!(amount_decoded, deposit_amount);
        assert_eq!(is_eth, U256::from(1)); // true
    }

    #[test]
    fn test_deposit_usdc_emits_event() {
        let (vm, mut contract) = setup();
        let amount = U256::from(50_000_000u64); // 50 USDC
        vm.set_sender(OWNER);

        let calldata = transfer_from_calldata(OWNER, CONTRACT_ADDR, amount);
        vm.mock_call(USDC_ADDR, calldata, U256::ZERO, Ok(encode_bool_true()));

        contract.deposit_usdc(amount).unwrap();

        let logs = vm.get_emitted_logs();
        assert!(!logs.is_empty());
        let (topics, data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 2);
        let from_topic = B256::left_padding_from(OWNER.as_slice());
        assert_eq!(topics[1], from_topic);
        let is_eth = U256::from_be_slice(&data[32..64]);
        assert_eq!(is_eth, U256::ZERO); // false
    }

    // ────────────────────────────── Round management ────────────────────────

    #[test]
    fn test_record_round_win_sets_state_and_emits() {
        let (vm, mut contract) = setup();
        vm.set_sender(BID_AGENT);

        let round = U256::from(42u64);
        let bid_cost = U256::from(500_000u64);
        contract.record_round_win(round, bid_cost).unwrap();

        assert!(contract.is_express_lane_controller.get());
        assert_eq!(contract.current_round.get(), round);
        assert_eq!(contract.total_bid_cost.get(), bid_cost);

        let logs = vm.get_emitted_logs();
        assert!(!logs.is_empty());
        let (topics, data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 2);
        let round_topic = B256::from(round);
        assert_eq!(topics[1], round_topic);
        let bid_cost_decoded = U256::from_be_slice(&data[0..32]);
        assert_eq!(bid_cost_decoded, bid_cost);
    }

    #[test]
    fn test_record_round_win_accumulates_bid_cost() {
        let (vm, mut contract) = setup();
        vm.set_sender(BID_AGENT);

        contract
            .record_round_win(U256::from(1u64), U256::from(100u64))
            .unwrap();
        contract
            .record_round_win(U256::from(2u64), U256::from(250u64))
            .unwrap();
        assert_eq!(contract.total_bid_cost.get(), U256::from(350u64));
    }

    #[test]
    fn test_record_round_win_only_agent() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract
            .record_round_win(U256::from(1u64), U256::from(100u64))
            .unwrap_err();
        assert_eq!(err, b"not authorized".to_vec());
    }

    #[test]
    fn test_end_round_resets_controller() {
        let (vm, mut contract) = setup();
        vm.set_sender(BID_AGENT);
        contract
            .record_round_win(U256::from(1u64), U256::from(100u64))
            .unwrap();
        assert!(contract.is_express_lane_controller.get());

        contract.end_round().unwrap();
        assert!(!contract.is_express_lane_controller.get());
    }

    #[test]
    fn test_end_round_only_agent() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.end_round().unwrap_err();
        assert_eq!(err, b"not authorized".to_vec());
    }

    // ────────────────────────────── Express lane resale ─────────────────────

    #[test]
    fn test_purchase_express_lane_fails_when_not_controller() {
        let (vm, mut contract) = setup();
        vm.set_sender(BUYER);
        let err = contract.purchase_express_lane_access().unwrap_err();
        assert_eq!(err, b"not controlling express lane".to_vec());
    }

    #[test]
    fn test_purchase_express_lane_success() {
        let (vm, mut contract) = setup();

        let round = U256::from(10u64);
        win_round(&vm, &mut contract, 10);

        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        mock_purchase(&vm, BUYER, price);

        contract.purchase_express_lane_access().unwrap();

        // Buyer should be authorized via round-scoped mapping
        assert!(contract.round_authorized_buyers.getter(round).get(BUYER));
        assert!(contract.is_authorized_buyer(BUYER));

        assert_eq!(contract.total_resale_earnings.get(), price);

        let logs = vm.get_emitted_logs();
        let (topics, data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 2);
        let buyer_topic = B256::left_padding_from(BUYER.as_slice());
        assert_eq!(topics[1], buyer_topic);
        let price_decoded = U256::from_be_slice(&data[0..32]);
        let round_decoded = U256::from_be_slice(&data[32..64]);
        assert_eq!(price_decoded, price);
        assert_eq!(round_decoded, round);
    }

    #[test]
    fn test_is_authorized_buyer_false_before_purchase() {
        let (_vm, contract) = setup();
        assert!(!contract.is_authorized_buyer(BUYER));
    }

    #[test]
    fn test_is_authorized_buyer_true_after_purchase() {
        let (vm, mut contract) = setup();
        win_round(&vm, &mut contract, 1);

        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        mock_purchase(&vm, BUYER, price);
        contract.purchase_express_lane_access().unwrap();

        assert!(contract.is_authorized_buyer(BUYER));
    }

    #[test]
    fn test_purchase_accumulates_earnings() {
        let (vm, mut contract) = setup();
        win_round(&vm, &mut contract, 1);

        let price = U256::from(RESALE_PRICE);

        vm.set_sender(BUYER);
        mock_purchase(&vm, BUYER, price);
        contract.purchase_express_lane_access().unwrap();

        let buyer2 = address!("0000000000000000000000000000000000000b02");
        vm.set_sender(buyer2);
        mock_purchase(&vm, buyer2, price);
        contract.purchase_express_lane_access().unwrap();

        assert_eq!(
            contract.total_resale_earnings.get(),
            U256::from(RESALE_PRICE * 2)
        );
    }

    // ────────────────────────────── Admin ────────────────────────────────────

    #[test]
    fn test_set_resale_price_owner_only() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let new_price = U256::from(10_000_000u64); // 10 USDC
        contract.set_resale_price(new_price).unwrap();
        assert_eq!(contract.resale_price_usdc.get(), new_price);

        let logs = vm.get_emitted_logs();
        let (topics, data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 1);
        let old_decoded = U256::from_be_slice(&data[0..32]);
        let new_decoded = U256::from_be_slice(&data[32..64]);
        assert_eq!(old_decoded, U256::from(RESALE_PRICE));
        assert_eq!(new_decoded, new_price);
    }

    #[test]
    fn test_set_resale_price_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract
            .set_resale_price(U256::from(10_000_000u64))
            .unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    // ────────────────────────────── Withdrawals ─────────────────────────────

    #[test]
    fn test_withdraw_eth_owner_only() {
        let (vm, mut contract) = setup();
        let amount = U256::from(500_000_000_000_000_000u128); // 0.5 ETH
        vm.set_sender(OWNER);

        vm.mock_call(OWNER, vec![], amount, Ok(vec![]));

        contract.withdraw_eth(amount).unwrap();

        let logs = vm.get_emitted_logs();
        let (topics, data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 2);
        let to_topic = B256::left_padding_from(OWNER.as_slice());
        assert_eq!(topics[1], to_topic);
        let is_eth = U256::from_be_slice(&data[32..64]);
        assert_eq!(is_eth, U256::from(1)); // true
    }

    #[test]
    fn test_withdraw_eth_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract
            .withdraw_eth(U256::from(100u64))
            .unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn test_withdraw_usdc_owner_only() {
        let (vm, mut contract) = setup();
        let amount = U256::from(25_000_000u64); // 25 USDC
        vm.set_sender(OWNER);

        let calldata = transfer_calldata(OWNER, amount);
        vm.mock_call(USDC_ADDR, calldata, U256::ZERO, Ok(encode_bool_true()));

        contract.withdraw_usdc(amount).unwrap();

        let logs = vm.get_emitted_logs();
        let (topics, data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 2);
        let to_topic = B256::left_padding_from(OWNER.as_slice());
        assert_eq!(topics[1], to_topic);
        let is_eth = U256::from_be_slice(&data[32..64]);
        assert_eq!(is_eth, U256::ZERO); // false
    }

    #[test]
    fn test_withdraw_usdc_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract
            .withdraw_usdc(U256::from(100u64))
            .unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    // ────────────────────────────── Stats ────────────────────────────────────

    #[test]
    fn test_get_stats_returns_correct_values() {
        let (vm, mut contract) = setup();

        vm.set_sender(BID_AGENT);
        contract
            .record_round_win(U256::from(5u64), U256::from(1000u64))
            .unwrap();

        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        mock_purchase(&vm, BUYER, price);
        contract.purchase_express_lane_access().unwrap();

        let eth_balance = U256::from(2_000_000_000_000_000_000u128); // 2 ETH
        vm.set_balance(CONTRACT_ADDR, eth_balance);

        let usdc_balance = U256::from(75_000_000u64); // 75 USDC
        let bal_calldata = balance_of_calldata(CONTRACT_ADDR);
        vm.mock_static_call(USDC_ADDR, bal_calldata, Ok(usdc_balance.to_be_bytes_vec()));

        let (eth, usdc, earnings, bid_cost, is_controller) = contract.get_stats().unwrap();
        assert_eq!(eth, eth_balance);
        assert_eq!(usdc, usdc_balance);
        assert_eq!(earnings, price);
        assert_eq!(bid_cost, U256::from(1000u64));
        assert!(is_controller);
    }

    #[test]
    fn test_get_stats_default_values() {
        let (vm, contract) = setup();

        let eth_balance = U256::ZERO;
        vm.set_balance(CONTRACT_ADDR, eth_balance);

        let usdc_balance = U256::ZERO;
        let bal_calldata = balance_of_calldata(CONTRACT_ADDR);
        vm.mock_static_call(USDC_ADDR, bal_calldata, Ok(usdc_balance.to_be_bytes_vec()));

        let (eth, usdc, earnings, bid_cost, is_controller) = contract.get_stats().unwrap();
        assert_eq!(eth, U256::ZERO);
        assert_eq!(usdc, U256::ZERO);
        assert_eq!(earnings, U256::ZERO);
        assert_eq!(bid_cost, U256::ZERO);
        assert!(!is_controller);
    }

    // ────────────────────────────── TV-2: CEI ordering ──────────────────────

    #[test]
    fn test_purchase_cei_ordering() {
        let (vm, mut contract) = setup();
        win_round(&vm, &mut contract, 5);

        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        mock_purchase(&vm, BUYER, price);

        contract.purchase_express_lane_access().unwrap();

        // State was updated (effects applied before interaction)
        assert!(contract.is_authorized_buyer(BUYER));
        assert_eq!(contract.total_resale_earnings.get(), price);
    }

    #[test]
    fn test_reentrancy_guard_blocks() {
        let (vm, mut contract) = setup();
        win_round(&vm, &mut contract, 1);

        // Manually set the locked flag to simulate a reentrant call
        contract.locked.set(true);

        vm.set_sender(BUYER);
        let err = contract.purchase_express_lane_access().unwrap_err();
        assert_eq!(err, b"reentrancy".to_vec());

        // Also test deposit_usdc
        vm.set_sender(OWNER);
        let err = contract.deposit_usdc(U256::from(100u64)).unwrap_err();
        assert_eq!(err, b"reentrancy".to_vec());

        // Also test withdraw_usdc
        let err = contract.withdraw_usdc(U256::from(100u64)).unwrap_err();
        assert_eq!(err, b"reentrancy".to_vec());
    }

    // ────────────────────────────── TV-1: Round-scoped buyers ───────────────

    #[test]
    fn test_authorized_buyer_round_scoped() {
        let (vm, mut contract) = setup();

        // Round 1: buyer purchases access
        win_round(&vm, &mut contract, 1);
        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        mock_purchase(&vm, BUYER, price);
        contract.purchase_express_lane_access().unwrap();
        assert!(contract.is_authorized_buyer(BUYER));

        // End round 1, start round 2
        vm.set_sender(BID_AGENT);
        contract.end_round().unwrap();
        win_round(&vm, &mut contract, 2);

        // Buyer from round 1 should NOT be authorized in round 2
        assert!(!contract.is_authorized_buyer(BUYER));
    }

    #[test]
    fn test_authorized_buyer_current_round() {
        let (vm, mut contract) = setup();
        win_round(&vm, &mut contract, 7);

        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        mock_purchase(&vm, BUYER, price);
        contract.purchase_express_lane_access().unwrap();

        // Buyer authorized in current round
        assert!(contract.is_authorized_buyer(BUYER));
        // Random address not authorized
        assert!(!contract.is_authorized_buyer(STRANGER));
    }

    #[test]
    fn test_end_round_no_stale_buyers() {
        let (vm, mut contract) = setup();

        // Round 1: two buyers purchase
        win_round(&vm, &mut contract, 1);

        vm.set_sender(BUYER);
        mock_purchase(&vm, BUYER, U256::from(RESALE_PRICE));
        contract.purchase_express_lane_access().unwrap();

        let buyer2 = address!("0000000000000000000000000000000000000b02");
        vm.set_sender(buyer2);
        mock_purchase(&vm, buyer2, U256::from(RESALE_PRICE));
        contract.purchase_express_lane_access().unwrap();

        assert!(contract.is_authorized_buyer(BUYER));
        assert!(contract.is_authorized_buyer(buyer2));

        // End round 1, start round 3 (skipping round 2)
        vm.set_sender(BID_AGENT);
        contract.end_round().unwrap();
        win_round(&vm, &mut contract, 3);

        // Neither buyer is authorized in round 3
        assert!(!contract.is_authorized_buyer(BUYER));
        assert!(!contract.is_authorized_buyer(buyer2));
    }

    // ────────────────────────────── TV-3: Zero resale price ─────────────────

    #[test]
    fn test_set_resale_price_zero_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let err = contract.set_resale_price(U256::ZERO).unwrap_err();
        assert_eq!(err, b"price cannot be zero".to_vec());
    }

    // ────────────────────────────── TV-7: Two-step ownership ────────────────

    #[test]
    fn test_transfer_ownership_propose_accept() {
        let (vm, mut contract) = setup();

        // Owner proposes transfer
        vm.set_sender(OWNER);
        contract.transfer_ownership(NEW_OWNER).unwrap();
        assert_eq!(contract.pending_owner.get(), NEW_OWNER);

        // Check OwnershipTransferStarted event
        let logs = vm.get_emitted_logs();
        let (topics, _data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 3); // sig + indexed currentOwner + indexed pendingOwner
        let owner_topic = B256::left_padding_from(OWNER.as_slice());
        let new_owner_topic = B256::left_padding_from(NEW_OWNER.as_slice());
        assert_eq!(topics[1], owner_topic);
        assert_eq!(topics[2], new_owner_topic);

        // New owner accepts
        vm.set_sender(NEW_OWNER);
        contract.accept_ownership().unwrap();
        assert_eq!(contract.owner.get(), NEW_OWNER);
        assert_eq!(contract.pending_owner.get(), Address::ZERO);

        // Check OwnershipTransferred event
        let logs = vm.get_emitted_logs();
        let (topics, _data) = &logs[logs.len() - 1];
        assert_eq!(topics.len(), 3);
        assert_eq!(topics[1], owner_topic); // previousOwner
        assert_eq!(topics[2], new_owner_topic); // newOwner
    }

    #[test]
    fn test_transfer_ownership_wrong_acceptor_rejected() {
        let (vm, mut contract) = setup();

        vm.set_sender(OWNER);
        contract.transfer_ownership(NEW_OWNER).unwrap();

        // Stranger tries to accept
        vm.set_sender(STRANGER);
        let err = contract.accept_ownership().unwrap_err();
        assert_eq!(err, b"not pending owner".to_vec());

        // Owner still unchanged
        assert_eq!(contract.owner.get(), OWNER);
    }

    #[test]
    fn test_transfer_ownership_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.transfer_ownership(NEW_OWNER).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    // ────────────────────────────── CC-2: Pausable ──────────────────────────

    #[test]
    fn test_pause_blocks_deposits() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.pause().unwrap();
        assert!(contract.paused.get());

        // deposit_eth should fail
        vm.set_value(U256::from(1_000u64));
        let err = contract.deposit_eth().unwrap_err();
        assert_eq!(err, b"paused".to_vec());

        // deposit_usdc should fail
        let err = contract.deposit_usdc(U256::from(100u64)).unwrap_err();
        assert_eq!(err, b"paused".to_vec());

        // purchase_express_lane_access should fail (set controller first)
        contract.is_express_lane_controller.set(true);
        vm.set_sender(BUYER);
        let err = contract.purchase_express_lane_access().unwrap_err();
        assert_eq!(err, b"paused".to_vec());
    }

    #[test]
    fn test_unpause_allows_deposits() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.pause().unwrap();
        contract.unpause().unwrap();
        assert!(!contract.paused.get());

        // deposit_eth should succeed again
        vm.set_value(U256::from(1_000u64));
        contract.deposit_eth().unwrap();
    }

    #[test]
    fn test_pause_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.pause().unwrap_err();
        assert_eq!(err, b"not owner".to_vec());

        // Also test unpause
        vm.set_sender(OWNER);
        contract.pause().unwrap();
        vm.set_sender(STRANGER);
        let err = contract.unpause().unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    // ─── Gap tests: error recovery & lifecycle ──────────────────────────

    #[test]
    fn test_purchase_transfer_failure_rollback() {
        let (vm, mut contract) = setup();
        win_round(&vm, &mut contract, 1);

        // Mock a FAILING transferFrom for this buyer
        let price = U256::from(RESALE_PRICE);
        let calldata = transfer_from_calldata(BUYER, CONTRACT_ADDR, price);
        vm.mock_call(USDC_ADDR, calldata, U256::ZERO, Err(b"insufficient allowance".to_vec()));

        // Attempt purchase — should fail
        vm.set_sender(BUYER);
        let err = contract.purchase_express_lane_access().unwrap_err();
        assert_eq!(err, b"usdc transfer failed".to_vec());

        // Verify rollback: buyer is NOT authorized, earnings unchanged
        assert!(!contract.round_authorized_buyers.getter(U256::from(1u64)).get(BUYER));
        assert_eq!(contract.total_resale_earnings.get(), U256::ZERO);
        // Verify reentrancy lock was released
        assert!(!contract.locked.get());
    }

    #[test]
    fn test_full_lifecycle() {
        let (vm, mut contract) = setup();

        // 1. Deposit ETH
        vm.set_sender(OWNER);
        vm.set_value(U256::from(1_000_000u64));
        contract.deposit_eth().unwrap();

        // 2. Record round win
        win_round(&vm, &mut contract, 1);
        assert!(contract.is_express_lane_controller.get());
        assert_eq!(contract.current_round.get(), U256::from(1u64));

        // 3. Buyer purchases express lane access
        let price = U256::from(RESALE_PRICE);
        mock_purchase(&vm, BUYER, price);
        vm.set_sender(BUYER);
        contract.purchase_express_lane_access().unwrap();
        assert!(contract.is_authorized_buyer(BUYER));
        assert_eq!(contract.total_resale_earnings.get(), price);

        // 4. End round
        vm.set_sender(BID_AGENT);
        contract.end_round().unwrap();
        assert!(!contract.is_express_lane_controller.get());

        // 5. Buyer is no longer authorized (round-scoped) — next round
        win_round(&vm, &mut contract, 2);
        // Buyer from round 1 is not authorized in round 2
        assert!(!contract.round_authorized_buyers.getter(U256::from(2u64)).get(BUYER));

        // 6. Withdraw USDC earnings
        let withdraw_calldata = transfer_calldata(OWNER, price);
        vm.mock_call(USDC_ADDR, withdraw_calldata, U256::ZERO, Ok(encode_bool_true()));
        vm.set_sender(OWNER);
        contract.withdraw_usdc(price).unwrap();
    }
}
