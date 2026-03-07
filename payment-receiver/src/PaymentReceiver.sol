// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @title PaymentReceiver
/// @notice Receives USDC x402 micropayments for the ouroborai agent platform.
///         Verified contract address resolves Blockaid "untrusted EOA" warnings.
/// @dev    This contract is the `to` address in EIP-3009 TransferWithAuthorization.
///         It simply accumulates USDC and allows the treasury to withdraw.
contract PaymentReceiver is ReentrancyGuard {
    using SafeERC20 for IERC20;

    address public immutable treasury;
    IERC20 public immutable usdc;

    event Withdrawn(address indexed to, uint256 amount);

    error NotTreasury();
    error NothingToWithdraw();
    error ZeroAddress();

    constructor(address _treasury, address _usdc) {
        if (_treasury == address(0)) revert ZeroAddress();
        if (_usdc == address(0)) revert ZeroAddress();
        treasury = _treasury;
        usdc = IERC20(_usdc);
    }

    /// @notice Withdraw all accumulated USDC to the treasury address
    function withdraw() external nonReentrant {
        if (msg.sender != treasury) revert NotTreasury();
        uint256 bal = usdc.balanceOf(address(this));
        if (bal == 0) revert NothingToWithdraw();
        usdc.safeTransfer(treasury, bal);
        emit Withdrawn(treasury, bal);
    }

    /// @notice Check accumulated balance
    function balance() external view returns (uint256) {
        return usdc.balanceOf(address(this));
    }
}
