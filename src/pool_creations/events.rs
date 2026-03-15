use crate::events::EKUBO_CORE;
use alloy_primitives::{Address, Log};
use alloy_sol_types::{sol, SolEvent};

sol! {
    #[derive(Debug)]
    event PairCreated(address indexed token0, address indexed token1, address pair, uint256 pairIndex);

    #[derive(Debug)]
    event PoolCreated(address indexed token0, address indexed token1, uint24 indexed fee, int24 tickSpacing, address pool);

    #[derive(Debug)]
    event Initialize(bytes32 indexed id, address indexed currency0, address indexed currency1, uint24 fee, int24 tickSpacing, address hooks, uint160 sqrtPriceX96, int24 tick);
}

// Separate sol! block for Ekubo to avoid PoolKey struct affecting V4 Initialize resolution.
sol! {
    /// Ekubo PoolKey struct — ensures event signature uses tuple encoding:
    /// keccak256("PoolInitialized(bytes32,(address,address,bytes32),int32,uint96)")
    #[derive(Debug)]
    struct EkuboPoolKey {
        address token0;
        address token1;
        bytes32 config;
    }

    #[derive(Debug)]
    event PoolInitialized(bytes32 poolId, EkuboPoolKey poolKey, int32 tick, uint96 sqrtRatio);
}

pub struct DecodedPoolCreation {
    /// Pool/pair contract address (lowercase hex for V2/V3, pool ID hex for V4)
    pub pool_address: String,
    /// Factory or pool manager address
    pub factory: Address,
    pub token0: Address,
    pub token1: Address,
    /// Fee in hundredths of a bip (V2: 3000, V3/V4: from event).
    /// Ekubo: raw u64 0.64 fixed-point fee stored as i64.
    pub fee: Option<i64>,
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
        pool_address: to_lowercase_hex(&decoded.data.pair),
        factory: log.address,
        token0: decoded.data.token0,
        token1: decoded.data.token1,
        fee: Some(3000i64), // V2 fixed 0.3% fee
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
        pool_address: to_lowercase_hex(&decoded.data.pool),
        factory: log.address,
        token0: decoded.data.token0,
        token1: decoded.data.token1,
        fee: Some(decoded.data.fee.to::<i64>()),
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
        fee: Some(decoded.data.fee.to::<i64>()),
        tick_spacing: Some(decoded.data.tickSpacing.as_i32()),
        additional_data: Some(serde_json::json!({
            "hooks_address": to_lowercase_hex(&decoded.data.hooks),
        })),
    })
}

/// Try to decode a log as an Ekubo PoolInitialized event.
pub fn decode_ekubo_pool_initialized(log: &Log) -> Option<DecodedPoolCreation> {
    // Only match logs from Ekubo Core
    if log.address != EKUBO_CORE {
        return None;
    }

    let topic0 = log.topics().first()?;
    if topic0.0 != PoolInitialized::SIGNATURE_HASH.0 {
        return None;
    }

    let decoded = PoolInitialized::decode_log(log).ok()?;

    // poolId is NOT indexed — it's in data (Ekubo Core has no indexed params on this event)
    let pool_address = format!("{}", decoded.data.poolId);

    let key = &decoded.data.poolKey;

    // Parse PoolConfig (bytes32): extension(20B) | fee(8B) | type_config(4B)
    let config_bytes: [u8; 32] = key.config.into();

    // Extension address: top 20 bytes
    let extension = Address::from_slice(&config_bytes[0..20]);

    // Fee: bytes 20..28 (u64, 0.64 fixed-point)
    let fee_bytes: [u8; 8] = config_bytes[20..28].try_into().unwrap();
    let fee_u64 = u64::from_be_bytes(fee_bytes);

    // Type config: bytes 28..32
    let type_config = u32::from_be_bytes(config_bytes[28..32].try_into().unwrap());
    let is_concentrated = (type_config & 0x80000000) != 0;

    let tick_spacing = if is_concentrated {
        Some((type_config & 0x7FFFFFFF) as i32)
    } else {
        None // stableswap — no tick spacing
    };

    // Build additional_data with Ekubo-specific fields
    let mut extra = serde_json::json!({
        "protocol": "ekubo",
        "extension": to_lowercase_hex(&extension),
        "fee_0_64": format!("{fee_u64}"),
        "type_config": format!("0x{type_config:08x}"),
        "is_concentrated": is_concentrated,
    });
    if !is_concentrated {
        let amplification = (type_config >> 24) & 0x7F;
        let center_tick_raw = type_config & 0x00FFFFFF;
        // Sign-extend 24-bit to i32, then multiply by 16
        let center_tick = ((center_tick_raw as i32) << 8 >> 8) * 16;
        extra["stableswap_amplification"] = serde_json::json!(amplification);
        extra["stableswap_center_tick"] = serde_json::json!(center_tick);
    }

    Some(DecodedPoolCreation {
        pool_address,
        factory: EKUBO_CORE,
        token0: key.token0,
        token1: key.token1,
        // Store fee as basis points approximation for DB compatibility.
        // Exact fee is in additional_data.fee_0_64.
        fee: i64::try_from(fee_u64).ok(), // raw u64 0.64 fixed-point fee
        tick_spacing,
        additional_data: Some(extra),
    })
}

/// Try decoding a log as any pool creation event (V2, V3, V4, Ekubo).
pub fn decode_pool_creation(log: &Log) -> Option<DecodedPoolCreation> {
    decode_pair_created(log)
        .or_else(|| decode_pool_created(log))
        .or_else(|| decode_initialize(log))
        .or_else(|| decode_ekubo_pool_initialized(log))
}

/// Convert an Address to lowercase hex string (matching existing DB convention).
fn to_lowercase_hex(addr: &Address) -> String {
    format!("{:#x}", addr)
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
    fn test_pool_initialized_signature() {
        // Must match keccak256("PoolInitialized(bytes32,(address,address,bytes32),int32,uint96)")
        assert_eq!(
            PoolInitialized::SIGNATURE_HASH.to_string(),
            "0x5e4688b340694b7c7fd30047fd082117dc46e32acfbf81a44bb1fac0ae65154d"
        );
    }

    #[test]
    fn test_ekubo_only_matches_core_address() {
        // A PoolInitialized log from a non-Ekubo address should be rejected
        let log = Log {
            address: Address::ZERO,
            data: LogData::new_unchecked(
                vec![PoolInitialized::SIGNATURE_HASH],
                vec![].into(),
            ),
        };
        assert!(decode_ekubo_pool_initialized(&log).is_none());
    }

    #[test]
    fn test_decode_ekubo_pool_initialized_golden() {
        // Real on-chain event from block 24500738, tx 0x6916f6e6...
        // WBTC / 0xcbb7... pool, tick_spacing=100, fee=0x000346dc5d638866
        // Real on-chain data: poolId | token0 | token1 | config | tick | sqrtRatio
        let data = hex::decode(concat!(
            "a8d735fdd345002618025f2304158152c337bb78b562e8c94336969c0ca78c46", // poolId
            "0000000000000000000000002260fac5e5542a773aa44fbcfedf7c193bc2c599", // token0 (WBTC)
            "000000000000000000000000cbb7c0000ab88b473b1f5afd9ef808440eed33bf", // token1
            "0000000000000000000000000000000000000000000346dc5d63886680000064", // config: ext=0 | fee | ts=100
            "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff3a9", // tick = -3159
            "00000000000000000000000000000000000000007fe6245453c0161629cc0393", // sqrtRatio
        )).unwrap();

        let log = Log {
            address: EKUBO_CORE,
            data: LogData::new_unchecked(
                vec![PoolInitialized::SIGNATURE_HASH],
                data.into(),
            ),
        };

        let decoded = decode_ekubo_pool_initialized(&log).expect("should decode");

        assert_eq!(
            decoded.pool_address,
            "0xa8d735fdd345002618025f2304158152c337bb78b562e8c94336969c0ca78c46"
        );
        assert_eq!(decoded.factory, EKUBO_CORE);
        assert_eq!(
            decoded.token0,
            "0x2260fac5e5542a773aa44fbcfedf7c193bc2c599".parse::<Address>().unwrap()
        );
        assert_eq!(
            decoded.token1,
            "0xcbb7c0000ab88b473b1f5afd9ef808440eed33bf".parse::<Address>().unwrap()
        );
        assert_eq!(decoded.tick_spacing, Some(100));

        let extra = decoded.additional_data.unwrap();
        assert_eq!(extra["protocol"], "ekubo");
        assert_eq!(extra["is_concentrated"], true);
        assert_eq!(extra["fee_0_64"], "922337203685478");
        assert_eq!(
            extra["extension"],
            "0x0000000000000000000000000000000000000000"
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
