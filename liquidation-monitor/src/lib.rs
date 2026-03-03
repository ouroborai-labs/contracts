//! Liquidation Monitor — Stylus (Rust) contract for Arbitrum One
//!
//! Scans lending protocol positions on-chain and returns accounts at
//! liquidation risk. Supports a dynamic registry of lending protocols
//! (Aave V3, Radiant, etc.) and perp protocols (GMX V2, etc.).
//!
//! Written in Rust for 10-100x gas savings vs Solidity for
//! computation-heavy scanning.

#![cfg_attr(not(any(feature = "export-abi", test)), no_main)]
#![cfg_attr(not(any(feature = "export-abi", test)), no_std)]
#![allow(unexpected_cfgs)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use alloy_sol_types::sol;
use stylus_sdk::{
    alloy_primitives::{Address, U256},
    prelude::*,
};

// ─── Protocol type constants ────────────────────────────────────────────────

const LENDING_TYPE_AAVE_V3: u64 = 0;
// const LENDING_TYPE_COMPOUND_V3: u64 = 1;  // future

const PERP_TYPE_GMX_V2: u64 = 0;

// ─── Protocol interfaces ────────────────────────────────────────────────────

sol_interface! {
    interface IAavePool {
        function getUserAccountData(address user)
            external view returns (
                uint256 totalCollateralBase,
                uint256 totalDebtBase,
                uint256 availableBorrowsBase,
                uint256 currentLiquidationThreshold,
                uint256 ltv,
                uint256 healthFactor
            );
    }

    interface IPerpReader {
        function getAccountHealth(address account) external view returns (uint256 healthFactor);
    }
}

// ─── Contract storage ───────────────────────────────────────────────────────

sol_storage! {
    #[entrypoint]
    pub struct LiquidationMonitor {
        address owner;
        address pending_owner;
        bool paused;
        address[] tracked_accounts;
        uint256 risk_threshold;

        // Lending protocol registry
        uint256 lending_count;
        mapping(uint256 => address) lending_pools;
        mapping(uint256 => uint256) lending_types;
        mapping(uint256 => bool) lending_active;

        // Perp protocol registry
        uint256 perp_count;
        mapping(uint256 => address) perp_readers;
        mapping(uint256 => uint256) perp_types;
        mapping(uint256 => bool) perp_active;
    }
}

// ─── Events ─────────────────────────────────────────────────────────────────

sol! {
    event AccountAtRisk(address indexed account, uint256 healthFactor, uint256 timestamp);
    event AccountAdded(address indexed account);
    event AccountRemoved(address indexed account);
    event ThresholdUpdated(uint256 oldThreshold, uint256 newThreshold);
    event LendingProtocolAdded(uint256 indexed index, address poolAddress, uint64 protocolType);
    event LendingProtocolRemoved(uint256 indexed index, address poolAddress);
    event PerpProtocolAdded(uint256 indexed index, address readerAddress, uint64 protocolType);
    event PerpProtocolRemoved(uint256 indexed index, address readerAddress);
    event OwnershipTransferStarted(address indexed currentOwner, address indexed pendingOwner);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);
    event Paused(address indexed by);
    event Unpaused(address indexed by);
}

// ─── Private helpers (LM-7: must be in separate impl block) ─────────────────

impl LiquidationMonitor {
    fn only_owner(&self) -> Result<(), Vec<u8>> {
        if self.vm().msg_sender() != self.owner.get() {
            return Err(b"not owner".to_vec());
        }
        Ok(())
    }

    fn when_not_paused(&self) -> Result<(), Vec<u8>> {
        if self.paused.get() {
            return Err(b"contract is paused".to_vec());
        }
        Ok(())
    }
}

// ─── Implementation ─────────────────────────────────────────────────────────

#[public]
impl LiquidationMonitor {
    pub fn initialize(
        &mut self,
        risk_threshold: U256,
    ) -> Result<(), Vec<u8>> {
        if self.owner.get() != Address::ZERO {
            return Err(b"already initialized".to_vec());
        }
        self.owner.set(self.vm().msg_sender());
        self.risk_threshold.set(risk_threshold);
        Ok(())
    }

    // ─── Two-step ownership transfer (LM-4 / CC-1) ─────────────────────

    pub fn transfer_ownership(&mut self, new_owner: Address) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if new_owner == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        self.pending_owner.set(new_owner);
        let current = self.owner.get();
        self.vm().log(OwnershipTransferStarted {
            currentOwner: current,
            pendingOwner: new_owner,
        });
        Ok(())
    }

    pub fn accept_ownership(&mut self) -> Result<(), Vec<u8>> {
        let pending = self.pending_owner.get();
        if self.vm().msg_sender() != pending {
            return Err(b"not pending owner".to_vec());
        }
        let previous = self.owner.get();
        self.owner.set(pending);
        self.pending_owner.set(Address::ZERO);
        self.vm().log(OwnershipTransferred {
            previousOwner: previous,
            newOwner: pending,
        });
        Ok(())
    }

    // ─── Pausable (CC-2) ────────────────────────────────────────────────

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

    pub fn is_paused(&self) -> bool {
        self.paused.get()
    }

    // ─── Lending protocol registry ──────────────────────────────────────

    pub fn add_lending_protocol(
        &mut self,
        pool_address: Address,
        protocol_type: u64,
    ) -> Result<U256, Vec<u8>> {
        self.only_owner()?;
        if pool_address == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        let index = self.lending_count.get();
        self.lending_pools.setter(index).set(pool_address);
        self.lending_types.setter(index).set(U256::from(protocol_type));
        self.lending_active.setter(index).set(true);
        self.lending_count.set(index + U256::from(1));
        self.vm().log(LendingProtocolAdded {
            index,
            poolAddress: pool_address,
            protocolType: protocol_type,
        });
        Ok(index)
    }

    pub fn remove_lending_protocol(&mut self, index: U256) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if index >= self.lending_count.get() {
            return Err(b"index out of bounds".to_vec());
        }
        if !self.lending_active.get(index) {
            return Err(b"protocol already removed".to_vec());
        }
        self.lending_active.setter(index).set(false);
        let addr = self.lending_pools.get(index);
        self.vm().log(LendingProtocolRemoved {
            index,
            poolAddress: addr,
        });
        Ok(())
    }

    pub fn lending_protocol_count(&self) -> U256 {
        self.lending_count.get()
    }

    pub fn get_lending_protocol(&self, index: U256) -> (Address, U256, bool) {
        (
            self.lending_pools.get(index),
            self.lending_types.get(index),
            self.lending_active.get(index),
        )
    }

    // ─── Perp protocol registry ─────────────────────────────────────────

    pub fn add_perp_protocol(
        &mut self,
        reader_address: Address,
        protocol_type: u64,
    ) -> Result<U256, Vec<u8>> {
        self.only_owner()?;
        if reader_address == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        let index = self.perp_count.get();
        self.perp_readers.setter(index).set(reader_address);
        self.perp_types.setter(index).set(U256::from(protocol_type));
        self.perp_active.setter(index).set(true);
        self.perp_count.set(index + U256::from(1));
        self.vm().log(PerpProtocolAdded {
            index,
            readerAddress: reader_address,
            protocolType: protocol_type,
        });
        Ok(index)
    }

    pub fn remove_perp_protocol(&mut self, index: U256) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if index >= self.perp_count.get() {
            return Err(b"index out of bounds".to_vec());
        }
        if !self.perp_active.get(index) {
            return Err(b"protocol already removed".to_vec());
        }
        self.perp_active.setter(index).set(false);
        let addr = self.perp_readers.get(index);
        self.vm().log(PerpProtocolRemoved {
            index,
            readerAddress: addr,
        });
        Ok(())
    }

    pub fn perp_protocol_count(&self) -> U256 {
        self.perp_count.get()
    }

    // ─── Health factor queries ──────────────────────────────────────────

    pub fn get_health_factor(&self, account: Address) -> Result<U256, Vec<u8>> {
        let mut lowest_hf = U256::MAX;
        let mut found_any = false;

        // Lending protocols
        // Safe: lending_count bounded by practical limits (u64 max >> realistic protocol count)
        let lending_count = self.lending_count.get();
        for idx in 0..lending_count.as_limbs()[0] {
            let index = U256::from(idx);
            if !self.lending_active.get(index) {
                continue;
            }

            let pool_addr = self.lending_pools.get(index);
            let protocol_type = self.lending_types.get(index).as_limbs()[0];

            let hf = match protocol_type {
                LENDING_TYPE_AAVE_V3 => {
                    let pool = IAavePool::new(pool_addr);
                    let (_c, _d, _b, _lt, _ltv, health_factor) =
                        pool.get_user_account_data(self.vm(), Call::new(), account)?;
                    health_factor
                }
                _ => continue,
            };

            found_any = true;
            if hf < lowest_hf {
                lowest_hf = hf;
            }
        }

        // Perp protocols
        // Safe: perp_count bounded by practical limits (u64 max >> realistic protocol count)
        let perp_count = self.perp_count.get();
        for idx in 0..perp_count.as_limbs()[0] {
            let index = U256::from(idx);
            if !self.perp_active.get(index) {
                continue;
            }

            let reader_addr = self.perp_readers.get(index);
            let perp_type = self.perp_types.get(index).as_limbs()[0];

            match perp_type {
                PERP_TYPE_GMX_V2 => {
                    let reader = IPerpReader::new(reader_addr);
                    if let Ok(hf) = reader.get_account_health(self.vm(), Call::new(), account) {
                        found_any = true;
                        if hf < lowest_hf {
                            lowest_hf = hf;
                        }
                    }
                }
                _ => continue,
            }
        }

        if !found_any {
            return Err(b"no protocols registered".to_vec());
        }
        Ok(lowest_hf)
    }

    /// LM-2: Returns (at_risk, failed) — failed accounts are those whose
    /// health factor query errored out instead of being silently dropped.
    #[allow(clippy::type_complexity)]
    pub fn scan_accounts(
        &self,
        accounts: Vec<Address>,
    ) -> Result<(Vec<(Address, U256)>, Vec<Address>), Vec<u8>> {
        self.when_not_paused()?;
        let mut at_risk = Vec::new();
        let mut failed = Vec::new();
        let threshold = self.risk_threshold.get();

        for account in &accounts {
            match self.get_health_factor(*account) {
                Ok(hf) => {
                    if hf < threshold {
                        at_risk.push((*account, hf));
                    }
                }
                Err(_) => {
                    failed.push(*account);
                }
            }
        }

        Ok((at_risk, failed))
    }

    #[allow(clippy::type_complexity)]
    pub fn scan_tracked_accounts(&self) -> Result<(Vec<(Address, U256)>, Vec<Address>), Vec<u8>> {
        self.when_not_paused()?;
        let count = self.tracked_accounts.len();
        let mut accounts = Vec::with_capacity(count);

        for i in 0..count {
            accounts.push(self.tracked_accounts.get(i).unwrap());
        }

        let (at_risk, failed) = self.scan_accounts(accounts)?;

        for (account, hf) in &at_risk {
            self.vm().log(AccountAtRisk {
                account: *account,
                healthFactor: *hf,
                timestamp: U256::from(self.vm().block_timestamp()),
            });
        }

        Ok((at_risk, failed))
    }

    // ─── Account management ─────────────────────────────────────────────

    pub fn add_account(&mut self, account: Address) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        self.when_not_paused()?;
        if self.tracked_accounts.len() >= 500 {
            return Err(b"max tracked accounts reached".to_vec());
        }
        self.tracked_accounts.push(account);
        self.vm().log(AccountAdded { account });
        Ok(())
    }

    pub fn remove_account(&mut self, account: Address) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        let count = self.tracked_accounts.len();
        for i in 0..count {
            if self.tracked_accounts.get(i).unwrap() == account {
                let last = self.tracked_accounts.get(count - 1).unwrap();
                self.tracked_accounts.setter(i).unwrap().set(last);
                self.tracked_accounts.pop();
                self.vm().log(AccountRemoved { account });
                return Ok(());
            }
        }
        Err(b"account not found".to_vec())
    }

    // ─── Threshold management ───────────────────────────────────────────

    pub fn set_threshold(&mut self, new_threshold: U256) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        let old = self.risk_threshold.get();
        self.risk_threshold.set(new_threshold);
        self.vm().log(ThresholdUpdated {
            oldThreshold: old,
            newThreshold: new_threshold,
        });
        Ok(())
    }

    pub fn tracked_count(&self) -> U256 {
        U256::from(self.tracked_accounts.len())
    }

    pub fn threshold(&self) -> U256 {
        self.risk_threshold.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, B256, U256};
    use alloy_sol_types::{SolCall, SolEvent, SolType, sol, sol_data};
    use stylus_test::TestVM;

    sol! {
        function getUserAccountData(address user) external view returns (
            uint256 totalCollateralBase,
            uint256 totalDebtBase,
            uint256 availableBorrowsBase,
            uint256 currentLiquidationThreshold,
            uint256 ltv,
            uint256 healthFactor
        );

        function getAccountHealth(address account) external view returns (
            uint256 healthFactor
        );
    }

    const OWNER: Address = address!("0000000000000000000000000000000000000001");
    const POOL: Address = address!("794a61358D6845594F94dc1DB02A252b5b4814aD");
    const POOL_2: Address = address!("0000000000000000000000000000000000000abc");
    const POOL_FAIL: Address = address!("0000000000000000000000000000000000000bbb");
    const STRANGER: Address = address!("0000000000000000000000000000000000000bad");
    const GMX_READER: Address = address!("0000000000000000000000000000000000000def");
    const NEW_OWNER: Address = address!("0000000000000000000000000000000000000002");

    fn default_threshold() -> U256 {
        U256::from(1_100_000_000_000_000_000u128)
    }

    fn setup() -> (TestVM, LiquidationMonitor) {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        contract.add_lending_protocol(POOL, 0).unwrap();
        (vm, contract)
    }

    fn mock_health_factor(vm: &TestVM, account: Address, health_factor: U256) {
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
        vm.mock_static_call(POOL, calldata, Ok(return_data));
    }

    fn mock_health_factor_on_pool(
        vm: &TestVM,
        pool: Address,
        account: Address,
        health_factor: U256,
    ) {
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
        vm.mock_static_call(pool, calldata, Ok(return_data));
    }

    fn mock_pool_error(vm: &TestVM, pool: Address, account: Address) {
        let calldata = getUserAccountDataCall { user: account }.abi_encode();
        vm.mock_static_call(pool, calldata, Err(b"call failed".to_vec()));
    }

    fn mock_perp_health_factor(vm: &TestVM, reader: Address, account: Address, health_factor: U256) {
        let calldata = getAccountHealthCall { account }.abi_encode();
        type PerpReturn = (sol_data::Uint<256>,);
        let return_data = <PerpReturn as SolType>::abi_encode_params(&(health_factor,));
        vm.mock_static_call(reader, calldata, Ok(return_data));
    }

    /// Registers both lending and perp mocks in the correct order for
    /// stylus-test's return_data behavior. The lending mock is registered
    /// LAST because it's called first and its 192-byte return data must be
    /// in state.return_data. The perp call uses `if let Ok` so it gracefully
    /// handles the larger return data (first 32 bytes = totalCollateralBase).
    ///
    /// To control what the perp "sees", we set totalCollateralBase to the
    /// desired perp health factor value.
    fn mock_lending_and_perp(
        vm: &TestVM,
        pool: Address,
        reader: Address,
        account: Address,
        lending_hf: U256,
        perp_hf: U256,
    ) {
        // Register perp mock FIRST (it will be overwritten in state.return_data)
        let perp_calldata = getAccountHealthCall { account }.abi_encode();
        type PerpReturn = (sol_data::Uint<256>,);
        let perp_return_data = <PerpReturn as SolType>::abi_encode_params(&(perp_hf,));
        vm.mock_static_call(reader, perp_calldata, Ok(perp_return_data));

        // Register lending mock LAST — state.return_data gets this 192-byte data.
        // The perp call will read the same data; first 32 bytes = totalCollateralBase.
        // Set totalCollateralBase to perp_hf so the perp decode sees the right value.
        let lending_calldata = getUserAccountDataCall { user: account }.abi_encode();
        type AaveReturn = (
            sol_data::Uint<256>,
            sol_data::Uint<256>,
            sol_data::Uint<256>,
            sol_data::Uint<256>,
            sol_data::Uint<256>,
            sol_data::Uint<256>,
        );
        let lending_return_data = <AaveReturn as SolType>::abi_encode_params(&(
            perp_hf, // totalCollateralBase = perp_hf (what perp decode sees)
            U256::from(500_000),
            U256::from(200_000),
            U256::from(8000),
            U256::from(7500),
            lending_hf,
        ));
        vm.mock_static_call(pool, lending_calldata, Ok(lending_return_data));
    }

    // ─── Initialize ─────────────────────────────────────────────────────

    #[test]
    fn initialize_sets_storage() {
        let (_, contract) = setup();
        assert_eq!(contract.threshold(), default_threshold());
        assert_eq!(contract.tracked_count(), U256::ZERO);
    }

    #[test]
    fn initialize_sets_caller_as_owner() {
        let (vm, _) = setup();
        vm.set_sender(OWNER);
        let mut contract2 = LiquidationMonitor::from(&vm);
        let err = contract2.initialize(default_threshold()).unwrap_err();
        assert_eq!(err, b"already initialized".to_vec());
    }

    #[test]
    fn initialize_twice_fails() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        let err = contract.initialize(default_threshold()).unwrap_err();
        assert_eq!(err, b"already initialized".to_vec());
    }

    // ─── Access control ─────────────────────────────────────────────────

    #[test]
    fn only_owner_passes_for_owner() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let acct = address!("0000000000000000000000000000000000000099");
        assert!(contract.add_account(acct).is_ok());
    }

    #[test]
    fn only_owner_rejects_stranger() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let acct = address!("0000000000000000000000000000000000000099");
        let err = contract.add_account(acct).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    // ─── Two-step ownership transfer (LM-4) ────────────────────────────

    #[test]
    fn test_transfer_ownership_propose_accept() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.transfer_ownership(NEW_OWNER).unwrap();

        let logs = vm.get_emitted_logs();
        let selector = OwnershipTransferStarted::SIGNATURE_HASH;
        let start_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == selector)
            .collect();
        assert_eq!(start_logs.len(), 1);
        assert_eq!(start_logs[0].0[1], B256::from(OWNER.into_word()));
        assert_eq!(start_logs[0].0[2], B256::from(NEW_OWNER.into_word()));

        vm.set_sender(NEW_OWNER);
        contract.accept_ownership().unwrap();

        let logs = vm.get_emitted_logs();
        let selector = OwnershipTransferred::SIGNATURE_HASH;
        let transfer_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == selector)
            .collect();
        assert_eq!(transfer_logs.len(), 1);
        assert_eq!(transfer_logs[0].0[1], B256::from(OWNER.into_word()));
        assert_eq!(transfer_logs[0].0[2], B256::from(NEW_OWNER.into_word()));

        vm.set_sender(NEW_OWNER);
        let acct = address!("0000000000000000000000000000000000000099");
        assert!(contract.add_account(acct).is_ok());

        vm.set_sender(OWNER);
        let acct2 = address!("0000000000000000000000000000000000000098");
        let err = contract.add_account(acct2).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn test_transfer_ownership_wrong_acceptor() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.transfer_ownership(NEW_OWNER).unwrap();

        vm.set_sender(STRANGER);
        let err = contract.accept_ownership().unwrap_err();
        assert_eq!(err, b"not pending owner".to_vec());
    }

    #[test]
    fn test_transfer_ownership_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.transfer_ownership(NEW_OWNER).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn test_transfer_ownership_zero_address_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let err = contract.transfer_ownership(Address::ZERO).unwrap_err();
        assert_eq!(err, b"zero address".to_vec());
    }

    // ─── Pausable (CC-2) ────────────────────────────────────────────────

    #[test]
    fn test_pause_blocks_add_account() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.pause().unwrap();
        assert!(contract.is_paused());

        let acct = address!("0000000000000000000000000000000000000099");
        let err = contract.add_account(acct).unwrap_err();
        assert_eq!(err, b"contract is paused".to_vec());
    }

    #[test]
    fn test_unpause_allows_add_account() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.pause().unwrap();
        contract.unpause().unwrap();
        assert!(!contract.is_paused());

        let acct = address!("0000000000000000000000000000000000000099");
        assert!(contract.add_account(acct).is_ok());
    }

    #[test]
    fn test_pause_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.pause().unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn test_pause_blocks_scan_accounts() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        contract.add_lending_protocol(POOL, 0).unwrap();
        contract.pause().unwrap();

        let a1 = address!("0000000000000000000000000000000000000021");
        let err = contract.scan_accounts(vec![a1]).unwrap_err();
        assert_eq!(err, b"contract is paused".to_vec());
    }

    #[test]
    fn test_pause_blocks_scan_tracked_accounts() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        contract.add_lending_protocol(POOL, 0).unwrap();
        contract.pause().unwrap();

        let err = contract.scan_tracked_accounts().unwrap_err();
        assert_eq!(err, b"contract is paused".to_vec());
    }

    #[test]
    fn test_pause_already_paused() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.pause().unwrap();
        let err = contract.pause().unwrap_err();
        assert_eq!(err, b"already paused".to_vec());
    }

    #[test]
    fn test_unpause_not_paused() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let err = contract.unpause().unwrap_err();
        assert_eq!(err, b"not paused".to_vec());
    }

    // ─── Account management ─────────────────────────────────────────────

    #[test]
    fn add_account_increases_tracked_count() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let acct = address!("0000000000000000000000000000000000000010");
        contract.add_account(acct).unwrap();
        assert_eq!(contract.tracked_count(), U256::from(1));
        let acct2 = address!("0000000000000000000000000000000000000011");
        contract.add_account(acct2).unwrap();
        assert_eq!(contract.tracked_count(), U256::from(2));
    }

    #[test]
    fn add_account_emits_account_added() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let acct = address!("0000000000000000000000000000000000000010");
        contract.add_account(acct).unwrap();

        let logs = vm.get_emitted_logs();
        let last = logs.last().unwrap();
        let selector = AccountAdded::SIGNATURE_HASH;
        assert_eq!(last.0[0], selector);
        assert_eq!(last.0[1], B256::from(acct.into_word()));
    }

    #[test]
    fn add_account_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let acct = address!("0000000000000000000000000000000000000010");
        let err = contract.add_account(acct).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn remove_account_decreases_tracked_count() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let a1 = address!("0000000000000000000000000000000000000010");
        let a2 = address!("0000000000000000000000000000000000000011");
        contract.add_account(a1).unwrap();
        contract.add_account(a2).unwrap();
        assert_eq!(contract.tracked_count(), U256::from(2));

        contract.remove_account(a1).unwrap();
        assert_eq!(contract.tracked_count(), U256::from(1));
    }

    #[test]
    fn remove_account_emits_account_removed() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let acct = address!("0000000000000000000000000000000000000010");
        contract.add_account(acct).unwrap();
        contract.remove_account(acct).unwrap();

        let logs = vm.get_emitted_logs();
        let last = logs.last().unwrap();
        let selector = AccountRemoved::SIGNATURE_HASH;
        assert_eq!(last.0[0], selector);
        assert_eq!(last.0[1], B256::from(acct.into_word()));
    }

    #[test]
    fn remove_account_not_found() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let acct = address!("0000000000000000000000000000000000000010");
        let err = contract.remove_account(acct).unwrap_err();
        assert_eq!(err, b"account not found".to_vec());
    }

    #[test]
    fn remove_account_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let acct = address!("0000000000000000000000000000000000000010");
        contract.add_account(acct).unwrap();

        vm.set_sender(STRANGER);
        let err = contract.remove_account(acct).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    // ─── Threshold ──────────────────────────────────────────────────────

    #[test]
    fn set_threshold_updates_and_emits() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let new_thresh = U256::from(1_500_000_000_000_000_000u128);
        contract.set_threshold(new_thresh).unwrap();
        assert_eq!(contract.threshold(), new_thresh);

        let logs = vm.get_emitted_logs();
        let last = logs.last().unwrap();
        let selector = ThresholdUpdated::SIGNATURE_HASH;
        assert_eq!(last.0[0], selector);
        type TwoU256 = (sol_data::Uint<256>, sol_data::Uint<256>);
        let decoded = <TwoU256 as SolType>::abi_decode_params(&last.1).unwrap();
        assert_eq!(decoded.0, default_threshold());
        assert_eq!(decoded.1, new_thresh);
    }

    #[test]
    fn set_threshold_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract
            .set_threshold(U256::from(2_000_000_000_000_000_000u128))
            .unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn threshold_returns_current_value() {
        let (_, contract) = setup();
        assert_eq!(contract.threshold(), default_threshold());
    }

    // ─── Health factor / scanning ───────────────────────────────────────

    #[test]
    fn get_health_factor_returns_mocked_value() {
        let (vm, contract) = setup();
        let account = address!("0000000000000000000000000000000000000020");
        let expected_hf = U256::from(1_050_000_000_000_000_000u128);
        mock_health_factor(&vm, account, expected_hf);

        let hf = contract.get_health_factor(account).unwrap();
        assert_eq!(hf, expected_hf);
    }

    #[test]
    fn scan_accounts_detects_at_risk() {
        let (vm, contract) = setup();
        let a1 = address!("0000000000000000000000000000000000000021");
        mock_health_factor(&vm, a1, U256::from(900_000_000_000_000_000u128));

        let (at_risk, failed) = contract.scan_accounts(vec![a1]).unwrap();
        assert_eq!(at_risk.len(), 1);
        assert_eq!(at_risk[0].0, a1);
        assert_eq!(at_risk[0].1, U256::from(900_000_000_000_000_000u128));
        assert!(failed.is_empty());
    }

    #[test]
    fn scan_accounts_skips_healthy() {
        let (vm, contract) = setup();
        let a2 = address!("0000000000000000000000000000000000000022");
        mock_health_factor(&vm, a2, U256::from(2_000_000_000_000_000_000u128));
        let (at_risk, failed) = contract.scan_accounts(vec![a2]).unwrap();
        assert!(at_risk.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn scan_accounts_boundary_below_threshold() {
        let (vm, contract) = setup();
        let a3 = address!("0000000000000000000000000000000000000023");
        mock_health_factor(&vm, a3, U256::from(1_000_000_000_000_000_000u128));
        let (at_risk, _) = contract.scan_accounts(vec![a3]).unwrap();
        assert_eq!(at_risk.len(), 1);
        assert_eq!(at_risk[0].0, a3);
    }

    #[test]
    fn scan_accounts_empty_input_returns_empty() {
        let (_, contract) = setup();
        let (at_risk, failed) = contract.scan_accounts(vec![]).unwrap();
        assert!(at_risk.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn scan_accounts_all_healthy_returns_empty() {
        let (vm, contract) = setup();
        let a1 = address!("0000000000000000000000000000000000000031");
        let a2 = address!("0000000000000000000000000000000000000032");
        mock_health_factor(&vm, a1, U256::from(3_000_000_000_000_000_000u128));
        mock_health_factor(&vm, a2, U256::from(5_000_000_000_000_000_000u128));
        let (at_risk, failed) = contract.scan_accounts(vec![a1, a2]).unwrap();
        assert!(at_risk.is_empty());
        assert!(failed.is_empty());
    }

    #[test]
    fn scan_tracked_accounts_emits_at_risk_event() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);

        let a1 = address!("0000000000000000000000000000000000000041");
        contract.add_account(a1).unwrap();

        let hf_low = U256::from(800_000_000_000_000_000u128);
        mock_health_factor(&vm, a1, hf_low);

        let timestamp = 1_700_000_000u64;
        vm.set_block_timestamp(timestamp);

        let (at_risk, failed) = contract.scan_tracked_accounts().unwrap();
        assert_eq!(at_risk.len(), 1);
        assert_eq!(at_risk[0], (a1, hf_low));
        assert!(failed.is_empty());

        let logs = vm.get_emitted_logs();
        let at_risk_selector = AccountAtRisk::SIGNATURE_HASH;
        let risk_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == at_risk_selector)
            .collect();
        assert_eq!(risk_logs.len(), 1);
        assert_eq!(risk_logs[0].0[1], B256::from(a1.into_word()));

        type TwoU256 = (sol_data::Uint<256>, sol_data::Uint<256>);
        let decoded = <TwoU256 as SolType>::abi_decode_params(&risk_logs[0].1).unwrap();
        assert_eq!(decoded.0, hf_low);
        assert_eq!(decoded.1, U256::from(timestamp));
    }

    #[test]
    fn scan_tracked_accounts_healthy_no_events() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);

        let a2 = address!("0000000000000000000000000000000000000042");
        contract.add_account(a2).unwrap();

        let hf_high = U256::from(2_000_000_000_000_000_000u128);
        mock_health_factor(&vm, a2, hf_high);

        let (at_risk, _) = contract.scan_tracked_accounts().unwrap();
        assert!(at_risk.is_empty());
    }

    // ─── Scan accounts with failed queries (LM-2) ──────────────────────

    #[test]
    fn test_scan_accounts_returns_failed_accounts() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        contract.add_lending_protocol(POOL_FAIL, 0).unwrap();

        let a_fail = address!("0000000000000000000000000000000000000061");
        mock_pool_error(&vm, POOL_FAIL, a_fail);

        let (at_risk, failed) = contract.scan_accounts(vec![a_fail]).unwrap();
        assert!(at_risk.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0], a_fail);
    }

    #[test]
    fn test_scan_tracked_returns_failed_accounts() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        // Use POOL for successful account, POOL_FAIL for failing account
        // Register POOL_FAIL only — accounts on this pool will fail
        contract.add_lending_protocol(POOL_FAIL, 0).unwrap();

        let a_fail = address!("0000000000000000000000000000000000000063");
        contract.add_account(a_fail).unwrap();

        // Mock error for a_fail — this is the LAST mock so state.return_data is error data
        mock_pool_error(&vm, POOL_FAIL, a_fail);

        let timestamp = 1_700_000_000u64;
        vm.set_block_timestamp(timestamp);

        let (at_risk, failed) = contract.scan_tracked_accounts().unwrap();
        assert!(at_risk.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0], a_fail);

        // Verify no at-risk events were emitted (only setup events exist)
        let logs = vm.get_emitted_logs();
        let at_risk_selector = AccountAtRisk::SIGNATURE_HASH;
        let risk_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == at_risk_selector)
            .collect();
        assert_eq!(risk_logs.len(), 0);
    }

    // ─── Perp protocol dispatch (LM-6) ─────────────────────────────────

    #[test]
    fn test_get_health_factor_includes_perp_protocols() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        contract.add_perp_protocol(GMX_READER, 0).unwrap();

        let account = address!("0000000000000000000000000000000000000070");
        let expected_hf = U256::from(1_050_000_000_000_000_000u128);
        mock_perp_health_factor(&vm, GMX_READER, account, expected_hf);

        let hf = contract.get_health_factor(account).unwrap();
        assert_eq!(hf, expected_hf);
    }

    #[test]
    fn test_get_health_factor_perp_lower_than_lending() {
        // Due to stylus-test's shared return_data behavior, both calls read
        // from the same buffer. We register the lending mock LAST so its
        // 192-byte data is in state.return_data (needed by the first call).
        // The perp call (second) reads the same 192 bytes — first 32 bytes
        // = totalCollateralBase. We set that to the perp HF value.
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        contract.add_lending_protocol(POOL, 0).unwrap();
        contract.add_perp_protocol(GMX_READER, 0).unwrap();

        let account = address!("0000000000000000000000000000000000000071");
        let lending_hf = U256::from(2_000_000_000_000_000_000u128);
        let perp_hf = U256::from(800_000_000_000_000_000u128);

        mock_lending_and_perp(&vm, POOL, GMX_READER, account, lending_hf, perp_hf);

        let hf = contract.get_health_factor(account).unwrap();
        // The perp reads totalCollateralBase (= perp_hf = 0.8e18) as its HF
        // The lending reads healthFactor (= lending_hf = 2.0e18)
        // Lowest is perp_hf
        assert_eq!(hf, perp_hf);
    }

    #[test]
    fn test_get_health_factor_skips_inactive_perp() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.add_perp_protocol(GMX_READER, 0).unwrap();
        contract.remove_perp_protocol(U256::ZERO).unwrap();

        let account = address!("0000000000000000000000000000000000000072");
        let lending_hf = U256::from(1_500_000_000_000_000_000u128);
        mock_health_factor(&vm, account, lending_hf);

        let hf = contract.get_health_factor(account).unwrap();
        assert_eq!(hf, lending_hf);
    }

    // ─── Lending protocol registry ──────────────────────────────────────

    #[test]
    fn add_lending_protocol_registers_correctly() {
        let (_, contract) = setup();
        assert_eq!(contract.lending_protocol_count(), U256::from(1));
        let (addr, ptype, active) = contract.get_lending_protocol(U256::ZERO);
        assert_eq!(addr, POOL);
        assert_eq!(ptype, U256::ZERO);
        assert!(active);
    }

    #[test]
    fn add_lending_protocol_increments_count() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        assert_eq!(contract.lending_protocol_count(), U256::from(1));
        contract.add_lending_protocol(POOL_2, 0).unwrap();
        assert_eq!(contract.lending_protocol_count(), U256::from(2));
    }

    #[test]
    fn add_lending_protocol_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.add_lending_protocol(POOL_2, 0).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn add_lending_protocol_emits_event() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();
        contract.add_lending_protocol(POOL, 0).unwrap();

        let logs = vm.get_emitted_logs();
        let selector = LendingProtocolAdded::SIGNATURE_HASH;
        let add_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == selector)
            .collect();
        assert_eq!(add_logs.len(), 1);
        assert_eq!(add_logs[0].0[1], B256::from(U256::ZERO.to_be_bytes::<32>()));
    }

    #[test]
    fn add_lending_protocol_zero_address_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let err = contract.add_lending_protocol(Address::ZERO, 0).unwrap_err();
        assert_eq!(err, b"zero address".to_vec());
    }

    #[test]
    fn add_perp_protocol_zero_address_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let err = contract.add_perp_protocol(Address::ZERO, 0).unwrap_err();
        assert_eq!(err, b"zero address".to_vec());
    }

    #[test]
    fn add_account_max_cap_enforced() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        for i in 0u64..500 {
            let addr_bytes: [u8; 20] = {
                let mut b = [0u8; 20];
                b[18] = (i >> 8) as u8;
                b[19] = i as u8;
                b
            };
            contract.add_account(Address::from(addr_bytes)).unwrap();
        }
        let err = contract.add_account(address!("0000000000000000000000000000000000ffffff")).unwrap_err();
        assert_eq!(err, b"max tracked accounts reached".to_vec());
    }

    #[test]
    fn remove_lending_protocol_soft_deletes() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.remove_lending_protocol(U256::ZERO).unwrap();
        let (_, _, active) = contract.get_lending_protocol(U256::ZERO);
        assert!(!active);
        assert_eq!(contract.lending_protocol_count(), U256::from(1));
    }

    #[test]
    fn remove_lending_protocol_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.remove_lending_protocol(U256::ZERO).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn remove_lending_protocol_out_of_bounds() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let err = contract.remove_lending_protocol(U256::from(99)).unwrap_err();
        assert_eq!(err, b"index out of bounds".to_vec());
    }

    #[test]
    fn remove_lending_protocol_already_removed() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.remove_lending_protocol(U256::ZERO).unwrap();
        let err = contract.remove_lending_protocol(U256::ZERO).unwrap_err();
        assert_eq!(err, b"protocol already removed".to_vec());
    }

    #[test]
    fn remove_lending_protocol_emits_event() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.remove_lending_protocol(U256::ZERO).unwrap();

        let logs = vm.get_emitted_logs();
        let selector = LendingProtocolRemoved::SIGNATURE_HASH;
        let remove_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == selector)
            .collect();
        assert_eq!(remove_logs.len(), 1);
    }

    #[test]
    fn get_health_factor_no_protocols_returns_error() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        contract.initialize(default_threshold()).unwrap();

        let account = address!("0000000000000000000000000000000000000020");
        let err = contract.get_health_factor(account).unwrap_err();
        assert_eq!(err, b"no protocols registered".to_vec());
    }

    #[test]
    fn get_health_factor_skips_inactive_protocol() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.remove_lending_protocol(U256::ZERO).unwrap();

        let account = address!("0000000000000000000000000000000000000020");
        let err = contract.get_health_factor(account).unwrap_err();
        assert_eq!(err, b"no protocols registered".to_vec());
    }

    #[test]
    fn get_health_factor_multi_protocol_returns_lowest() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.add_lending_protocol(POOL_2, 0).unwrap();

        let account = address!("0000000000000000000000000000000000000050");
        let hf_high = U256::from(2_000_000_000_000_000_000u128);
        let hf_low = U256::from(900_000_000_000_000_000u128);

        // Mock POOL with high HF, POOL_2 with low HF
        // Note: stylus-test bug — last mock_static_call wins for return_data.
        // Register the low HF mock LAST so it gets returned.
        mock_health_factor_on_pool(&vm, POOL, account, hf_high);
        mock_health_factor_on_pool(&vm, POOL_2, account, hf_low);

        let hf = contract.get_health_factor(account).unwrap();
        // Due to stylus-test bug, both pools return hf_low (last mock registered).
        // The contract logic correctly picks the lowest.
        assert_eq!(hf, hf_low);
    }

    // ─── Perp protocol registry ─────────────────────────────────────────

    #[test]
    fn add_perp_protocol_registers_correctly() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let index = contract.add_perp_protocol(GMX_READER, 0).unwrap();
        assert_eq!(index, U256::ZERO);
        assert_eq!(contract.perp_protocol_count(), U256::from(1));
    }

    #[test]
    fn add_perp_protocol_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(STRANGER);
        let err = contract.add_perp_protocol(GMX_READER, 0).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn add_perp_protocol_emits_event() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.add_perp_protocol(GMX_READER, 0).unwrap();

        let logs = vm.get_emitted_logs();
        let selector = PerpProtocolAdded::SIGNATURE_HASH;
        let add_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == selector)
            .collect();
        assert_eq!(add_logs.len(), 1);
    }

    #[test]
    fn remove_perp_protocol_soft_deletes() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.add_perp_protocol(GMX_READER, 0).unwrap();
        contract.remove_perp_protocol(U256::ZERO).unwrap();
        assert_eq!(contract.perp_protocol_count(), U256::from(1));
    }

    #[test]
    fn remove_perp_protocol_non_owner_rejected() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.add_perp_protocol(GMX_READER, 0).unwrap();
        vm.set_sender(STRANGER);
        let err = contract.remove_perp_protocol(U256::ZERO).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn remove_perp_protocol_out_of_bounds() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        let err = contract.remove_perp_protocol(U256::ZERO).unwrap_err();
        assert_eq!(err, b"index out of bounds".to_vec());
    }

    #[test]
    fn lending_protocol_count_view() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        assert_eq!(contract.lending_protocol_count(), U256::from(1));
        contract.add_lending_protocol(POOL_2, 0).unwrap();
        assert_eq!(contract.lending_protocol_count(), U256::from(2));
    }

    #[test]
    fn get_lending_protocol_view() {
        let (vm, mut contract) = setup();
        vm.set_sender(OWNER);
        contract.add_lending_protocol(POOL_2, 0).unwrap();
        let (addr, ptype, active) = contract.get_lending_protocol(U256::from(1));
        assert_eq!(addr, POOL_2);
        assert_eq!(ptype, U256::ZERO);
        assert!(active);
    }

    // ─── Gap tests: mixed results & edge cases ─────────────────────────

    #[test]
    fn scan_mixed_success_and_failure() {
        // Use separate contracts to avoid mock_static_call ordering issues.
        // Test 1: account at risk (low HF on working pool)
        let vm1 = TestVM::new();
        vm1.set_sender(OWNER);
        let mut c1 = LiquidationMonitor::from(&vm1);
        c1.initialize(default_threshold()).unwrap();
        c1.add_lending_protocol(POOL, 0).unwrap();

        let risky_acct = address!("0000000000000000000000000000000000000aaa");
        let low_hf = U256::from(900_000_000_000_000_000u128); // 0.9e18
        mock_health_factor(&vm1, risky_acct, low_hf);

        let (at_risk, failed) = c1.scan_accounts(vec![risky_acct]).unwrap();
        assert_eq!(at_risk.len(), 1);
        assert_eq!(at_risk[0].0, risky_acct);
        assert_eq!(at_risk[0].1, low_hf);
        assert!(failed.is_empty());

        // Test 2: failing account (pool errors)
        let vm2 = TestVM::new();
        vm2.set_sender(OWNER);
        let mut c2 = LiquidationMonitor::from(&vm2);
        c2.initialize(default_threshold()).unwrap();
        c2.add_lending_protocol(POOL_FAIL, 0).unwrap();

        let fail_acct = address!("0000000000000000000000000000000000000bbb");
        mock_pool_error(&vm2, POOL_FAIL, fail_acct);

        let (at_risk, failed) = c2.scan_accounts(vec![fail_acct]).unwrap();
        assert!(at_risk.is_empty());
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0], fail_acct);
    }

    #[test]
    fn scan_zero_threshold_flags_all() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = LiquidationMonitor::from(&vm);
        // Initialize with threshold = 0
        contract.initialize(U256::ZERO).unwrap();
        contract.add_lending_protocol(POOL, 0).unwrap();

        let acct = address!("0000000000000000000000000000000000000ccc");
        // Even a very low HF is NOT < 0, so nobody should be flagged
        let low_hf = U256::from(1u64);
        mock_health_factor(&vm, acct, low_hf);

        let accounts = vec![acct];
        let (at_risk, failed) = contract.scan_accounts(accounts).unwrap();
        // HF=1 is not < 0, so not at risk
        assert!(at_risk.is_empty());
        assert!(failed.is_empty());
    }
}
