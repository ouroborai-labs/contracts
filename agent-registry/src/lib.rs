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

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use alloy_sol_types::sol;
use stylus_sdk::{
    alloy_primitives::{Address, U256},
    prelude::*,
};

// Capability flags (bitmask)
const CAP_TRADE: u64 = 1 << 0;
const CAP_PERPS: u64 = 1 << 1;
const CAP_LEND: u64 = 1 << 2;
const CAP_YIELD: u64 = 1 << 3;
const CAP_OPTIONS: u64 = 1 << 4;
const CAP_TIMEBOOST: u64 = 1 << 5;
const CAP_RWA: u64 = 1 << 6;

sol! {
    event AgentRegistered(address indexed owner, uint256 indexed agentId, uint64 capabilities);
    event AgentUpdated(uint256 indexed agentId, uint64 capabilities, uint16 revenueShareBps);
    event AgentDeactivated(uint256 indexed agentId);
    event ReputationUpdated(uint256 indexed agentId, uint256 newScore);
}

sol_storage! {
    #[entrypoint]
    pub struct AgentRegistry {
        uint256 next_id;
        address governance;
        mapping(uint256 => address) owners;
        mapping(uint256 => uint256) capabilities;
        mapping(uint256 => uint256) revenue_share_bps;
        mapping(uint256 => uint256) reputation;
        mapping(uint256 => bool) active;
    }
}

#[public]
impl AgentRegistry {
    pub fn register(&mut self, capabilities: u64, revenue_share_bps: u16) -> U256 {
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

        id
    }

    pub fn update(
        &mut self,
        agent_id: U256,
        capabilities: u64,
        revenue_share_bps: u16,
    ) -> bool {
        let caller = self.vm().msg_sender();
        let owner = self.owners.get(agent_id);
        if owner != caller {
            return false;
        }

        self.capabilities.setter(agent_id).set(U256::from(capabilities));
        self.revenue_share_bps.setter(agent_id).set(U256::from(revenue_share_bps));

        self.vm().log(AgentUpdated {
            agentId: agent_id,
            capabilities,
            revenueShareBps: revenue_share_bps,
        });

        true
    }

    pub fn deactivate(&mut self, agent_id: U256) -> bool {
        let caller = self.vm().msg_sender();
        let owner = self.owners.get(agent_id);
        let gov = self.governance.get();

        if caller != owner && caller != gov {
            return false;
        }

        self.active.setter(agent_id).set(false);
        self.vm().log(AgentDeactivated { agentId: agent_id });
        true
    }

    pub fn update_reputation(&mut self, agent_id: U256, new_score: U256) -> bool {
        let caller = self.vm().msg_sender();
        if caller != self.governance.get() {
            return false;
        }

        self.reputation.setter(agent_id).set(new_score);
        self.vm().log(ReputationUpdated {
            agentId: agent_id,
            newScore: new_score,
        });
        true
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

    pub fn set_governance(&mut self, new_governance: Address) -> bool {
        let caller = self.vm().msg_sender();
        let current = self.governance.get();
        if current != Address::ZERO && caller != current {
            return false;
        }
        self.governance.set(new_governance);
        true
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

    fn setup() -> (TestVM, AgentRegistry) {
        let vm = TestVM::new();
        let registry = AgentRegistry::from(&vm);
        (vm, registry)
    }

    #[test]
    fn test_register_agent() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);

        let caps = CAP_TRADE | CAP_PERPS | CAP_LEND;
        let id = registry.register(caps, 500);

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
        let id = registry.register(caps, 0);

        assert!(registry.has_capability(id, CAP_TRADE));
        assert!(registry.has_capability(id, CAP_TIMEBOOST));
        assert!(!registry.has_capability(id, CAP_PERPS));
        assert!(!registry.has_capability(id, CAP_RWA));
    }

    #[test]
    fn test_update_by_owner() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);

        let id = registry.register(CAP_TRADE, 500);
        let ok = registry.update(id, CAP_TRADE | CAP_PERPS | CAP_RWA, 750);
        assert!(ok);

        let (_, caps, rev, _, _) = registry.get_agent(id);
        assert_eq!(caps, U256::from(CAP_TRADE | CAP_PERPS | CAP_RWA));
        assert_eq!(rev, U256::from(750));
    }

    #[test]
    fn test_update_by_non_owner_fails() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 500);

        vm.set_sender(BOB);
        let ok = registry.update(id, CAP_PERPS, 0);
        assert!(!ok);
    }

    #[test]
    fn test_deactivate() {
        let (vm, mut registry) = setup();
        vm.set_sender(ALICE);

        let id = registry.register(CAP_TRADE, 0);
        assert!(registry.deactivate(id));

        let (_, _, _, _, active) = registry.get_agent(id);
        assert!(!active);
    }

    #[test]
    fn test_governance_reputation() {
        let (vm, mut registry) = setup();

        vm.set_sender(GOV);
        assert!(registry.set_governance(GOV));

        vm.set_sender(ALICE);
        let id = registry.register(CAP_TRADE, 0);

        // Non-governance can't update reputation
        vm.set_sender(ALICE);
        assert!(!registry.update_reputation(id, U256::from(100)));

        // Governance can
        vm.set_sender(GOV);
        assert!(registry.update_reputation(id, U256::from(100)));

        let (_, _, _, rep, _) = registry.get_agent(id);
        assert_eq!(rep, U256::from(100));
    }

    #[test]
    fn test_multiple_agents() {
        let (vm, mut registry) = setup();

        vm.set_sender(ALICE);
        let id0 = registry.register(CAP_TRADE | CAP_LEND, 500);

        vm.set_sender(BOB);
        let id1 = registry.register(CAP_PERPS | CAP_TIMEBOOST, 1000);

        assert_eq!(id0, U256::ZERO);
        assert_eq!(id1, U256::from(1));
        assert_eq!(registry.total_agents(), U256::from(2));

        let (owner0, _, _, _, _) = registry.get_agent(id0);
        let (owner1, _, _, _, _) = registry.get_agent(id1);
        assert_eq!(owner0, ALICE);
        assert_eq!(owner1, BOB);
    }
}
