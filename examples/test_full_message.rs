use alloy_primitives::{Address, I256, U256};
use bincode;

// Copy the type definitions from src/types.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PoolIdentifier {
    Address(Address),
    PoolId([u8; 32]),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Protocol {
    V2,
    V3,
    V4,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UpdateType {
    Swap,
    Mint,
    Burn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PoolUpdate {
    V2Swap { amount0: I256, amount1: I256 },
    V2Liquidity { amount0: I256, amount1: I256 },
    V3Swap {
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
    },
    V3Liquidity {
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
    },
    V4Swap {
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
    },
    V4Liquidity {
        tick_lower: i32,
        tick_upper: i32,
        liquidity_delta: i128,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolUpdateMessage {
    pub pool_id: PoolIdentifier,
    pub protocol: Protocol,
    pub update_type: UpdateType,
    pub block_number: u64,
    pub block_timestamp: u64,
    pub tx_index: u64,
    pub log_index: u64,
    pub is_revert: bool,
    pub update: PoolUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    UpdateWhitelist,
    BeginBlock {
        block_number: u64,
        block_timestamp: u64,
        is_revert: bool,
    },
    PoolUpdate(PoolUpdateMessage),
    EndBlock {
        block_number: u64,
        num_updates: u64,
    },
    Ping,
    Pong,
}

fn main() {
    println!("Testing full PoolUpdateMessage serialization\n");
    println!("{}", "=".repeat(80));

    // Test V3 Swap message (matching Etherscan data)
    let pool_addr: Address = "0x8ad599c3a0ff1de082011efddc58f1908eb6e6d8".parse().unwrap();
    let sqrt_price = U256::from(1382840672037684546977487336313952u128);

    let v3_swap_msg = PoolUpdateMessage {
        pool_id: PoolIdentifier::Address(pool_addr),
        protocol: Protocol::V3,
        update_type: UpdateType::Swap,
        block_number: 23741637,
        block_timestamp: 1730000000,
        tx_index: 2,
        log_index: 2,
        is_revert: false,
        update: PoolUpdate::V3Swap {
            sqrt_price_x96: sqrt_price,
            liquidity: 3100233156779584315,
            tick: 195356,
        },
    };

    let control_msg = ControlMessage::PoolUpdate(v3_swap_msg);
    let bytes = bincode::serialize(&control_msg).unwrap();

    println!("V3 Swap ControlMessage:");
    println!("  Total length: {} bytes", bytes.len());
    println!("  Hex: {}", hex::encode(&bytes));
    println!("\nByte breakdown:");

    // Parse manually
    let mut offset = 0;

    // ControlMessage discriminant (u32)
    let disc = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    println!("  [0-3] ControlMessage discriminant: {} (PoolUpdate)", disc);
    offset += 4;

    // PoolIdentifier discriminant (u32)
    let pool_id_disc = u32::from_le_bytes([bytes[offset], bytes[offset+1], bytes[offset+2], bytes[offset+3]]);
    println!("  [{}-{}] PoolIdentifier discriminant: {} (Address)", offset, offset+3, pool_id_disc);
    offset += 4;

    // Address length (u64)
    let addr_len = u64::from_le_bytes(bytes[offset..offset+8].try_into().unwrap());
    println!("  [{}-{}] Address length: {}", offset, offset+7, addr_len);
    offset += 8;

    // Address data (20 bytes)
    println!("  [{}-{}] Address data: {}", offset, offset+19, hex::encode(&bytes[offset..offset+20]));
    offset += 20;

    // Protocol (u32)
    let protocol = u32::from_le_bytes([bytes[offset], bytes[offset+1], bytes[offset+2], bytes[offset+3]]);
    println!("  [{}-{}] Protocol: {} (V3)", offset, offset+3, protocol);
    offset += 4;

    // UpdateType (u32)
    let update_type = u32::from_le_bytes([bytes[offset], bytes[offset+1], bytes[offset+2], bytes[offset+3]]);
    println!("  [{}-{}] UpdateType: {} (Swap)", offset, offset+3, update_type);
    offset += 4;

    // Metadata fields
    println!("  [{}-{}] block_number: {}", offset, offset+7, u64::from_le_bytes(bytes[offset..offset+8].try_into().unwrap()));
    offset += 8;
    println!("  [{}-{}] block_timestamp: {}", offset, offset+7, u64::from_le_bytes(bytes[offset..offset+8].try_into().unwrap()));
    offset += 8;
    println!("  [{}-{}] tx_index: {}", offset, offset+7, u64::from_le_bytes(bytes[offset..offset+8].try_into().unwrap()));
    offset += 8;
    println!("  [{}-{}] log_index: {}", offset, offset+7, u64::from_le_bytes(bytes[offset..offset+8].try_into().unwrap()));
    offset += 8;
    println!("  [{}-{}] is_revert: {}", offset, offset, bytes[offset]);
    offset += 1;

    // PoolUpdate discriminant (u32)
    let pool_update_disc = u32::from_le_bytes([bytes[offset], bytes[offset+1], bytes[offset+2], bytes[offset+3]]);
    println!("  [{}-{}] PoolUpdate discriminant: {} (V3Swap)", offset, offset+3, pool_update_disc);
    offset += 4;

    // V3Swap fields
    // sqrt_price_x96 (U256)
    let u256_len = u64::from_le_bytes(bytes[offset..offset+8].try_into().unwrap());
    println!("  [{}-{}] sqrt_price_x96 length: {}", offset, offset+7, u256_len);
    offset += 8;
    println!("  [{}-{}] sqrt_price_x96 data: {}", offset, offset+31, hex::encode(&bytes[offset..offset+32]));
    offset += 32;

    // liquidity (u128)
    println!("  [{}-{}] liquidity: {}", offset, offset+15, u128::from_le_bytes(bytes[offset..offset+16].try_into().unwrap()));
    offset += 16;

    // tick (i32)
    println!("  [{}-{}] tick: {}", offset, offset+3, i32::from_le_bytes(bytes[offset..offset+4].try_into().unwrap()));
    offset += 4;

    println!("\nTotal bytes parsed: {}", offset);
    println!("\n{}", "=".repeat(80));

    // Test V2 Swap
    println!("\nV2 Swap test:");
    let v2_pool: Address = "0xb4e16d0168e52d35cacd2c6185b44281ec28c9dc".parse().unwrap();
    let amount0 = I256::try_from(-4965441256i64).unwrap();
    let amount1 = I256::try_from(1512537406709823118i128).unwrap();

    let v2_swap_msg = PoolUpdateMessage {
        pool_id: PoolIdentifier::Address(v2_pool),
        protocol: Protocol::V2,
        update_type: UpdateType::Swap,
        block_number: 23741637,
        block_timestamp: 1730000000,
        tx_index: 2,
        log_index: 51,
        is_revert: false,
        update: PoolUpdate::V2Swap {
            amount0,
            amount1,
        },
    };

    let control_msg2 = ControlMessage::PoolUpdate(v2_swap_msg);
    let bytes2 = bincode::serialize(&control_msg2).unwrap();
    println!("  Total length: {} bytes", bytes2.len());
    println!("  Hex: {}", hex::encode(&bytes2));
}
