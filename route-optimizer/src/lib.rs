//! Route Optimizer — Stylus (Rust) contract for Arbitrum One
//!
//! Evaluates multi-hop swap routes across registered DEXes to find
//! the best price with minimal price impact. Supports a dynamic
//! registry of DEX quoters/routers (Uniswap V3, Camelot, SushiSwap, etc.).
//!
//! 10-100x gas savings vs Solidity for multi-path computation.

#![cfg_attr(not(any(feature = "export-abi", test)), no_main)]
#![cfg_attr(not(any(feature = "export-abi", test)), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use alloy_sol_types::sol;
use stylus_sdk::{
    alloy_primitives::{Address, Uint, U256},
    prelude::*,
};

// ─── DEX type constants ─────────────────────────────────────────────────────

const DEX_TYPE_UNIV3: u64 = 0;
const DEX_TYPE_AMM_V2: u64 = 1;
const MAX_DEXES: u64 = 20;

// ─── DEX interfaces ─────────────────────────────────────────────────────────

sol_interface! {
    interface IUniswapV3Quoter {
        function quoteExactInput(bytes memory path, uint256 amountIn)
            external returns (uint256 amountOut);

        function quoteExactInputSingle(
            address tokenIn,
            address tokenOut,
            uint24 fee,
            uint256 amountIn,
            uint160 sqrtPriceLimitX96
        ) external returns (uint256 amountOut);
    }

    interface IAmmRouter {
        function getAmountsOut(uint256 amountIn, address[] calldata path)
            external view returns (uint256[] memory amounts);
    }
}

// ─── Events ─────────────────────────────────────────────────────────────────

sol! {
    event DexAdded(uint256 indexed index, address dexAddress, uint64 dexType);
    event DexRemoved(uint256 indexed index, address dexAddress);
}

// ─── Contract storage ───────────────────────────────────────────────────────

sol_storage! {
    #[entrypoint]
    pub struct RouteOptimizer {
        address owner;
        uint256 dex_count;
        mapping(uint256 => address) dex_addresses;
        mapping(uint256 => uint256) dex_types;
        mapping(uint256 => bool) dex_active;
        address[] routing_tokens;
    }
}

// ─── Implementation ─────────────────────────────────────────────────────────

#[public]
impl RouteOptimizer {
    pub fn initialize(&mut self) -> Result<(), Vec<u8>> {
        if self.owner.get() != Address::ZERO {
            return Err(b"already initialized".to_vec());
        }
        self.owner.set(self.vm().msg_sender());

        // Default routing tokens: WETH, USDC on Arbitrum One
        self.routing_tokens.push(
            "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1"
                .parse::<Address>()
                .unwrap(),
        );
        self.routing_tokens.push(
            "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
                .parse::<Address>()
                .unwrap(),
        );

        Ok(())
    }

    // ─── DEX registry ───────────────────────────────────────────────────

    pub fn add_dex(
        &mut self,
        dex_address: Address,
        dex_type: u64,
    ) -> Result<U256, Vec<u8>> {
        self.only_owner()?;
        if dex_address == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        let index = self.dex_count.get();
        if index >= U256::from(MAX_DEXES) {
            return Err(b"max dexes reached".to_vec());
        }
        self.dex_addresses.setter(index).set(dex_address);
        self.dex_types.setter(index).set(U256::from(dex_type));
        self.dex_active.setter(index).set(true);
        self.dex_count.set(index + U256::from(1));
        self.vm().log(DexAdded {
            index,
            dexAddress: dex_address,
            dexType: dex_type,
        });
        Ok(index)
    }

    pub fn remove_dex(&mut self, index: U256) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if index >= self.dex_count.get() {
            return Err(b"index out of bounds".to_vec());
        }
        if !self.dex_active.get(index) {
            return Err(b"dex already removed".to_vec());
        }
        self.dex_active.setter(index).set(false);
        let addr = self.dex_addresses.get(index);
        self.vm().log(DexRemoved {
            index,
            dexAddress: addr,
        });
        Ok(())
    }

    pub fn dex_count(&self) -> U256 {
        self.dex_count.get()
    }

    pub fn get_dex(&self, index: U256) -> (Address, U256, bool) {
        (
            self.dex_addresses.get(index),
            self.dex_types.get(index),
            self.dex_active.get(index),
        )
    }

    // ─── Routing token management ───────────────────────────────────────

    pub fn add_routing_token(&mut self, token: Address) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        if token == Address::ZERO {
            return Err(b"zero address".to_vec());
        }
        if self.routing_tokens.len() >= 50 {
            return Err(b"max routing tokens reached".to_vec());
        }
        for i in 0..self.routing_tokens.len() {
            if self.routing_tokens.get(i).unwrap() == token {
                return Err(b"token already exists".to_vec());
            }
        }
        self.routing_tokens.push(token);
        Ok(())
    }

    pub fn remove_routing_token(&mut self, token: Address) -> Result<(), Vec<u8>> {
        self.only_owner()?;
        let count = self.routing_tokens.len();
        for i in 0..count {
            if self.routing_tokens.get(i).unwrap() == token {
                let last = self.routing_tokens.get(count - 1).unwrap();
                self.routing_tokens.setter(i).unwrap().set(last);
                self.routing_tokens.pop();
                return Ok(());
            }
        }
        Err(b"token not found".to_vec())
    }

    pub fn routing_token_count(&self) -> U256 {
        U256::from(self.routing_tokens.len())
    }

    // ─── Route finding ──────────────────────────────────────────────────

    pub fn find_best_route(
        &mut self,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> Result<(U256, Vec<Address>, Vec<u32>), Vec<u8>> {
        let mut best_out = U256::ZERO;
        let mut best_tokens: Vec<Address> = Vec::new();
        let mut best_fees: Vec<u32> = Vec::new();
        let count = self.dex_count.get();

        for idx in 0..count.as_limbs()[0] {
            let index = U256::from(idx);
            if !self.dex_active.get(index) {
                continue;
            }

            let dex_addr = self.dex_addresses.get(index);
            let dex_type = self.dex_types.get(index).as_limbs()[0];

            match dex_type {
                DEX_TYPE_UNIV3 => {
                    self.quote_univ3(
                        dex_addr,
                        token_in,
                        token_out,
                        amount_in,
                        &mut best_out,
                        &mut best_tokens,
                        &mut best_fees,
                    );
                }
                DEX_TYPE_AMM_V2 => {
                    self.quote_amm_v2(
                        dex_addr,
                        token_in,
                        token_out,
                        amount_in,
                        &mut best_out,
                        &mut best_tokens,
                        &mut best_fees,
                    );
                }
                _ => { /* unknown type, skip */ }
            }
        }

        if best_out == U256::ZERO {
            return Err(b"no route found".to_vec());
        }

        Ok((best_out, best_tokens, best_fees))
    }

    pub fn compare_routes(
        &mut self,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> Result<(U256, U256), Vec<u8>> {
        let mut best_index = U256::ZERO;
        let mut best_out = U256::ZERO;
        let count = self.dex_count.get();

        for idx in 0..count.as_limbs()[0] {
            let index = U256::from(idx);
            if !self.dex_active.get(index) {
                continue;
            }

            let dex_addr = self.dex_addresses.get(index);
            let dex_type = self.dex_types.get(index).as_limbs()[0];

            let quote = match dex_type {
                DEX_TYPE_UNIV3 => self.best_univ3_direct(dex_addr, token_in, token_out, amount_in),
                DEX_TYPE_AMM_V2 => self.amm_direct_quote(dex_addr, token_in, token_out, amount_in),
                _ => U256::ZERO,
            };

            if quote > best_out {
                best_out = quote;
                best_index = index;
            }
        }

        Ok((best_index, best_out))
    }
}

// ─── Private helpers (not ABI-exported) ─────────────────────────────────────

impl RouteOptimizer {
    fn quote_univ3(
        &mut self,
        quoter_addr: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        best_out: &mut U256,
        best_tokens: &mut Vec<Address>,
        best_fees: &mut Vec<u32>,
    ) {
        let quoter = IUniswapV3Quoter::new(quoter_addr);
        let fee_tiers: [u32; 4] = [100, 500, 3000, 10000];

        // Direct single-hop routes
        for &fee in &fee_tiers {
            let vm = self.vm().clone();
            if let Ok(amount_out) = quoter.quote_exact_input_single(
                &vm,
                Call::new_mutating(self),
                token_in,
                token_out,
                Uint::<24, 1>::from(fee),
                amount_in,
                Uint::<160, 3>::ZERO,
            ) {
                if amount_out > *best_out {
                    *best_out = amount_out;
                    *best_tokens = vec![token_in, token_out];
                    *best_fees = vec![fee];
                }
            }
        }

        // Two-hop routes via intermediate tokens
        let routing_count = self.routing_tokens.len();
        for i in 0..routing_count {
            let intermediate = self.routing_tokens.get(i).unwrap();
            if intermediate == token_in || intermediate == token_out {
                continue;
            }

            for &fee1 in &fee_tiers {
                for &fee2 in &fee_tiers {
                    let path =
                        encode_path_two_hop(token_in, fee1, intermediate, fee2, token_out);

                    let vm = self.vm().clone();
                    if let Ok(amount_out) = quoter.quote_exact_input(
                        &vm,
                        Call::new_mutating(self),
                        path.into(),
                        amount_in,
                    ) {
                        if amount_out > *best_out {
                            *best_out = amount_out;
                            *best_tokens = vec![token_in, intermediate, token_out];
                            *best_fees = vec![fee1, fee2];
                        }
                    }
                }
            }
        }
    }

    fn quote_amm_v2(
        &mut self,
        router_addr: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        best_out: &mut U256,
        best_tokens: &mut Vec<Address>,
        best_fees: &mut Vec<u32>,
    ) {
        let router = IAmmRouter::new(router_addr);

        // Direct path
        let direct_path = vec![token_in, token_out];
        let vm = self.vm().clone();
        if let Ok(amounts) =
            router.get_amounts_out(&vm, Call::new(), amount_in, direct_path.clone())
        {
            if amounts.len() >= 2 {
                let out = amounts[amounts.len() - 1];
                if out > *best_out {
                    *best_out = out;
                    *best_tokens = direct_path;
                    *best_fees = vec![];
                }
            }
        }

        // Two-hop routes via intermediate tokens
        let routing_count = self.routing_tokens.len();
        for i in 0..routing_count {
            let intermediate = self.routing_tokens.get(i).unwrap();
            if intermediate == token_in || intermediate == token_out {
                continue;
            }

            let two_hop_path = vec![token_in, intermediate, token_out];
            let vm = self.vm().clone();
            if let Ok(amounts) =
                router.get_amounts_out(&vm, Call::new(), amount_in, two_hop_path.clone())
            {
                if amounts.len() >= 3 {
                    let out = amounts[amounts.len() - 1];
                    if out > *best_out {
                        *best_out = out;
                        *best_tokens = two_hop_path;
                        *best_fees = vec![];
                    }
                }
            }
        }
    }

    fn best_univ3_direct(
        &mut self,
        quoter_addr: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> U256 {
        let quoter = IUniswapV3Quoter::new(quoter_addr);
        let mut best = U256::ZERO;

        for &fee in &[100u32, 500, 3000, 10000] {
            let vm = self.vm().clone();
            if let Ok(out) = quoter.quote_exact_input_single(
                &vm,
                Call::new_mutating(self),
                token_in,
                token_out,
                Uint::<24, 1>::from(fee),
                amount_in,
                Uint::<160, 3>::ZERO,
            ) {
                if out > best {
                    best = out;
                }
            }
        }

        best
    }

    fn amm_direct_quote(
        &mut self,
        router_addr: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> U256 {
        let router = IAmmRouter::new(router_addr);
        let path = vec![token_in, token_out];
        let vm = self.vm().clone();
        if let Ok(amounts) = router.get_amounts_out(&vm, Call::new(), amount_in, path) {
            if amounts.len() >= 2 {
                return amounts[amounts.len() - 1];
            }
        }
        U256::ZERO
    }

    fn only_owner(&self) -> Result<(), Vec<u8>> {
        if self.vm().msg_sender() != self.owner.get() {
            return Err(b"not owner".to_vec());
        }
        Ok(())
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Encodes a Uniswap V3 two-hop path:
/// tokenA (20 bytes) + fee1 (3 bytes) + tokenB (20 bytes) + fee2 (3 bytes) + tokenC (20 bytes)
fn encode_path_two_hop(
    token_a: Address,
    fee1: u32,
    token_b: Address,
    fee2: u32,
    token_c: Address,
) -> Vec<u8> {
    let mut path = Vec::with_capacity(20 + 3 + 20 + 3 + 20);
    path.extend_from_slice(token_a.as_slice());
    path.extend_from_slice(&fee1.to_be_bytes()[1..]); // 3 bytes
    path.extend_from_slice(token_b.as_slice());
    path.extend_from_slice(&fee2.to_be_bytes()[1..]); // 3 bytes
    path.extend_from_slice(token_c.as_slice());
    path
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, B256, U256};
    use alloy_sol_types::{SolCall, SolEvent, SolType, sol, sol_data};
    use stylus_test::TestVM;

    const OWNER: Address = address!("0000000000000000000000000000000000000001");
    const STRANGER: Address = address!("0000000000000000000000000000000000000bad");
    const WETH: Address = address!("82aF49447D8a07e3bd95BD0d56f35241523fBab1");
    const USDC: Address = address!("af88d065e77c8cC2239327C5EDb3A432268e5831");
    const ARB: Address = address!("912CE59144191C1204E64559FE8253a0e49E6548");
    const DAI: Address = address!("DA10009cBd5D07dd0CeCc66161FC93D7c9000da1");
    const QUOTER: Address = address!("1111111111111111111111111111111111111111");
    const CAMELOT: Address = address!("2222222222222222222222222222222222222222");
    const SUSHI_QUOTER: Address = address!("3333333333333333333333333333333333333333");

    sol! {
        function quoteExactInputSingle(
            address tokenIn,
            address tokenOut,
            uint24 fee,
            uint256 amountIn,
            uint160 sqrtPriceLimitX96
        ) external returns (uint256 amountOut);

        function quoteExactInput(
            bytes path,
            uint256 amountIn
        ) external returns (uint256 amountOut);

        function getAmountsOut(
            uint256 amountIn,
            address[] path
        ) external view returns (uint256[] amounts);
    }

    fn setup_contract() -> (TestVM, RouteOptimizer) {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = RouteOptimizer::from(&vm);
        contract.initialize().unwrap();
        contract.add_dex(QUOTER, DEX_TYPE_UNIV3).unwrap();
        (vm, contract)
    }

    fn mock_single_quote(
        vm: &TestVM,
        quoter: Address,
        token_in: Address,
        token_out: Address,
        fee: u32,
        amount_in: U256,
        amount_out: U256,
    ) {
        let calldata = quoteExactInputSingleCall {
            tokenIn: token_in,
            tokenOut: token_out,
            fee: Uint::<24, 1>::from(fee),
            amountIn: amount_in,
            sqrtPriceLimitX96: Uint::<160, 3>::ZERO,
        }
        .abi_encode();
        let return_data = amount_out.to_be_bytes_vec();
        vm.mock_call(quoter, calldata, U256::ZERO, Ok(return_data));
    }

    fn mock_single_quote_revert(
        vm: &TestVM,
        quoter: Address,
        token_in: Address,
        token_out: Address,
        fee: u32,
        amount_in: U256,
    ) {
        let calldata = quoteExactInputSingleCall {
            tokenIn: token_in,
            tokenOut: token_out,
            fee: Uint::<24, 1>::from(fee),
            amountIn: amount_in,
            sqrtPriceLimitX96: Uint::<160, 3>::ZERO,
        }
        .abi_encode();
        vm.mock_call(quoter, calldata, U256::ZERO, Err(b"no liquidity".to_vec()));
    }

    fn mock_multi_quote(
        vm: &TestVM,
        quoter: Address,
        path: Vec<u8>,
        amount_in: U256,
        amount_out: U256,
    ) {
        let calldata = quoteExactInputCall {
            path: path.into(),
            amountIn: amount_in,
        }
        .abi_encode();
        let return_data = amount_out.to_be_bytes_vec();
        vm.mock_call(quoter, calldata, U256::ZERO, Ok(return_data));
    }

    fn mock_amm_direct(
        vm: &TestVM,
        router: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        amount_out: U256,
    ) {
        let calldata = getAmountsOutCall {
            amountIn: amount_in,
            path: vec![token_in, token_out],
        }
        .abi_encode();
        type AmountsReturn = sol_data::Array<sol_data::Uint<256>>;
        let return_data =
            <AmountsReturn as SolType>::abi_encode_params(&vec![amount_in, amount_out]);
        vm.mock_static_call(router, calldata, Ok(return_data));
    }

    fn mock_amm_two_hop(
        vm: &TestVM,
        router: Address,
        token_in: Address,
        intermediate: Address,
        token_out: Address,
        amount_in: U256,
        amount_out: U256,
    ) {
        let calldata = getAmountsOutCall {
            amountIn: amount_in,
            path: vec![token_in, intermediate, token_out],
        }
        .abi_encode();
        let mid_amount = (amount_in + amount_out) / U256::from(2);
        type AmountsReturn = sol_data::Array<sol_data::Uint<256>>;
        let return_data =
            <AmountsReturn as SolType>::abi_encode_params(&vec![amount_in, mid_amount, amount_out]);
        vm.mock_static_call(router, calldata, Ok(return_data));
    }

    fn mock_amm_direct_revert(
        vm: &TestVM,
        router: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) {
        let calldata = getAmountsOutCall {
            amountIn: amount_in,
            path: vec![token_in, token_out],
        }
        .abi_encode();
        vm.mock_static_call(router, calldata, Err(b"no liq".to_vec()));
    }

    // ─── encode_path_two_hop tests ──────────────────────────────────────

    #[test]
    fn encode_path_two_hop_total_length() {
        let path = encode_path_two_hop(WETH, 3000, USDC, 500, ARB);
        assert_eq!(path.len(), 66);
    }

    #[test]
    fn encode_path_two_hop_fee_3000_encoding() {
        let path = encode_path_two_hop(WETH, 3000, USDC, 100, ARB);
        assert_eq!(&path[20..23], &[0x00, 0x0B, 0xB8]);
    }

    #[test]
    fn encode_path_two_hop_fee_100_encoding() {
        let path = encode_path_two_hop(WETH, 100, USDC, 3000, ARB);
        assert_eq!(&path[20..23], &[0x00, 0x00, 0x64]);
    }

    #[test]
    fn encode_path_two_hop_fee_10000_encoding() {
        let path = encode_path_two_hop(WETH, 10000, USDC, 500, ARB);
        assert_eq!(&path[20..23], &[0x00, 0x27, 0x10]);
    }

    #[test]
    fn encode_path_two_hop_fee_500_at_second_position() {
        let path = encode_path_two_hop(WETH, 3000, USDC, 500, ARB);
        assert_eq!(&path[43..46], &[0x00, 0x01, 0xF4]);
    }

    #[test]
    fn encode_path_two_hop_address_offsets() {
        let path = encode_path_two_hop(WETH, 3000, USDC, 500, ARB);
        assert_eq!(&path[0..20], WETH.as_slice());
        assert_eq!(&path[23..43], USDC.as_slice());
        assert_eq!(&path[46..66], ARB.as_slice());
    }

    #[test]
    fn encode_path_two_hop_roundtrip_identical_fees() {
        let path = encode_path_two_hop(ARB, 3000, WETH, 3000, USDC);
        assert_eq!(&path[20..23], &path[43..46]);
    }

    // ─── initialize tests ───────────────────────────────────────────────

    #[test]
    fn initialize_sets_owner_and_defaults() {
        let (_, contract) = setup_contract();
        assert_eq!(contract.routing_token_count(), U256::from(2));
        assert_eq!(contract.dex_count(), U256::from(1));
    }

    #[test]
    fn initialize_reinit_guard() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        let err = contract.initialize().unwrap_err();
        assert_eq!(err, b"already initialized".to_vec());
    }

    // ─── DEX registry tests ────────────────────────────────────────────

    #[test]
    fn add_dex_registers_correctly() {
        let (_, contract) = setup_contract();
        let (addr, dtype, active) = contract.get_dex(U256::ZERO);
        assert_eq!(addr, QUOTER);
        assert_eq!(dtype, U256::ZERO);
        assert!(active);
    }

    #[test]
    fn add_dex_increments_count() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        assert_eq!(contract.dex_count(), U256::from(1));
        contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap();
        assert_eq!(contract.dex_count(), U256::from(2));
    }

    #[test]
    fn add_dex_non_owner_rejected() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(STRANGER);
        let err = contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn add_dex_emits_event() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = RouteOptimizer::from(&vm);
        contract.initialize().unwrap();
        contract.add_dex(QUOTER, DEX_TYPE_UNIV3).unwrap();

        let logs = vm.get_emitted_logs();
        let selector = DexAdded::SIGNATURE_HASH;
        let add_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == selector)
            .collect();
        assert_eq!(add_logs.len(), 1);
    }

    #[test]
    fn remove_dex_soft_deletes() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.remove_dex(U256::ZERO).unwrap();
        let (_, _, active) = contract.get_dex(U256::ZERO);
        assert!(!active);
        assert_eq!(contract.dex_count(), U256::from(1));
    }

    #[test]
    fn remove_dex_non_owner_rejected() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(STRANGER);
        let err = contract.remove_dex(U256::ZERO).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn remove_dex_out_of_bounds() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        let err = contract.remove_dex(U256::from(99)).unwrap_err();
        assert_eq!(err, b"index out of bounds".to_vec());
    }

    #[test]
    fn remove_dex_already_removed() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.remove_dex(U256::ZERO).unwrap();
        let err = contract.remove_dex(U256::ZERO).unwrap_err();
        assert_eq!(err, b"dex already removed".to_vec());
    }

    #[test]
    fn remove_dex_emits_event() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.remove_dex(U256::ZERO).unwrap();

        let logs = vm.get_emitted_logs();
        let selector = DexRemoved::SIGNATURE_HASH;
        let remove_logs: Vec<_> = logs
            .iter()
            .filter(|(topics, _)| !topics.is_empty() && topics[0] == selector)
            .collect();
        assert_eq!(remove_logs.len(), 1);
    }

    // ─── Routing token tests ────────────────────────────────────────────

    #[test]
    fn add_routing_token_adds_to_list() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        let token = address!("0000000000000000000000000000000000000abc");
        contract.add_routing_token(token).unwrap();
        assert_eq!(contract.routing_token_count(), U256::from(3));
    }

    #[test]
    fn add_routing_token_duplicate_rejected() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        let err = contract.add_routing_token(WETH).unwrap_err();
        assert_eq!(err, b"token already exists".to_vec());
    }

    #[test]
    fn add_routing_token_non_owner_rejected() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(STRANGER);
        let token = address!("0000000000000000000000000000000000000abc");
        let err = contract.add_routing_token(token).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    #[test]
    fn remove_routing_token_removes() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.remove_routing_token(WETH).unwrap();
        assert_eq!(contract.routing_token_count(), U256::from(1));
    }

    #[test]
    fn remove_routing_token_not_found() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        let token = address!("0000000000000000000000000000000000000abc");
        let err = contract.remove_routing_token(token).unwrap_err();
        assert_eq!(err, b"token not found".to_vec());
    }

    #[test]
    fn remove_routing_token_non_owner_rejected() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(STRANGER);
        let err = contract.remove_routing_token(WETH).unwrap_err();
        assert_eq!(err, b"not owner".to_vec());
    }

    // ─── find_best_route tests (UniV3) ──────────────────────────────────

    #[test]
    fn find_best_route_single_hop_picks_best_fee_tier() {
        let (vm, mut contract) = setup_contract();
        let amount_in = U256::from(1_000_000_000_000_000_000u128);

        // Register losing single-hop fee tiers as reverts
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 100, amount_in);
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 3000, amount_in);
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 10000, amount_in);

        // Register all two-hop routes as reverts
        let fee_tiers = [100u32, 500, 3000, 10000];
        for &f1 in &fee_tiers {
            for &f2 in &fee_tiers {
                let path_weth = encode_path_two_hop(ARB, f1, WETH, f2, DAI);
                let cd = quoteExactInputCall {
                    path: path_weth.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd, U256::ZERO, Err(b"no liq".to_vec()));

                let path_usdc = encode_path_two_hop(ARB, f1, USDC, f2, DAI);
                let cd2 = quoteExactInputCall {
                    path: path_usdc.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd2, U256::ZERO, Err(b"no liq".to_vec()));
            }
        }

        // Register winning Ok mock LAST
        mock_single_quote(&vm, QUOTER, ARB, DAI, 500, amount_in, U256::from(1_000_000u64));

        let (best_out, best_tokens, best_fees) =
            contract.find_best_route(ARB, DAI, amount_in).unwrap();
        assert_eq!(best_out, U256::from(1_000_000u64));
        assert_eq!(best_tokens, vec![ARB, DAI]);
        assert_eq!(best_fees, vec![500]);
    }

    #[test]
    fn find_best_route_two_hop_via_weth_wins() {
        let (vm, mut contract) = setup_contract();
        let amount_in = U256::from(1_000_000u64);

        // All single-hop reverts
        for &fee in &[100u32, 500, 3000, 10000] {
            mock_single_quote_revert(&vm, QUOTER, ARB, DAI, fee, amount_in);
        }

        // All two-hop routes revert except the winner
        let fee_tiers = [100u32, 500, 3000, 10000];
        for &f1 in &fee_tiers {
            for &f2 in &fee_tiers {
                if f1 == 3000 && f2 == 500 {
                    continue;
                }
                let path_weth = encode_path_two_hop(ARB, f1, WETH, f2, DAI);
                let cd = quoteExactInputCall {
                    path: path_weth.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd, U256::ZERO, Err(b"no liq".to_vec()));

                let path_usdc = encode_path_two_hop(ARB, f1, USDC, f2, DAI);
                let cd2 = quoteExactInputCall {
                    path: path_usdc.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd2, U256::ZERO, Err(b"no liq".to_vec()));
            }
        }

        // USDC path for the winner combo as revert
        let path_usdc_winner = encode_path_two_hop(ARB, 3000, USDC, 500, DAI);
        let cd_usdc = quoteExactInputCall {
            path: path_usdc_winner.into(),
            amountIn: amount_in,
        }
        .abi_encode();
        vm.mock_call(QUOTER, cd_usdc, U256::ZERO, Err(b"no liq".to_vec()));

        // Register winning two-hop LAST
        let path_weth_winner = encode_path_two_hop(ARB, 3000, WETH, 500, DAI);
        mock_multi_quote(&vm, QUOTER, path_weth_winner, amount_in, U256::from(1200u64));

        let (best_out, best_tokens, best_fees) =
            contract.find_best_route(ARB, DAI, amount_in).unwrap();
        assert_eq!(best_out, U256::from(1200u64));
        assert_eq!(best_tokens, vec![ARB, WETH, DAI]);
        assert_eq!(best_fees, vec![3000, 500]);
    }

    #[test]
    fn find_best_route_no_liquidity_returns_error() {
        let (vm, mut contract) = setup_contract();
        let amount_in = U256::from(1_000_000u64);

        for &fee in &[100u32, 500, 3000, 10000] {
            mock_single_quote_revert(&vm, QUOTER, ARB, DAI, fee, amount_in);
        }

        let fee_tiers = [100u32, 500, 3000, 10000];
        for &f1 in &fee_tiers {
            for &f2 in &fee_tiers {
                let path_weth = encode_path_two_hop(ARB, f1, WETH, f2, DAI);
                let cd = quoteExactInputCall {
                    path: path_weth.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd, U256::ZERO, Err(b"no liq".to_vec()));

                let path_usdc = encode_path_two_hop(ARB, f1, USDC, f2, DAI);
                let cd2 = quoteExactInputCall {
                    path: path_usdc.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd2, U256::ZERO, Err(b"no liq".to_vec()));
            }
        }

        let err = contract.find_best_route(ARB, DAI, amount_in).unwrap_err();
        assert_eq!(err, b"no route found".to_vec());
    }

    // ─── find_best_route: intermediate token skip ───────────────────────

    #[test]
    fn find_best_route_skips_intermediate_equal_to_token_in() {
        let (vm, mut contract) = setup_contract();
        let amount_in = U256::from(500_000u64);
        let uni_out = U256::from(499_000u64);

        // All two-hop USDC routes revert
        let fee_tiers = [100u32, 500, 3000, 10000];
        for &f1 in &fee_tiers {
            for &f2 in &fee_tiers {
                let path_usdc = encode_path_two_hop(WETH, f1, USDC, f2, DAI);
                let cd = quoteExactInputCall {
                    path: path_usdc.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd, U256::ZERO, Err(b"no liq".to_vec()));
            }
        }

        mock_single_quote_revert(&vm, QUOTER, WETH, DAI, 100, amount_in);
        mock_single_quote_revert(&vm, QUOTER, WETH, DAI, 3000, amount_in);
        mock_single_quote_revert(&vm, QUOTER, WETH, DAI, 10000, amount_in);
        mock_single_quote(&vm, QUOTER, WETH, DAI, 500, amount_in, uni_out);

        let (best_out, best_tokens, best_fees) =
            contract.find_best_route(WETH, DAI, amount_in).unwrap();
        assert_eq!(best_out, uni_out);
        assert_eq!(best_tokens, vec![WETH, DAI]);
        assert_eq!(best_fees, vec![500]);
    }

    #[test]
    fn find_best_route_skips_intermediate_equal_to_token_out() {
        let (vm, mut contract) = setup_contract();
        let amount_in = U256::from(500_000u64);
        let uni_out = U256::from(499_500u64);

        let fee_tiers = [100u32, 500, 3000, 10000];
        for &f1 in &fee_tiers {
            for &f2 in &fee_tiers {
                let path_usdc = encode_path_two_hop(ARB, f1, USDC, f2, WETH);
                let cd = quoteExactInputCall {
                    path: path_usdc.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd, U256::ZERO, Err(b"no liq".to_vec()));
            }
        }

        mock_single_quote_revert(&vm, QUOTER, ARB, WETH, 100, amount_in);
        mock_single_quote_revert(&vm, QUOTER, ARB, WETH, 3000, amount_in);
        mock_single_quote_revert(&vm, QUOTER, ARB, WETH, 10000, amount_in);
        mock_single_quote(&vm, QUOTER, ARB, WETH, 500, amount_in, uni_out);

        let (best_out, best_tokens, _) = contract.find_best_route(ARB, WETH, amount_in).unwrap();
        assert_eq!(best_out, uni_out);
        assert_eq!(best_tokens, vec![ARB, WETH]);
    }

    // ─── find_best_route: skips inactive DEX ────────────────────────────

    #[test]
    fn find_best_route_skips_inactive_dex() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.remove_dex(U256::ZERO).unwrap();

        let amount_in = U256::from(1_000u64);
        let err = contract.find_best_route(ARB, DAI, amount_in).unwrap_err();
        assert_eq!(err, b"no route found".to_vec());
    }

    // ─── find_best_route: AMM V2 ────────────────────────────────────────

    #[test]
    fn find_best_route_amm_v2_direct() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = RouteOptimizer::from(&vm);
        contract.initialize().unwrap();
        contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap();

        let amount_in = U256::from(1_000_000u64);
        let amount_out = U256::from(997_000u64);

        // Mock AMM direct and two-hop routes
        // Two-hop reverts
        mock_amm_direct_revert(&vm, CAMELOT, ARB, WETH, amount_in);
        let calldata_weth = getAmountsOutCall {
            amountIn: amount_in,
            path: vec![ARB, WETH, DAI],
        }
        .abi_encode();
        vm.mock_static_call(CAMELOT, calldata_weth, Err(b"no liq".to_vec()));

        let calldata_usdc = getAmountsOutCall {
            amountIn: amount_in,
            path: vec![ARB, USDC, DAI],
        }
        .abi_encode();
        vm.mock_static_call(CAMELOT, calldata_usdc, Err(b"no liq".to_vec()));

        // Direct route succeeds (register LAST due to stylus-test bug)
        mock_amm_direct(&vm, CAMELOT, ARB, DAI, amount_in, amount_out);

        let (best_out, best_tokens, best_fees) =
            contract.find_best_route(ARB, DAI, amount_in).unwrap();
        assert_eq!(best_out, amount_out);
        assert_eq!(best_tokens, vec![ARB, DAI]);
        assert!(best_fees.is_empty());
    }

    #[test]
    fn find_best_route_amm_v2_two_hop() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = RouteOptimizer::from(&vm);
        contract.initialize().unwrap();
        contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap();

        let amount_in = U256::from(1_000_000u64);
        let two_hop_out = U256::from(998_000u64);

        // Direct reverts
        mock_amm_direct_revert(&vm, CAMELOT, ARB, DAI, amount_in);

        // USDC two-hop reverts
        let calldata_usdc = getAmountsOutCall {
            amountIn: amount_in,
            path: vec![ARB, USDC, DAI],
        }
        .abi_encode();
        vm.mock_static_call(CAMELOT, calldata_usdc, Err(b"no liq".to_vec()));

        // WETH two-hop succeeds (register LAST)
        mock_amm_two_hop(&vm, CAMELOT, ARB, WETH, DAI, amount_in, two_hop_out);

        let (best_out, best_tokens, best_fees) =
            contract.find_best_route(ARB, DAI, amount_in).unwrap();
        assert_eq!(best_out, two_hop_out);
        assert_eq!(best_tokens, vec![ARB, WETH, DAI]);
        assert!(best_fees.is_empty());
    }

    // ─── find_best_route: multi-DEX ─────────────────────────────────────

    #[test]
    fn find_best_route_multi_dex_picks_best() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap();

        let amount_in = U256::from(1_000_000u64);
        let uni_out = U256::from(990_000u64);
        let amm_out = U256::from(997_000u64);

        // UniV3: all routes revert except one with lower output
        for &fee in &[100u32, 3000, 10000] {
            mock_single_quote_revert(&vm, QUOTER, ARB, DAI, fee, amount_in);
        }
        let fee_tiers = [100u32, 500, 3000, 10000];
        for &f1 in &fee_tiers {
            for &f2 in &fee_tiers {
                let path_weth = encode_path_two_hop(ARB, f1, WETH, f2, DAI);
                let cd = quoteExactInputCall {
                    path: path_weth.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd, U256::ZERO, Err(b"no liq".to_vec()));

                let path_usdc = encode_path_two_hop(ARB, f1, USDC, f2, DAI);
                let cd2 = quoteExactInputCall {
                    path: path_usdc.into(),
                    amountIn: amount_in,
                }
                .abi_encode();
                vm.mock_call(QUOTER, cd2, U256::ZERO, Err(b"no liq".to_vec()));
            }
        }
        mock_single_quote(&vm, QUOTER, ARB, DAI, 500, amount_in, uni_out);

        // AMM: two-hop reverts, direct succeeds with better output
        let calldata_weth = getAmountsOutCall {
            amountIn: amount_in,
            path: vec![ARB, WETH, DAI],
        }
        .abi_encode();
        vm.mock_static_call(CAMELOT, calldata_weth, Err(b"no liq".to_vec()));
        let calldata_usdc = getAmountsOutCall {
            amountIn: amount_in,
            path: vec![ARB, USDC, DAI],
        }
        .abi_encode();
        vm.mock_static_call(CAMELOT, calldata_usdc, Err(b"no liq".to_vec()));
        mock_amm_direct(&vm, CAMELOT, ARB, DAI, amount_in, amm_out);

        let (best_out, best_tokens, best_fees) =
            contract.find_best_route(ARB, DAI, amount_in).unwrap();
        assert_eq!(best_out, amm_out);
        assert_eq!(best_tokens, vec![ARB, DAI]);
        assert!(best_fees.is_empty());
    }

    // ─── compare_routes tests ───────────────────────────────────────────

    #[test]
    fn compare_routes_univ3_wins() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap();

        let amount_in = U256::from(1_000_000_000u64);
        let uni_out = U256::from(998_000_000u64);
        let amm_out = U256::from(997_000_000u64);

        // AMM mock (register first)
        mock_amm_direct(&vm, CAMELOT, ARB, DAI, amount_in, amm_out);

        // UniV3 losing fee tiers revert
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 100, amount_in);
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 3000, amount_in);
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 10000, amount_in);
        // UniV3 winner (register LAST)
        mock_single_quote(&vm, QUOTER, ARB, DAI, 500, amount_in, uni_out);

        let (dex_index, best_amount) = contract.compare_routes(ARB, DAI, amount_in).unwrap();
        assert_eq!(dex_index, U256::ZERO); // UniV3 is index 0
        assert_eq!(best_amount, uni_out);
    }

    #[test]
    fn compare_routes_amm_wins() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap();

        let amount_in = U256::from(10_000u64);

        // All UniV3 quotes revert
        for &fee in &[100u32, 500, 3000, 10000] {
            mock_single_quote_revert(&vm, QUOTER, ARB, DAI, fee, amount_in);
        }

        // AMM succeeds (register LAST)
        let amm_out = U256::from(9_970u64);
        mock_amm_direct(&vm, CAMELOT, ARB, DAI, amount_in, amm_out);

        let (dex_index, best_amount) = contract.compare_routes(ARB, DAI, amount_in).unwrap();
        assert_eq!(dex_index, U256::from(1)); // Camelot is index 1
        assert_eq!(best_amount, amm_out);
    }

    #[test]
    fn compare_routes_returns_dex_index() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = RouteOptimizer::from(&vm);
        contract.initialize().unwrap();
        // Index 0: QUOTER (UniV3)
        contract.add_dex(QUOTER, DEX_TYPE_UNIV3).unwrap();
        // Index 1: CAMELOT (AMM)
        contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap();
        // Index 2: SUSHI_QUOTER (UniV3)
        contract.add_dex(SUSHI_QUOTER, DEX_TYPE_UNIV3).unwrap();

        let amount_in = U256::from(1_000u64);
        let sushi_out = U256::from(999u64);

        // QUOTER and CAMELOT return nothing
        for &fee in &[100u32, 500, 3000, 10000] {
            mock_single_quote_revert(&vm, QUOTER, ARB, DAI, fee, amount_in);
        }
        mock_amm_direct_revert(&vm, CAMELOT, ARB, DAI, amount_in);

        // SUSHI_QUOTER: losing tiers revert, winning registered LAST
        mock_single_quote_revert(&vm, SUSHI_QUOTER, ARB, DAI, 100, amount_in);
        mock_single_quote_revert(&vm, SUSHI_QUOTER, ARB, DAI, 3000, amount_in);
        mock_single_quote_revert(&vm, SUSHI_QUOTER, ARB, DAI, 10000, amount_in);
        mock_single_quote(&vm, SUSHI_QUOTER, ARB, DAI, 500, amount_in, sushi_out);

        let (dex_index, best_amount) = contract.compare_routes(ARB, DAI, amount_in).unwrap();
        assert_eq!(dex_index, U256::from(2)); // Sushi is index 2
        assert_eq!(best_amount, sushi_out);
    }

    #[test]
    fn compare_routes_tie_goes_to_first() {
        let (vm, mut contract) = setup_contract();
        vm.set_sender(OWNER);
        contract.add_dex(CAMELOT, DEX_TYPE_AMM_V2).unwrap();

        let amount_in = U256::from(10_000u64);
        let tied_out = U256::from(9_970u64);

        // AMM mock (register first — will not be > uni, just ==)
        mock_amm_direct(&vm, CAMELOT, ARB, DAI, amount_in, tied_out);

        // UniV3 returning same amount
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 100, amount_in);
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 500, amount_in);
        mock_single_quote_revert(&vm, QUOTER, ARB, DAI, 10000, amount_in);
        mock_single_quote(&vm, QUOTER, ARB, DAI, 3000, amount_in, tied_out);

        let (dex_index, best_amount) = contract.compare_routes(ARB, DAI, amount_in).unwrap();
        // Tie: UniV3 was checked first and set best_out, AMM is NOT > (only ==)
        assert_eq!(dex_index, U256::ZERO);
        assert_eq!(best_amount, tied_out);
    }

    #[test]
    fn compare_routes_no_dex_returns_zero() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = RouteOptimizer::from(&vm);
        contract.initialize().unwrap();

        let (dex_index, best_amount) =
            contract.compare_routes(ARB, DAI, U256::from(1000u64)).unwrap();
        assert_eq!(dex_index, U256::ZERO);
        assert_eq!(best_amount, U256::ZERO);
    }

    #[test]
    fn add_dex_max_cap_enforced() {
        let vm = TestVM::new();
        vm.set_sender(OWNER);
        let mut contract = RouteOptimizer::from(&vm);
        contract.initialize().unwrap();

        for i in 0..MAX_DEXES {
            let addr_bytes = format!("000000000000000000000000000000000000{:04x}", i + 0x10);
            let addr: Address = addr_bytes.parse().unwrap();
            contract.add_dex(addr, DEX_TYPE_UNIV3).unwrap();
        }

        assert_eq!(contract.dex_count(), U256::from(MAX_DEXES));

        let overflow_addr: Address = "0000000000000000000000000000000000009999".parse().unwrap();
        let err = contract.add_dex(overflow_addr, DEX_TYPE_UNIV3).unwrap_err();
        assert_eq!(err, b"max dexes reached".to_vec());
    }
}
