// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import {PaymentReceiver} from "../src/PaymentReceiver.sol";

/// @dev Minimal ERC20 mock for testing
contract MockUSDC {
    mapping(address => uint256) public balanceOf;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "insufficient");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

contract PaymentReceiverTest is Test {
    PaymentReceiver receiver;
    MockUSDC usdc;
    address treasury = address(0xBEEF);
    address user = address(0xCAFE);

    function setUp() public {
        usdc = new MockUSDC();
        receiver = new PaymentReceiver(treasury, address(usdc));
    }

    function test_constructor() public view {
        assertEq(receiver.treasury(), treasury);
        assertEq(receiver.usdc(), address(usdc));
    }

    function test_balance_zero() public view {
        assertEq(receiver.balance(), 0);
    }

    function test_balance_after_transfer() public {
        usdc.mint(address(receiver), 1_000_000); // 1 USDC
        assertEq(receiver.balance(), 1_000_000);
    }

    function test_withdraw_success() public {
        usdc.mint(address(receiver), 10_000_000); // 10 USDC
        vm.prank(treasury);
        receiver.withdraw();
        assertEq(usdc.balanceOf(treasury), 10_000_000);
        assertEq(receiver.balance(), 0);
    }

    function test_withdraw_emits_event() public {
        usdc.mint(address(receiver), 5_000_000);
        vm.prank(treasury);
        vm.expectEmit(true, false, false, true);
        emit PaymentReceiver.Withdrawn(treasury, 5_000_000);
        receiver.withdraw();
    }

    function test_withdraw_reverts_not_treasury() public {
        usdc.mint(address(receiver), 1_000_000);
        vm.prank(user);
        vm.expectRevert(PaymentReceiver.NotTreasury.selector);
        receiver.withdraw();
    }

    function test_withdraw_reverts_nothing() public {
        vm.prank(treasury);
        vm.expectRevert(PaymentReceiver.NothingToWithdraw.selector);
        receiver.withdraw();
    }

    function test_multiple_payments_then_withdraw() public {
        usdc.mint(address(receiver), 10_000); // $0.01
        usdc.mint(address(receiver), 10_000);
        usdc.mint(address(receiver), 10_000);
        assertEq(receiver.balance(), 30_000);

        vm.prank(treasury);
        receiver.withdraw();
        assertEq(usdc.balanceOf(treasury), 30_000);
    }
}
