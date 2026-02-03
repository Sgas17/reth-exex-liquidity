use alloy_primitives::{Address, Log};
use alloy_sol_types::{sol, SolEvent};

sol! {
    #[derive(Debug)]
    event PairCreated(address indexed token0, address indexed token1, address pair, uint256 pairIndex);

    #[derive(Debug)]
    event PoolCreated(address indexed token0, address indexed token1, uint24 indexed fee, int24 tickSpacing, address pool);

    #[derive(Debug)]
    event Initialize(bytes32 indexed id, address indexed currency0, address indexed currency1, uint24 fee, int24 tickSpacing, address hooks);
}

pub struct DecodedPoolCreation {
    /// Pool/pair contract address (checksummed hex for V2/V3, pool ID hex for V4)
    pub pool_address: String,
    /// Factory or pool manager address
    pub factory: Address,
    pub token0: Address,
    pub token1: Address,
    /// Fee in hundredths of a bip (V2: 3000, V3/V4: from event)
    pub fee: Option<i32>,
    /// Tick spacing (V3/V4 only)
    pub tick_spacing: Option<i32>,
    /// Protocol-specific JSON data (V4 hooks address, etc.)
    pub additional_data: Option<serde_json::Value>,
}

/// Try to decode a log as a Uniswap V2 PairCreated event.
pub fn decode_pair_created(log: &Log) -> Option<DecodedPoolCreation> {
    let topic0 = log.topics().first()?;
    if topic0.0 != PairCreated::SIGNATURE_HASH.0 {
        return None;
    }

    let decoded = PairCreated::decode_log(log).ok()?;

    Some(DecodedPoolCreation {
        pool_address: to_checksum(&decoded.data.pair),
        factory: log.address,
        token0: decoded.data.token0,
        token1: decoded.data.token1,
        fee: Some(3000), // V2 fixed 0.3% fee
        tick_spacing: None,
        additional_data: None,
    })
}

/// Try to decode a log as a Uniswap V3 PoolCreated event.
pub fn decode_pool_created(log: &Log) -> Option<DecodedPoolCreation> {
    let topic0 = log.topics().first()?;
    if topic0.0 != PoolCreated::SIGNATURE_HASH.0 {
        return None;
    }

    let decoded = PoolCreated::decode_log(log).ok()?;

    Some(DecodedPoolCreation {
        pool_address: to_checksum(&decoded.data.pool),
        factory: log.address,
        token0: decoded.data.token0,
        token1: decoded.data.token1,
        fee: Some(decoded.data.fee.to::<i32>()),
        tick_spacing: Some(decoded.data.tickSpacing.as_i32()),
        additional_data: None,
    })
}

/// Try to decode a log as a Uniswap V4 Initialize event.
pub fn decode_initialize(log: &Log) -> Option<DecodedPoolCreation> {
    let topic0 = log.topics().first()?;
    if topic0.0 != Initialize::SIGNATURE_HASH.0 {
        return None;
    }

    let decoded = Initialize::decode_log(log).ok()?;

    // V4 pools don't have individual contract addresses — use pool ID
    let (_, pool_id, _, _) = decoded.topics();
    let pool_address = format!("{pool_id}");

    Some(DecodedPoolCreation {
        pool_address,
        factory: log.address,
        token0: decoded.data.currency0,
        token1: decoded.data.currency1,
        fee: Some(decoded.data.fee.to::<i32>()),
        tick_spacing: Some(decoded.data.tickSpacing.as_i32()),
        additional_data: Some(serde_json::json!({
            "hooks_address": to_checksum(&decoded.data.hooks),
        })),
    })
}

/// Try decoding a log as any pool creation event (V2, V3, V4).
pub fn decode_pool_creation(log: &Log) -> Option<DecodedPoolCreation> {
    decode_pair_created(log)
        .or_else(|| decode_pool_created(log))
        .or_else(|| decode_initialize(log))
}

/// Convert an Address to EIP-55 checksummed hex string.
fn to_checksum(addr: &Address) -> String {
    addr.to_checksum(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::LogData;

    #[test]
    fn test_pair_created_signature() {
        assert_eq!(
            PairCreated::SIGNATURE_HASH.to_string(),
            "0x0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9"
        );
    }

    #[test]
    fn test_pool_created_signature() {
        assert_eq!(
            PoolCreated::SIGNATURE_HASH.to_string(),
            "0x783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118"
        );
    }

    #[test]
    fn test_initialize_signature() {
        assert_eq!(
            Initialize::SIGNATURE_HASH.to_string(),
            "0xdd466e674ea557f56295e2d0218a125ea4b4f0f6f3307b95f85e6110838d6438"
        );
    }

    #[test]
    fn test_reject_unknown_event() {
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![alloy_primitives::B256::from([0xff; 32])],
                vec![].into(),
            ),
        };
        assert!(decode_pool_creation(&log).is_none());
    }
}
