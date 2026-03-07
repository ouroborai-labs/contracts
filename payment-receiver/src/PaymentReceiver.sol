// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "forge-std/interfaces/IERC20.sol";

/// @title PaymentReceiver
/// @notice Receives USDC x402 micropayments for the ouroborai agent platform.
///         Verified contract address resolves Blockaid "untrusted EOA" warnings.
/// @dev    This contract is the `to` address in EIP-3009 TransferWithAuthorization.
///         It simply accumulates USDC and allows the treasury to withdraw.
contract PaymentReceiver {
    address public immutable treasury;
    address public immutable usdc;

    event PaymentReceived(address indexed from, uint256 amount);
    event Withdrawn(address indexed to, uint256 amount);

    error NotTreasury();
    error NothingToWithdraw();

    constructor(address _treasury, address _usdc) {
        treasury = _treasury;
        usdc = _usdc;
    }

    /// @notice Withdraw all accumulated USDC to the treasury address
    function withdraw() external {
        if (msg.sender != treasury) revert NotTreasury();
        uint256 bal = IERC20(usdc).balanceOf(address(this));
        if (bal == 0) revert NothingToWithdraw();
        IERC20(usdc).transfer(treasury, bal);
        emit Withdrawn(treasury, bal);
    }

    /// @notice Check accumulated balance
    function balance() external view returns (uint256) {
        return IERC20(usdc).balanceOf(address(this));
    }
}
