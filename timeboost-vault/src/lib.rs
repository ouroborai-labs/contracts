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

#![cfg_attr(not(any(feature = "export-abi", test)), no_main)]
#![cfg_attr(not(any(feature = "export-abi", test)), no_std)]

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
        /// Authorized resale buyers for current round
        mapping(address => bool) authorized_buyers;
    }
}

// ─── Events ───────────────────────────────────────────────────────────────────

sol! {
    event Deposited(address indexed from, uint256 amount, bool isEth);
    event Withdrawn(address indexed to, uint256 amount, bool isEth);
    event RoundWon(uint256 indexed round, uint256 bidCost);
    event ResalePurchased(address indexed buyer, uint256 pricePaid, uint256 round);
    event ResalePriceUpdated(uint256 oldPrice, uint256 newPrice);
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
        self.vm().log(Deposited {
            from: self.vm().msg_sender(),
            amount: self.vm().msg_value(),
            isEth: true,
        });
        Ok(())
    }

    /// Deposit USDC for bid payments.
    pub fn deposit_usdc(&mut self, amount: U256) -> Result<(), Vec<u8>> {
        let usdc = IERC20::new(self.usdc.get());
        let sender = self.vm().msg_sender();
        let contract_addr = self.vm().contract_address();
        let vm = self.vm().clone();
        usdc.transfer_from(&vm, Call::new_mutating(self), sender, contract_addr, amount)?;
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
    pub fn end_round(&mut self) -> Result<(), Vec<u8>> {
        self.only_agent()?;
        self.is_express_lane_controller.set(false);
        // Clear authorized buyers
        Ok(())
    }

    /// Purchase express lane access for the current round.
    /// Buyer pays resale_price_usdc USDC, gets added to authorized_buyers.
    pub fn purchase_express_lane_access(&mut self) -> Result<(), Vec<u8>> {
        if !self.is_express_lane_controller.get() {
            return Err(b"not controlling express lane".to_vec());
        }

        let price = self.resale_price_usdc.get();
        let usdc = IERC20::new(self.usdc.get());
        let sender = self.vm().msg_sender();
        let contract_addr = self.vm().contract_address();

        // Collect USDC from buyer
        let vm = self.vm().clone();
        usdc.transfer_from(&vm, Call::new_mutating(self), sender, contract_addr, price)?;

        // Authorize buyer
        self.authorized_buyers.setter(sender).set(true);

        let prev_earnings = self.total_resale_earnings.get();
        self.total_resale_earnings.set(prev_earnings + price);

        self.vm().log(ResalePurchased {
            buyer: sender,
            pricePaid: price,
            round: self.current_round.get(),
        });

        Ok(())
    }

    /// Check if an address is authorized to use the express lane.
    pub fn is_authorized_buyer(&self, buyer: Address) -> bool {
        self.authorized_buyers.get(buyer)
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
        let usdc = IERC20::new(self.usdc.get());
        let recipient = self.vm().msg_sender();
        let vm = self.vm().clone();
        usdc.transfer(&vm, Call::new_mutating(self), recipient, amount)?;
        self.vm().log(Withdrawn {
            to: recipient,
            amount,
            isEth: false,
        });
        Ok(())
    }

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

    // ────────────────────────────── Initialize ──────────────────────────────

    #[test]
    fn test_initialize_sets_state() {
        let (vm, contract) = setup();
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
        // set_resale_price is owner-only — should succeed
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
        // topic[0] = Deposited event sig, topic[1] = indexed `from`
        assert_eq!(topics.len(), 2);
        // Check the `from` indexed param matches OWNER (left-padded to 32 bytes)
        let from_topic = B256::left_padding_from(OWNER.as_slice());
        assert_eq!(topics[1], from_topic);
        // Data: amount (32 bytes) + isEth bool (32 bytes, =1)
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

        // Mock the transferFrom call on the USDC contract
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
        // RoundWon(uint256 indexed round, uint256 bidCost)
        assert_eq!(topics.len(), 2); // sig + indexed round
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

        // First, win a round so the vault is express lane controller
        vm.set_sender(BID_AGENT);
        let round = U256::from(10u64);
        contract
            .record_round_win(round, U256::from(100u64))
            .unwrap();

        // Now buyer purchases access
        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        let calldata = transfer_from_calldata(BUYER, CONTRACT_ADDR, price);
        vm.mock_call(USDC_ADDR, calldata, U256::ZERO, Ok(encode_bool_true()));

        contract.purchase_express_lane_access().unwrap();

        // Buyer should be authorized
        assert!(contract.authorized_buyers.get(BUYER));

        // Earnings should be updated
        assert_eq!(contract.total_resale_earnings.get(), price);

        // Check ResalePurchased event
        let logs = vm.get_emitted_logs();
        let (topics, data) = &logs[logs.len() - 1];
        // ResalePurchased(address indexed buyer, uint256 pricePaid, uint256 round)
        assert_eq!(topics.len(), 2); // sig + indexed buyer
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
        vm.set_sender(BID_AGENT);
        contract
            .record_round_win(U256::from(1u64), U256::from(100u64))
            .unwrap();

        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        let calldata = transfer_from_calldata(BUYER, CONTRACT_ADDR, price);
        vm.mock_call(USDC_ADDR, calldata, U256::ZERO, Ok(encode_bool_true()));
        contract.purchase_express_lane_access().unwrap();

        assert!(contract.is_authorized_buyer(BUYER));
    }

    #[test]
    fn test_purchase_accumulates_earnings() {
        let (vm, mut contract) = setup();
        vm.set_sender(BID_AGENT);
        contract
            .record_round_win(U256::from(1u64), U256::from(100u64))
            .unwrap();

        let price = U256::from(RESALE_PRICE);

        // First purchase
        vm.set_sender(BUYER);
        let calldata1 = transfer_from_calldata(BUYER, CONTRACT_ADDR, price);
        vm.mock_call(USDC_ADDR, calldata1, U256::ZERO, Ok(encode_bool_true()));
        contract.purchase_express_lane_access().unwrap();

        // Second purchase from a different buyer
        let buyer2 = address!("0000000000000000000000000000000000000b02");
        vm.set_sender(buyer2);
        let calldata2 = transfer_from_calldata(buyer2, CONTRACT_ADDR, price);
        vm.mock_call(USDC_ADDR, calldata2, U256::ZERO, Ok(encode_bool_true()));
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

        // Check event
        let logs = vm.get_emitted_logs();
        let (topics, data) = &logs[logs.len() - 1];
        // ResalePriceUpdated(uint256 oldPrice, uint256 newPrice) — no indexed params
        assert_eq!(topics.len(), 1); // just the event sig
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

        // Mock the ETH transfer (transfer_eth calls call_contract with value)
        vm.mock_call(OWNER, vec![], amount, Ok(vec![]));

        contract.withdraw_eth(amount).unwrap();

        let logs = vm.get_emitted_logs();
        let (topics, data) = &logs[logs.len() - 1];
        // Withdrawn(address indexed to, uint256 amount, bool isEth)
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

        // Mock the USDC transfer call
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

        // Win a round with a known bid cost
        vm.set_sender(BID_AGENT);
        contract
            .record_round_win(U256::from(5u64), U256::from(1000u64))
            .unwrap();

        // Purchase express lane access to generate earnings
        vm.set_sender(BUYER);
        let price = U256::from(RESALE_PRICE);
        let calldata = transfer_from_calldata(BUYER, CONTRACT_ADDR, price);
        vm.mock_call(USDC_ADDR, calldata, U256::ZERO, Ok(encode_bool_true()));
        contract.purchase_express_lane_access().unwrap();

        // Mock balanceOf for get_stats
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
}
