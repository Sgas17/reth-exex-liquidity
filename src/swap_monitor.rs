//! Swap Monitor — detects swap events in transactions from the executor address.
//!
//! Publishes `SwapConfirmation` to NATS for hedger correlation via tx_hash.
//! Integrated into the balance_monitor ExEx — single pass per block.

use alloy_consensus::TxReceipt;
use alloy_primitives::{Address, Log, I256, U256};
use alloy_sol_types::SolEvent;
use serde::Serialize;
use tracing::debug;

// Re-use the sol! event definitions from events.rs (same crate).
// We need the full event structs with sender/recipient for swap detection.
mod swap_events {
    use alloy_sol_types::sol;

    sol! {
        event V2Swap(
            address indexed sender,
            uint256 amount0In,
            uint256 amount1In,
            uint256 amount0Out,
            uint256 amount1Out,
            address indexed to
        );

        event V3Swap(
            address indexed sender,
            address indexed recipient,
            int256 amount0,
            int256 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick
        );

        // V4: topics[0]=sig, topics[1]=poolId, topics[2]=sender (indexed)
        // Data: amount0, amount1, sqrtPriceX96, liquidity, tick, fee
        event V4Swap(
            bytes32 indexed id,
            address indexed sender,
            int128 amount0,
            int128 amount1,
            uint160 sqrtPriceX96,
            uint128 liquidity,
            int24 tick,
            uint24 fee
        );
    }
}

/// A confirmed swap extracted from block logs.
#[derive(Debug, Clone, Serialize)]
pub struct SwapConfirmation {
    pub tx_hash: String,
    pub pool: String,
    pub protocol: String,
    pub amount0: String,
    pub amount1: String,
    pub token0: String,
    pub token1: String,
    pub block_number: u64,
    pub tx_index: u64,
    pub log_index: u64,
    pub ts: u64,
}

/// Try to decode a log as a swap event involving the executor address.
/// Returns None if it's not a swap or doesn't involve the executor.
///
/// For V2: executor must be `sender` (topic1) or `to` (topic2).
/// For V3: executor must be `sender` (topic1) or `recipient` (topic2).
/// For V4: executor must be `sender` (topic2).
pub fn decode_executor_swap(log: &Log, executor: Address) -> Option<DecodedSwap> {
    // V2 Swap
    if let Ok(event) = swap_events::V2Swap::decode_log(log) {
        let sender = event.topics().1;
        let to = event.topics().2;
        if sender != executor && to != executor {
            return None;
        }
        // V2: amount0In/Out, amount1In/Out → compute signed amounts
        // Positive = received by executor, negative = sent by executor
        let amount0 = if event.data.amount0Out > U256::ZERO {
            I256::try_from(event.data.amount0Out).unwrap_or(I256::MAX)
        } else {
            -I256::try_from(event.data.amount0In).unwrap_or(I256::MAX)
        };
        let amount1 = if event.data.amount1Out > U256::ZERO {
            I256::try_from(event.data.amount1Out).unwrap_or(I256::MAX)
        } else {
            -I256::try_from(event.data.amount1In).unwrap_or(I256::MAX)
        };
        return Some(DecodedSwap {
            pool: format!("{:#x}", log.address),
            protocol: "v2".to_string(),
            amount0: amount0.to_string(),
            amount1: amount1.to_string(),
        });
    }

    // V3 Swap
    if let Ok(event) = swap_events::V3Swap::decode_log(log) {
        let sender = event.topics().1;
        let recipient = event.topics().2;
        if sender != executor && recipient != executor {
            return None;
        }
        return Some(DecodedSwap {
            pool: format!("{:#x}", log.address),
            protocol: "v3".to_string(),
            amount0: event.data.amount0.to_string(),
            amount1: event.data.amount1.to_string(),
        });
    }

    // V4 Swap
    if log.topics().len() >= 3 && log.topics()[0] == swap_events::V4Swap::SIGNATURE_HASH {
        if let Ok(event) = swap_events::V4Swap::decode_log_data(&log.data) {
            // Indexed address is stored right-aligned in 32-byte topic.
            let sender = Address::from_slice(&log.topics()[2].as_slice()[12..]);
            if sender != executor {
                return None;
            }
            let pool_id = log.topics()[1];
            return Some(DecodedSwap {
                pool: format!("{:#x}", pool_id),
                protocol: "v4".to_string(),
                amount0: event.amount0.to_string(),
                amount1: event.amount1.to_string(),
            });
        }
    }

    None
}

/// Intermediate decoded swap before we have tx context.
#[derive(Debug)]
pub struct DecodedSwap {
    pub pool: String,
    pub protocol: String,
    pub amount0: String,
    pub amount1: String,
}

/// Scan a transaction's receipt logs for swaps involving the executor.
/// Returns SwapConfirmations with tx_hash and block context filled in.
pub fn scan_receipt_for_swaps<R: TxReceipt<Log = Log>>(
    receipt: &R,
    executor: Address,
    tx_hash: &str,
    block_number: u64,
    tx_index: u64,
    ts: u64,
) -> Vec<SwapConfirmation> {
    let mut confirmations = Vec::new();

    for (log_index, log) in receipt.logs().iter().enumerate() {
        if let Some(decoded) = decode_executor_swap(log, executor) {
            debug!(
                tx_hash = %tx_hash,
                pool = %decoded.pool,
                protocol = %decoded.protocol,
                "swap confirmation detected"
            );
            confirmations.push(SwapConfirmation {
                tx_hash: tx_hash.to_string(),
                pool: decoded.pool,
                protocol: decoded.protocol,
                amount0: decoded.amount0,
                amount1: decoded.amount1,
                // token0/token1 not available from swap event alone — filled as empty.
                // Hedger correlates by tx_hash, doesn't need tokens from here.
                token0: String::new(),
                token1: String::new(),
                block_number,
                tx_index,
                log_index: log_index as u64,
                ts,
            });
        }
    }

    confirmations
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, FixedBytes, I256, Uint};
    use alloy_sol_types::SolEvent;

    const EXECUTOR: Address = address!("f39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
    const OTHER: Address = address!("dEAD000000000000000000000000000000000000");
    const POOL: Address = address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640");

    fn make_v3_swap_log(pool: Address, sender: Address, recipient: Address) -> Log {
        // V3 Swap topics: [sig, sender, recipient]
        let sig = swap_events::V3Swap::SIGNATURE_HASH;
        let mut sender_topic = FixedBytes::<32>::ZERO;
        sender_topic[12..].copy_from_slice(sender.as_slice());
        let mut recipient_topic = FixedBytes::<32>::ZERO;
        recipient_topic[12..].copy_from_slice(recipient.as_slice());

        // Encode data: amount0, amount1, sqrtPriceX96, liquidity, tick
        let amount0 = I256::try_from(1000i64).unwrap();
        let amount1 = I256::try_from(-500i64).unwrap();
        let sqrt_price: Uint<160, 3> = Uint::from(0u64);
        let liquidity: u128 = 0;
        let tick = alloy_sol_types::private::primitives::aliases::I24::ZERO;

        use alloy_sol_types::SolValue;
        let data = (amount0, amount1, sqrt_price, liquidity, tick).abi_encode();

        Log::new(pool, vec![sig, sender_topic, recipient_topic], data.into()).unwrap()
    }

    #[test]
    fn detects_v3_swap_executor_is_recipient() {
        let log = make_v3_swap_log(POOL, OTHER, EXECUTOR);
        let result = decode_executor_swap(&log, EXECUTOR);
        assert!(result.is_some());
        let swap = result.unwrap();
        assert_eq!(swap.protocol, "v3");
        assert_eq!(swap.amount0, "1000");
        assert_eq!(swap.amount1, "-500");
    }

    #[test]
    fn detects_v3_swap_executor_is_sender() {
        let log = make_v3_swap_log(POOL, EXECUTOR, OTHER);
        let result = decode_executor_swap(&log, EXECUTOR);
        assert!(result.is_some());
    }

    #[test]
    fn ignores_swap_without_executor() {
        let log = make_v3_swap_log(POOL, OTHER, OTHER);
        let result = decode_executor_swap(&log, EXECUTOR);
        assert!(result.is_none());
    }
}
