//! Agent Registry — on-chain registry of ArbitrumAgent instances.
//!
//! Each agent is registered with:
//! - Owner address
//! - Capabilities bitmask (trade, perps, lend, yield, options, timeboost, rwa)
//! - Revenue share percentage (basis points)
//! - Reputation score (updated by governance or usage metrics)
//! - Active status
//!
//! Stored as a Stylus contract on Arbitrum for 10-100x gas savings vs Solidity.

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

// Capability flags (bitmask) — used by callers and tests
#[allow(dead_code)]
const CAP_TRADE: u64 = 1 << 0;
#[allow(dead_code)]
const CAP_PERPS: u64 = 1 << 1;
#[allow(dead_code)]
const CAP_LEND: u64 = 1 << 2;
#[allow(dead_code)]
const CAP_YIELD: u64 = 1 << 3;
#[allow(dead_code)]
const CAP_OPTIONS: u64 = 1 << 4;
#[allow(dead_code)]
const CAP_TIMEBOOST: u64 = 1 << 5;
#[allow(dead_code)]
const CAP_RWA: u64 = 1 << 6;

// AR-3: Valid capability bits mask (bits 0-6)
const VALID_CAPS_MASK: u64 = 0x7F;

sol! {
    event AgentRegistered(address indexed owner, uint256 indexed agentId, uint64 capabilities);
    event AgentUpdated(uint256 indexed agentId, uint64 capabilities, uint16 revenueShareBps);
    event AgentDeactivated(uint256 indexed agentId);
    event AgentReactivated(uint256 indexed agentId);
    event ReputationUpdated(uint256 indexed agentId, uint256 newScore);
    event GovernanceTransferStarted(address indexed current, address indexed pending);
    event GovernanceTransferred(address indexed previous, address indexed current);
    event Paused(address indexed by);
    event Unpaused(address indexed by);
}

sol_storage! {
    #[entrypoint]
    pub struct AgentRegistry {
        uint256 next_id;
        address governance;
        address pending_governance;
        bool paused;
        mapping(uint256 => address) owners;
        mapping(uint256 => uint256) capabilities;
        mapping(uint256 => uint256) revenue_share_bps;
        mapping(uint256 => uint256) reputation;
        mapping(uint256 => bool) active;
    }
}

// Private helpers in a separate impl block (Stylus SDK requirement)
impl AgentRegistry {
    fn require_governance(&self) -> Result<(), Vec<u8>> {
        if self.vm().msg_sender() != self.governance.get() {
            return Err(b"not governance".to_vec());
        }
        Ok(())
    }

    fn when_not_paused(&self) -> Result<(), Vec<u8>> {
        if self.paused.get() {
            return Err(b"paused".to_vec());
        }
        Ok(())
    }

    fn validate_capabilities(capabilities: u64) -> Result<(), Vec<u8>> {
        if capabilities & !VALID_CAPS_MASK != 0 {
            return Err(b"invalid capability bits".to_vec());
        }
        Ok(())
    }
}

#[public]
impl AgentRegistry {
    // AR-1: Dedicated initializer — can only be called once
    pub fn initialize(&mut self, governance: Address) -> Result<(), Vec<u8>> {
        if self.governance.get() != Address::ZERO {
            return Err(b"already initialized".to_vec());
        }
        if governance == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        self.governance.set(governance);
        Ok(())
    }

    pub fn register(&mut self, capabilities: u64, revenue_share_bps: u16) -> Result<U256, Vec<u8>> {
        self.when_not_paused()?;
        Self::validate_capabilities(capabilities)?;
        if revenue_share_bps > 10_000 {
            return Err(b"revenue_share_bps exceeds 10000".to_vec());
        }
        let caller = self.vm().msg_sender();
        let id = self.next_id.get();

        self.owners.setter(id).set(caller);
        self.capabilities.setter(id).set(U256::from(capabilities));
        self.revenue_share_bps.setter(id).set(U256::from(revenue_share_bps));
        self.reputation.setter(id).set(U256::ZERO);
        self.active.setter(id).set(true);

        self.next_id.set(id + U256::from(1));

        self.vm().log(AgentRegistered {
            owner: caller,
            agentId: id,
            capabilities,
        });

        Ok(id)
    }

    pub fn update(
        &mut self,
        agent_id: U256,
        capabilities: u64,
        revenue_share_bps: u16,
    ) -> Result<(), Vec<u8>> {
        self.when_not_paused()?;
        Self::validate_capabilities(capabilities)?;
        if revenue_share_bps > 10_000 {
            return Err(b"revenue_share_bps exceeds 10000".to_vec());
        }
        let caller = self.vm().msg_sender();
        let owner = self.owners.get(agent_id);
        if owner != caller {
            return Err(b"not owner".to_vec());
        }

        self.capabilities.setter(agent_id).set(U256::from(capabilities));
        self.revenue_share_bps.setter(agent_id).set(U256::from(revenue_share_bps));

        self.vm().log(AgentUpdated {
            agentId: agent_id,
            capabilities,
            revenueShareBps: revenue_share_bps,
        });

        Ok(())
    }

    // AR-4: Changed from bool to Result
    pub fn deactivate(&mut self, agent_id: U256) -> Result<(), Vec<u8>> {
        self.when_not_paused()?;
        let caller = self.vm().msg_sender();
        let owner = self.owners.get(agent_id);
        let gov = self.governance.get();

        if caller != owner && caller != gov {
            return Err(b"not authorized".to_vec());
        }

        self.active.setter(agent_id).set(false);
        self.vm().log(AgentDeactivated { agentId: agent_id });
        Ok(())
    }

    // AR-5: Reactivation by owner
    pub fn reactivate(&mut self, agent_id: U256) -> Result<(), Vec<u8>> {
        self.when_not_paused()?;
        let caller = self.vm().msg_sender();
        let owner = self.owners.get(agent_id);
        if owner == Address::ZERO {
            return Err(b"agent does not exist".to_vec());
        }
        if owner != caller {
            return Err(b"not owner".to_vec());
        }
        self.active.setter(agent_id).set(true);
        self.vm().log(AgentReactivated { agentId: agent_id });
        Ok(())
    }

    // AR-4: Changed from bool to Result
    pub fn update_reputation(&mut self, agent_id: U256, new_score: U256) -> Result<(), Vec<u8>> {
        self.require_governance()?;

        self.reputation.setter(agent_id).set(new_score);
        self.vm().log(ReputationUpdated {
            agentId: agent_id,
            newScore: new_score,
        });
        Ok(())
    }

    pub fn get_agent(
        &self,
        agent_id: U256,
    ) -> (Address, U256, U256, U256, bool) {
        (
            self.owners.get(agent_id),
            self.capabilities.get(agent_id),
            self.revenue_share_bps.get(agent_id),
            self.reputation.get(agent_id),
            self.active.get(agent_id),
        )
    }

    pub fn total_agents(&self) -> U256 {
        self.next_id.get()
    }

    pub fn has_capability(&self, agent_id: U256, cap_flag: u64) -> bool {
        let caps = self.capabilities.get(agent_id);
        let flag = U256::from(cap_flag);
        (caps & flag) == flag
    }

    // AR-1: Always requires caller == current governance (no ZERO bypass)
    // AR-4: Changed from bool to Result
    pub fn set_governance(&mut self, new_governance: Address) -> Result<(), Vec<u8>> {
        self.require_governance()?;
        if new_governance == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        self.governance.set(new_governance);
        Ok(())
    }

    // AR-6: Two-step governance transfer — step 1
    pub fn transfer_governance(&mut self, new_governance: Address) -> Result<(), Vec<u8>> {
        self.require_governance()?;
        if new_governance == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        let current = self.governance.get();
        self.pending_governance.set(new_governance);
        self.vm().log(GovernanceTransferStarted {
            current,
            pending: new_governance,
        });
        Ok(())
    }

    // AR-6: Two-step governance transfer — step 2
    pub fn accept_governance(&mut self) -> Result<(), Vec<u8>> {
        let caller = self.vm().msg_sender();
        let pending = self.pending_governance.get();
        if caller != pending {
            return Err(b"not pending governance".to_vec());
        }
        let previous = self.governance.get();
        self.governance.set(caller);
        self.pending_governance.set(Address::ZERO);
        self.vm().log(GovernanceTransferred {
            previous,
            current: caller,
        });
        Ok(())
    }

    // AR-6: Per-agent ownership transfer
    pub fn transfer_agent_ownership(&mut self, agent_id: U256, new_owner: Address) -> Result<(), Vec<u8>> {
        let caller = self.vm().msg_sender();
        let owner = self.owners.get(agent_id);
        if owner == Address::ZERO {
            return Err(b"agent does not exist".to_vec());
        }
        if owner != caller {
            return Err(b"not owner".to_vec());
        }
        if new_owner == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        self.owners.setter(agent_id).set(new_owner);
        Ok(())
    }

    // CC-2: Pausable — governance only
    pub fn pause(&mut self) -> Result<(), Vec<u8>> {
        self.require_governance()?;
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
        self.require_governance()?;
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

    pub fn get_governance(&self) -> Address {
        self.governance.get()
    }

    pub fn get_pending_governance(&self) -> Address {
        self.pending_governance.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, U256};
    use stylus_test::TestVM;

    const ALICE: Address = address!("aaa0000000000000000000000000000000000001");
    const BOB: Address = address!("bbb0000000000000000000000000000000000002");
    const GOV: Address = address!("aaa0000000000000000000000000000000000099");
    const GOV2: Address = address!("ccc0000000000000000000000000000000000003");

    fn setup() -> (TestVM, AgentRegistry) {
        let vm = TestVM::new();
        let registry = AgentRegistry::from(&vm);
        (vm, registry)
    }

    fn setup_with_governance() -> (TestVM, AgentRegistry) {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        registry.initialize(GOV).unwrap();
        (vm, registry)
    }

    // --- Existing tests (updated for new signatures) ---

    #[test]
    fn test_register_agent() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);

        let caps = CAP_TRADE | CAP_PERPS | CAP_LEND;
        let id = registry.register(caps, 500).unwrap();

        assert_eq!(id, U256::ZERO);
        assert_eq!(registry.total_agents(), U256::from(1));

        let (owner, reg_caps, rev_share, rep, active) = registry.get_agent(id);
        assert_eq!(owner, ALICE);
        assert_eq!(reg_caps, U256::from(caps));
        assert_eq!(rev_share, U256::from(500));
        assert_eq!(rep, U256::ZERO);
        assert!(active);
    }

    #[test]
    fn test_has_capability() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);

        let caps = CAP_TRADE | CAP_TIMEBOOST;
        let id = registry.register(caps, 0).unwrap();

        assert!(registry.has_capability(id, CAP_TRADE));
        assert!(registry.has_capability(id, CAP_TIMEBOOST));
        assert!(!registry.has_capability(id, CAP_PERPS));
        assert!(!registry.has_capability(id, CAP_RWA));
    }

    #[test]
    fn test_update_by_owner() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);

        let id = registry.register(CAP_TRADE, 500).unwrap();
        registry.update(id, CAP_TRADE | CAP_PERPS | CAP_RWA, 750).unwrap();

        let (_, caps, rev, _, _) = registry.get_agent(id);
        assert_eq!(caps, U256::from(CAP_TRADE | CAP_PERPS | CAP_RWA));
        assert_eq!(rev, U256::from(750));
    }

    #[test]
    fn test_update_by_non_owner_fails() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 500).unwrap();

        vm.set_sender(BOB);
        let err = registry.update(id, CAP_PERPS, 0).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn test_deactivate() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);

        let id = registry.register(CAP_TRADE, 0).unwrap();
        registry.deactivate(id).unwrap();

        let (_, _, _, _, active) = registry.get_agent(id);
        assert!(!active);
    }

    #[test]
    fn test_governance_reputation() {
        let (vm, mut registry) = setup_with_governance();

        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        // Non-governance can't update reputation
        vm.set_sender(ALICE);
        let err = registry.update_reputation(id, U256::from(100)).unwrap_err();
        assert_eq!(err, b"not governance".to_vec());

        // Governance can
        vm.set_sender(GOV);
        registry.update_reputation(id, U256::from(100)).unwrap();

        let (_, _, _, rep, _) = registry.get_agent(id);
        assert_eq!(rep, U256::from(100));
    }

    #[test]
    fn test_register_revenue_share_too_high() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let err = registry.register(CAP_TRADE, 10_001).unwrap_err();
        assert_eq!(err, b"revenue_share_bps exceeds 10000".to_vec());
    }

    #[test]
    fn test_register_revenue_share_at_max() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 10_000).unwrap();
        let (_, _, rev, _, _) = registry.get_agent(id);
        assert_eq!(rev, U256::from(10_000));
    }

    #[test]
    fn test_update_revenue_share_too_high() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 500).unwrap();
        let err = registry.update(id, CAP_TRADE, 10_001).unwrap_err();
        assert_eq!(err, b"revenue_share_bps exceeds 10000".to_vec());
    }

    #[test]
    fn test_multiple_agents() {
        let (vm, mut registry) = setup();

        vm.set_sender(ALICE);
        let id0 = registry.register(CAP_TRADE | CAP_LEND, 500).unwrap();

        vm.set_sender(BOB);
        let id1 = registry.register(CAP_PERPS | CAP_TIMEBOOST, 1000).unwrap();

        assert_eq!(id0, U256::ZERO);
        assert_eq!(id1, U256::from(1));
        assert_eq!(registry.total_agents(), U256::from(2));

        let (owner0, _, _, _, _) = registry.get_agent(id0);
        let (owner1, _, _, _, _) = registry.get_agent(id1);
        assert_eq!(owner0, ALICE);
        assert_eq!(owner1, BOB);
    }

    // --- AR-1: Initialize and governance ---

    #[test]
    fn test_initialize_sets_governance() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        registry.initialize(GOV).unwrap();
        assert_eq!(registry.get_governance(), GOV);
    }

    #[test]
    fn test_initialize_twice_fails() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        registry.initialize(GOV).unwrap();
        let err = registry.initialize(GOV2).unwrap_err();
        assert_eq!(err, b"already initialized".to_vec());
    }

    #[test]
    fn test_initialize_zero_address_fails() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let err = registry.initialize(Address::ZERO).unwrap_err();
        assert_eq!(err, b"zero address".to_vec());
    }

    #[test]
    fn test_set_governance_requires_governance() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let err = registry.set_governance(ALICE).unwrap_err();
        assert_eq!(err, b"not governance".to_vec());
    }

    #[test]
    fn test_set_governance_zero_bypass_removed() {
        let (vm, mut registry) = setup();
        // governance is Address::ZERO (uninitialized)
        // set_governance should fail because nobody can be Address::ZERO
        vm.set_sender(ALICE);
        let err = registry.set_governance(ALICE).unwrap_err();
        assert_eq!(err, b"not governance".to_vec());
    }

    #[test]
    fn test_set_governance_by_governance() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(GOV);
        registry.set_governance(GOV2).unwrap();
        assert_eq!(registry.get_governance(), GOV2);
    }

    #[test]
    fn test_set_governance_zero_target_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(GOV);
        let err = registry.set_governance(Address::ZERO).unwrap_err();
        assert_eq!(err, b"zero address".to_vec());
    }

    // --- AR-3: Capability bitmask validation ---

    #[test]
    fn test_register_invalid_capability_bits_rejected() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let invalid_caps = 1 << 7; // bit 7 is outside VALID_CAPS_MASK
        let err = registry.register(invalid_caps, 500).unwrap_err();
        assert_eq!(err, b"invalid capability bits".to_vec());
    }

    #[test]
    fn test_update_invalid_capability_bits_rejected() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 500).unwrap();
        let invalid_caps = CAP_TRADE | (1 << 10);
        let err = registry.update(id, invalid_caps, 500).unwrap_err();
        assert_eq!(err, b"invalid capability bits".to_vec());
    }

    #[test]
    fn test_register_all_valid_caps() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let all_caps = VALID_CAPS_MASK;
        let id = registry.register(all_caps, 0).unwrap();
        let (_, caps, _, _, _) = registry.get_agent(id);
        assert_eq!(caps, U256::from(all_caps));
    }

    // --- AR-4: Bool returns -> Result ---

    #[test]
    fn test_deactivate_returns_result() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();
        let result = registry.deactivate(id);
        assert!(result.is_ok());
    }

    #[test]
    fn test_deactivate_non_owner_returns_error() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        vm.set_sender(BOB);
        let err = registry.deactivate(id).unwrap_err();
        assert_eq!(err, b"not authorized".to_vec());
    }

    #[test]
    fn test_deactivate_by_governance() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        vm.set_sender(GOV);
        registry.deactivate(id).unwrap();
        let (_, _, _, _, active) = registry.get_agent(id);
        assert!(!active);
    }

    #[test]
    fn test_update_reputation_returns_result() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        vm.set_sender(GOV);
        let result = registry.update_reputation(id, U256::from(42));
        assert!(result.is_ok());
    }

    #[test]
    fn test_update_reputation_non_governance_returns_error() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        vm.set_sender(ALICE);
        let err = registry.update_reputation(id, U256::from(42)).unwrap_err();
        assert_eq!(err, b"not governance".to_vec());
    }

    // --- AR-5: Reactivation ---

    #[test]
    fn test_reactivate_by_owner() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();
        registry.deactivate(id).unwrap();

        let (_, _, _, _, active) = registry.get_agent(id);
        assert!(!active);

        registry.reactivate(id).unwrap();
        let (_, _, _, _, active) = registry.get_agent(id);
        assert!(active);
    }

    #[test]
    fn test_reactivate_by_non_owner_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();
        registry.deactivate(id).unwrap();

        vm.set_sender(BOB);
        let err = registry.reactivate(id).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn test_reactivate_nonexistent_agent() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let err = registry.reactivate(U256::from(999)).unwrap_err();
        assert_eq!(err, b"agent does not exist".to_vec());
    }

    // --- AR-6: Two-step governance transfer ---

    #[test]
    fn test_transfer_governance_two_step() {
        let (vm, mut registry) = setup_with_governance();

        vm.set_sender(GOV);
        registry.transfer_governance(GOV2).unwrap();
        assert_eq!(registry.get_pending_governance(), GOV2);
        assert_eq!(registry.get_governance(), GOV);

        vm.set_sender(GOV2);
        registry.accept_governance().unwrap();
        assert_eq!(registry.get_governance(), GOV2);
        assert_eq!(registry.get_pending_governance(), Address::ZERO);
    }

    #[test]
    fn test_transfer_governance_non_governance_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let err = registry.transfer_governance(GOV2).unwrap_err();
        assert_eq!(err, b"not governance".to_vec());
    }

    #[test]
    fn test_accept_governance_wrong_caller_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(GOV);
        registry.transfer_governance(GOV2).unwrap();

        vm.set_sender(ALICE);
        let err = registry.accept_governance().unwrap_err();
        assert_eq!(err, b"not pending governance".to_vec());
    }

    #[test]
    fn test_transfer_governance_zero_address_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(GOV);
        let err = registry.transfer_governance(Address::ZERO).unwrap_err();
        assert_eq!(err, b"zero address".to_vec());
    }

    // --- AR-6: Per-agent ownership transfer ---

    #[test]
    fn test_transfer_agent_ownership() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        registry.transfer_agent_ownership(id, BOB).unwrap();
        let (owner, _, _, _, _) = registry.get_agent(id);
        assert_eq!(owner, BOB);
    }

    #[test]
    fn test_transfer_agent_ownership_non_owner_rejected() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        vm.set_sender(BOB);
        let err = registry.transfer_agent_ownership(id, BOB).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn test_transfer_agent_ownership_zero_address_rejected() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();
        let err = registry.transfer_agent_ownership(id, Address::ZERO).unwrap_err();
        assert_eq!(err, b"zero address".to_vec());
    }

    #[test]
    fn test_transfer_agent_ownership_nonexistent_rejected() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let err = registry.transfer_agent_ownership(U256::from(999), BOB).unwrap_err();
        assert_eq!(err, b"agent does not exist".to_vec());
    }

    // --- CC-2: Pausable ---

    #[test]
    fn test_pause_blocks_register() {
        let (vm, mut registry) = setup_with_governance();

        vm.set_sender(GOV);
        registry.pause().unwrap();
        assert!(registry.is_paused());

        vm.set_sender(ALICE);
        let err = registry.register(CAP_TRADE, 0).unwrap_err();
        assert_eq!(err, b"paused".to_vec());
    }

    #[test]
    fn test_pause_blocks_update() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        vm.set_sender(GOV);
        registry.pause().unwrap();

        vm.set_sender(ALICE);
        let err = registry.update(id, CAP_TRADE | CAP_PERPS, 0).unwrap_err();
        assert_eq!(err, b"paused".to_vec());
    }

    #[test]
    fn test_pause_blocks_deactivate() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();

        vm.set_sender(GOV);
        registry.pause().unwrap();

        vm.set_sender(ALICE);
        let err = registry.deactivate(id).unwrap_err();
        assert_eq!(err, b"paused".to_vec());
    }

    #[test]
    fn test_pause_blocks_reactivate() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();
        registry.deactivate(id).unwrap();

        vm.set_sender(GOV);
        registry.pause().unwrap();

        vm.set_sender(ALICE);
        let err = registry.reactivate(id).unwrap_err();
        assert_eq!(err, b"paused".to_vec());
    }

    #[test]
    fn test_pause_non_governance_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(ALICE);
        let err = registry.pause().unwrap_err();
        assert_eq!(err, b"not governance".to_vec());
    }

    #[test]
    fn test_unpause() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(GOV);
        registry.pause().unwrap();
        assert!(registry.is_paused());

        registry.unpause().unwrap();
        assert!(!registry.is_paused());

        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0).unwrap();
        assert_eq!(id, U256::ZERO);
    }

    #[test]
    fn test_unpause_non_governance_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(GOV);
        registry.pause().unwrap();

        vm.set_sender(ALICE);
        let err = registry.unpause().unwrap_err();
        assert_eq!(err, b"not governance".to_vec());
    }

    #[test]
    fn test_pause_already_paused_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(GOV);
        registry.pause().unwrap();
        let err = registry.pause().unwrap_err();
        assert_eq!(err, b"already paused".to_vec());
    }

    #[test]
    fn test_unpause_not_paused_rejected() {
        let (vm, mut registry) = setup_with_governance();
        vm.set_sender(GOV);
        let err = registry.unpause().unwrap_err();
        assert_eq!(err, b"not paused".to_vec());
    }

    // ─── Gap test: full agent lifecycle ─────────────────────────────────

    #[test]
    fn test_register_update_deactivate_reactivate_lifecycle() {
        let (vm, mut registry) = setup_with_governance();

        // 1. Register
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 500).unwrap();
        let (owner, caps, rev, rep, active) = registry.get_agent(id);
        assert_eq!(owner, ALICE);
        assert_eq!(caps, U256::from(CAP_TRADE));
        assert_eq!(rev, U256::from(500));
        assert_eq!(rep, U256::ZERO);
        assert!(active);

        // 2. Update capabilities and revenue
        registry.update(id, CAP_TRADE | CAP_PERPS | CAP_RWA, 750).unwrap();
        let (_, caps, rev, _, active) = registry.get_agent(id);
        assert_eq!(caps, U256::from(CAP_TRADE | CAP_PERPS | CAP_RWA));
        assert_eq!(rev, U256::from(750));
        assert!(active);

        // 3. Governance updates reputation
        vm.set_sender(GOV);
        registry.update_reputation(id, U256::from(100u64)).unwrap();
        let (_, _, _, rep, _) = registry.get_agent(id);
        assert_eq!(rep, U256::from(100u64));

        // 4. Deactivate
        vm.set_sender(ALICE);
        registry.deactivate(id).unwrap();
        let (_, _, _, _, active) = registry.get_agent(id);
        assert!(!active);

        // 5. Reactivate
        registry.reactivate(id).unwrap();
        let (_, _, _, _, active) = registry.get_agent(id);
        assert!(active);

        // 6. Verify state persisted through lifecycle
        assert!(registry.has_capability(id, CAP_TRADE));
        assert!(registry.has_capability(id, CAP_PERPS));
        assert!(registry.has_capability(id, CAP_RWA));
        assert!(!registry.has_capability(id, CAP_YIELD));
    }
}
