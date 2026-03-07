// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import {PaymentReceiver} from "../src/PaymentReceiver.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @dev Minimal ERC20 mock with full IERC20 interface for SafeERC20 compatibility
contract MockUSDC {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "insufficient");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        require(balanceOf[from] >= amount, "insufficient");
        require(allowance[from][msg.sender] >= amount, "allowance");
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        return true;
    }

    function totalSupply() external pure returns (uint256) {
        return 0;
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
        assertEq(address(receiver.usdc()), address(usdc));
    }

    function test_constructor_reverts_zero_treasury() public {
        vm.expectRevert(PaymentReceiver.ZeroAddress.selector);
        new PaymentReceiver(address(0), address(usdc));
    }

    function test_constructor_reverts_zero_usdc() public {
        vm.expectRevert(PaymentReceiver.ZeroAddress.selector);
        new PaymentReceiver(treasury, address(0));
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
