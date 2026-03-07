// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Script.sol";
import {PaymentReceiver} from "../src/PaymentReceiver.sol";

contract DeployPaymentReceiver is Script {
    // Arbitrum One USDC (native, Circle)
    address constant USDC_ARB = 0xaf88d065e77c8cC2239327C5EDb3A432268e5831;
    // Arbitrum Sepolia USDC (test)
    address constant USDC_SEPOLIA = 0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d;

    function run() external {
        address treasury = vm.envAddress("TREASURY_ADDRESS");
        bool isSepolia = block.chainid == 421614;
        address usdc = isSepolia ? USDC_SEPOLIA : USDC_ARB;

        vm.startBroadcast();
        PaymentReceiver receiver = new PaymentReceiver(treasury, usdc);
        vm.stopBroadcast();

        console.log("PaymentReceiver deployed to:", address(receiver));
        console.log("Treasury:", treasury);
        console.log("USDC:", usdc);
        console.log("Chain ID:", block.chainid);
    }
}
