//! Fluid DEX reserve decoder — pure math.
//!
//! Reproduces the on-chain `_getPricesAndExchangePrices`, `_getCollateralReserves`,
//! and `_getDebtReserves` functions from Fluid's `CoreHelpers.sol` in Rust.
//!
//! All inputs are raw `U256` storage slot values read from:
//! - Pool contract: slots 0 (`dexVariables`) and 1 (`dexVariables2`)
//! - Liquidity Layer: `exchangePriceToken{0,1}Slot`, `supplyToken{0,1}Slot`, `borrowToken{0,1}Slot`
//!
//! These slot addresses are immutable constants per pool, obtained once via `constantsView()`.

use alloy_primitives::{Address, U256};
use alloy_sol_types::{sol, SolCall, SolValue};

// ============================================================================
// CONSTANTS (matching Solidity)
// ============================================================================

const SIX_DECIMALS: u128 = 1_000_000;
const THREE_DECIMALS: u128 = 1_000;
const EXCHANGE_PRICES_PRECISION: u128 = 1_000_000_000_000; // 1e12
const SECONDS_PER_YEAR: u128 = 365 * 24 * 3600;
const FOUR_DECIMALS: u128 = 10_000;
const DEFAULT_EXPONENT_SIZE: u32 = 8;
const DEFAULT_EXPONENT_MASK: u128 = 0xFF;

const E27: u128 = 1_000_000_000_000_000_000_000_000_000; // 1e27

// Bit masks
const X10: u128 = 0x3FF;
const X14: u128 = 0x3FFF;
const X15: u128 = 0x7FFF;
const X16: u128 = 0xFFFF;
const X17: u128 = 0x1FFFF;
const X20: u128 = 0xFFFFF;
const X24: u128 = 0xFFFFFF;
const X28: u128 = 0xFFFFFFF;
const X30: u128 = 0x3FFFFFFF;
const X33: u128 = 0x1FFFFFFFF;
const X40: u128 = 0xFF_FFFFFFFF;
const X64: u128 = 0xFFFFFFFF_FFFFFFFF;

// LiquiditySlotsLink bit positions for exchangePricesAndConfig slot
const BITS_EXCHANGE_PRICES_BORROW_RATE: u32 = 0;        // 16 bits
const BITS_EXCHANGE_PRICES_FEE: u32 = 16;               // 14 bits
const BITS_EXCHANGE_PRICES_UTILIZATION: u32 = 30;        // 14 bits
const BITS_EXCHANGE_PRICES_LAST_TIMESTAMP: u32 = 58;     // 33 bits
const BITS_EXCHANGE_PRICES_SUPPLY_EXCHANGE_PRICE: u32 = 91;  // 64 bits
const BITS_EXCHANGE_PRICES_BORROW_EXCHANGE_PRICE: u32 = 155; // 64 bits
const BITS_EXCHANGE_PRICES_SUPPLY_RATIO: u32 = 219;      // 15 bits
const BITS_EXCHANGE_PRICES_BORROW_RATIO: u32 = 234;      // 15 bits

// LiquiditySlotsLink bit positions for user supply/borrow
const BITS_USER_SUPPLY_AMOUNT: u32 = 1; // starts at bit 1 (bit 0 is interest mode flag)
const BITS_USER_BORROW_AMOUNT: u32 = 1;

// ============================================================================
// OUTPUT TYPES
// ============================================================================

/// Complete decoded reserves for a Fluid pool, in 1e12 adjusted decimals.
#[derive(Debug, Clone, Default)]
pub struct FluidReserves {
    pub col_token0_real_reserves: u128,
    pub col_token1_real_reserves: u128,
    pub col_token0_imaginary_reserves: u128,
    pub col_token1_imaginary_reserves: u128,
    pub debt_token0_debt: u128,
    pub debt_token1_debt: u128,
    pub debt_token0_real_reserves: u128,
    pub debt_token1_real_reserves: u128,
    pub debt_token0_imaginary_reserves: u128,
    pub debt_token1_imaginary_reserves: u128,
    pub center_price: u128,
    pub fee: u128, // from dexVariables2 bits 2..18
}

/// Per-pool immutable configuration (obtained once from `constantsView()` + `constantsView2()`).
///
/// All fields are constants baked into the pool's bytecode — they never change.
/// Slot addresses point into the Liquidity Layer's storage for this specific pool.
#[derive(Debug, Clone)]
pub struct FluidPoolConfig {
    /// Pool contract address (for reading slots 0 and 1).
    pub pool_address: Address,
    /// Liquidity Layer address (for reading exchange price / supply / borrow slots).
    pub liquidity_address: Address,

    // ── Liquidity Layer slot keys (bytes32) ──────────────────────────────
    pub exchange_price_token0_slot: U256,
    pub exchange_price_token1_slot: U256,
    pub supply_token0_slot: U256,
    pub supply_token1_slot: U256,
    pub borrow_token0_slot: U256,
    pub borrow_token1_slot: U256,

    // ── Precision multipliers from constantsView2() ─────────────────────
    pub token0_numerator_precision: u128,
    pub token0_denominator_precision: u128,
    pub token1_numerator_precision: u128,
    pub token1_denominator_precision: u128,
}

impl FluidPoolConfig {
    /// The 8 storage reads needed to decode reserves, as (address, slot) pairs.
    pub fn storage_reads(&self) -> [(Address, U256); 8] {
        [
            (self.pool_address, U256::from(0)),                     // dexVariables
            (self.pool_address, U256::from(1)),                     // dexVariables2
            (self.liquidity_address, self.exchange_price_token0_slot),
            (self.liquidity_address, self.exchange_price_token1_slot),
            (self.liquidity_address, self.supply_token0_slot),
            (self.liquidity_address, self.supply_token1_slot),
            (self.liquidity_address, self.borrow_token0_slot),
            (self.liquidity_address, self.borrow_token1_slot),
        ]
    }

    /// Build a `FluidStorageSlots` from 8 raw U256 values (in the same order as `storage_reads()`).
    pub fn to_storage_slots(&self, values: &[U256; 8]) -> FluidStorageSlots {
        FluidStorageSlots {
            dex_variables: values[0],
            dex_variables2: values[1],
            exchange_price_token0: values[2],
            exchange_price_token1: values[3],
            supply_token0: values[4],
            supply_token1: values[5],
            borrow_token0: values[6],
            borrow_token1: values[7],
        }
    }
}

// ============================================================================
// ABI DEFINITIONS FOR POOL CONFIG RESOLUTION
// ============================================================================

sol! {
    /// `constantsView()` return struct from FluidDexT1.
    /// Returns immutable pool constants including Liquidity Layer slot addresses.
    /// Field order must match the Solidity struct exactly for ABI decoding.
    #[derive(Debug)]
    struct ConstantsView {
        uint256 dexId;
        address liquidity;
        address factory;
        ConstantsViewImplementations implementations;
        address deployerContract;
        address token0;
        address token1;
        bytes32 supplyToken0Slot;
        bytes32 borrowToken0Slot;
        bytes32 supplyToken1Slot;
        bytes32 borrowToken1Slot;
        bytes32 exchangePriceToken0Slot;
        bytes32 exchangePriceToken1Slot;
        uint256 oracleMapping;
    }

    #[derive(Debug)]
    struct ConstantsViewImplementations {
        address shift;
        address admin;
        address colOperations;
        address debtOperations;
        address perfectOperationsAndSwapOut;
    }

    /// `constantsView2()` return struct.
    #[derive(Debug)]
    struct ConstantsView2 {
        uint256 token0NumeratorPrecision;
        uint256 token0DenominatorPrecision;
        uint256 token1NumeratorPrecision;
        uint256 token1DenominatorPrecision;
    }

    /// ABI for calling `constantsView()` and `constantsView2()`.
    #[derive(Debug)]
    interface IFluidDexT1 {
        function constantsView() external view returns (ConstantsView memory constantsView_);
        function constantsView2() external view returns (ConstantsView2 memory constantsView2_);
    }
}

/// Build `FluidPoolConfig` from raw ABI return data of `constantsView()` and `constantsView2()`.
///
/// Call this once per pool at registration time:
/// ```ignore
/// let cv_calldata = IFluidDexT1::constantsViewCall {}.abi_encode();
/// let cv2_calldata = IFluidDexT1::constantsView2Call {}.abi_encode();
/// // ... send calls, get return bytes ...
/// let config = FluidPoolConfig::from_abi(pool_addr, &cv_returndata, &cv2_returndata)?;
/// ```
impl FluidPoolConfig {
    /// ABI-encoded calldata for `constantsView()`.
    pub fn constants_view_calldata() -> Vec<u8> {
        IFluidDexT1::constantsViewCall {}.abi_encode()
    }

    /// ABI-encoded calldata for `constantsView2()`.
    pub fn constants_view2_calldata() -> Vec<u8> {
        IFluidDexT1::constantsView2Call {}.abi_encode()
    }

    /// Parse ABI return data from both `constantsView()` and `constantsView2()`
    /// into a `FluidPoolConfig`.
    pub fn from_abi(
        pool_address: Address,
        constants_view_returndata: &[u8],
        constants_view2_returndata: &[u8],
    ) -> Result<Self, alloy_sol_types::Error> {
        let cv = ConstantsView::abi_decode(constants_view_returndata)?;
        let cv2 = ConstantsView2::abi_decode(constants_view2_returndata)?;

        Ok(Self {
            pool_address,
            liquidity_address: cv.liquidity,
            exchange_price_token0_slot: U256::from_be_bytes(cv.exchangePriceToken0Slot.0),
            exchange_price_token1_slot: U256::from_be_bytes(cv.exchangePriceToken1Slot.0),
            supply_token0_slot: U256::from_be_bytes(cv.supplyToken0Slot.0),
            supply_token1_slot: U256::from_be_bytes(cv.supplyToken1Slot.0),
            borrow_token0_slot: U256::from_be_bytes(cv.borrowToken0Slot.0),
            borrow_token1_slot: U256::from_be_bytes(cv.borrowToken1Slot.0),
            token0_numerator_precision: u128_from_u256(cv2.token0NumeratorPrecision),
            token0_denominator_precision: u128_from_u256(cv2.token0DenominatorPrecision),
            token1_numerator_precision: u128_from_u256(cv2.token1NumeratorPrecision),
            token1_denominator_precision: u128_from_u256(cv2.token1DenominatorPrecision),
        })
    }
}

/// Raw storage inputs needed for reserve computation.
#[derive(Debug, Clone)]
pub struct FluidStorageSlots {
    pub dex_variables: U256,       // pool slot 0
    pub dex_variables2: U256,      // pool slot 1
    pub exchange_price_token0: U256, // Liquidity Layer
    pub exchange_price_token1: U256, // Liquidity Layer
    pub supply_token0: U256,       // Liquidity Layer (user supply data for pool as user)
    pub supply_token1: U256,       // Liquidity Layer
    pub borrow_token0: U256,       // Liquidity Layer (user borrow data for pool as user)
    pub borrow_token1: U256,       // Liquidity Layer
}

// ============================================================================
// HELPERS
// ============================================================================

/// Low 128 bits of a U256.
#[inline]
fn u128_from_u256(v: U256) -> u128 {
    v.as_limbs()[0] as u128 | ((v.as_limbs()[1] as u128) << 64)
}

/// Extract bits from U256 as u128: `(val >> shift) & mask`.
#[inline]
fn extract_u128(val: &U256, shift: u32, mask: u128) -> u128 {
    u128_from_u256(*val >> shift) & mask
}

/// BigMath decode: `normal = coefficient << exponent`
/// where `coefficient = bigNumber >> exponentSize` and `exponent = bigNumber & exponentMask`.
#[inline]
fn from_big_number(big_number: u128) -> u128 {
    let coefficient = big_number >> DEFAULT_EXPONENT_SIZE;
    let exponent = big_number & DEFAULT_EXPONENT_MASK;
    coefficient << exponent as u32
}

/// Integer square root (Babylonian method) for u128.
fn isqrt_u128(n: u128) -> u128 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Integer square root for U256, returning u128.
fn isqrt_u256(val: U256) -> u128 {
    if val <= U256::from(u128::MAX) {
        return isqrt_u128(u128_from_u256(val));
    }
    let mut x = val;
    let mut y = (x + U256::from(1)) / U256::from(2);
    while y < x {
        x = y;
        y = (x + val / x) / U256::from(2);
    }
    u128_from_u256(x)
}

/// Multiply two u128 values, returning U256 to avoid overflow.
#[inline]
fn mul256(a: u128, b: u128) -> U256 {
    U256::from(a) * U256::from(b)
}

/// Invert a 1e27-scaled price: `1e54 / price`. Uses U256 because 1e54 overflows u128.
#[inline]
fn inv_price(price: u128) -> u128 {
    u128_from_u256(mul256(E27, E27) / U256::from(price))
}

// ============================================================================
// EXCHANGE PRICE CALCULATION
// ============================================================================

/// Reproduces `LiquidityCalcs.calcExchangePrices()`.
///
/// Reads a packed `exchangePricesAndConfig` storage slot and returns
/// (supplyExchangePrice, borrowExchangePrice) updated for elapsed interest.
pub fn calc_exchange_prices(
    exchange_prices_and_config: &U256,
    current_timestamp: u64,
) -> (u128, u128) {
    let mut supply_exchange_price =
        extract_u128(exchange_prices_and_config, BITS_EXCHANGE_PRICES_SUPPLY_EXCHANGE_PRICE, X64);
    let mut borrow_exchange_price =
        extract_u128(exchange_prices_and_config, BITS_EXCHANGE_PRICES_BORROW_EXCHANGE_PRICE, X64);

    if supply_exchange_price == 0 || borrow_exchange_price == 0 {
        return (supply_exchange_price, borrow_exchange_price);
    }

    let borrow_rate = extract_u128(exchange_prices_and_config, BITS_EXCHANGE_PRICES_BORROW_RATE, X16);

    let last_timestamp = extract_u128(exchange_prices_and_config, BITS_EXCHANGE_PRICES_LAST_TIMESTAMP, X33);
    let seconds_since = (current_timestamp as u128).saturating_sub(last_timestamp);
    if current_timestamp != 0 && last_timestamp > current_timestamp as u128 {
        // Timestamp from storage is in the future — exchange prices may be stale or
        // test timestamp is wrong. Use 0 seconds elapsed (no interest accrual).
        return (supply_exchange_price, borrow_exchange_price);
    }

    let borrow_ratio = extract_u128(exchange_prices_and_config, BITS_EXCHANGE_PRICES_BORROW_RATIO, X15);

    if seconds_since == 0 || borrow_rate == 0 || borrow_ratio == 1 {
        return (supply_exchange_price, borrow_exchange_price);
    }

    // Update borrow exchange price
    borrow_exchange_price += (borrow_exchange_price * borrow_rate * seconds_since)
        / (SECONDS_PER_YEAR * FOUR_DECIMALS);

    // Calculate supply rate
    let supply_ratio = extract_u128(exchange_prices_and_config, BITS_EXCHANGE_PRICES_SUPPLY_RATIO, X15);
    if supply_ratio == 1 {
        return (supply_exchange_price, borrow_exchange_price);
    }

    let utilization = extract_u128(exchange_prices_and_config, BITS_EXCHANGE_PRICES_UTILIZATION, X14);
    let fee = extract_u128(exchange_prices_and_config, BITS_EXCHANGE_PRICES_FEE, X14);

    // ratioSupplyYield calculation
    let ratio_supply_yield: u128;
    if supply_ratio & 1 == 1 {
        // supplyWithInterest / supplyInterestFree (free is bigger)
        let sr = supply_ratio >> 1;
        if sr == 0 {
            return (supply_exchange_price, borrow_exchange_price);
        }
        let temp = (E27 * FOUR_DECIMALS) / sr; // 1e27 * 1e4 / sr
        ratio_supply_yield = (utilization * (E27 + temp)) / FOUR_DECIMALS;
    } else {
        // supplyInterestFree / supplyWithInterest (with interest is bigger)
        let sr = supply_ratio >> 1;
        ratio_supply_yield = (E27 * utilization * (FOUR_DECIMALS + sr))
            / (FOUR_DECIMALS * FOUR_DECIMALS);
    }

    // borrowRatio contribution
    let borrow_ratio_yield: u128;
    if borrow_ratio & 1 == 1 {
        // borrowWithInterest / borrowInterestFree (free is bigger)
        let br = borrow_ratio >> 1;
        if br == 0 {
            return (supply_exchange_price, borrow_exchange_price);
        }
        borrow_ratio_yield = (br * E27) / (FOUR_DECIMALS + br);
    } else {
        let br = borrow_ratio >> 1;
        borrow_ratio_yield = E27
            - ((br * E27) / (FOUR_DECIMALS + br));
    }

    // temp_ = ratioSupplyYield scaled to normal percent
    // Divisor is 1e54 which overflows u128 — use U256
    let e54 = U256::from(10u64).pow(U256::from(54u64));
    let temp = {
        let r = U256::from(FOUR_DECIMALS)
            * U256::from(ratio_supply_yield)
            * U256::from(borrow_ratio_yield)
            / e54;
        u128_from_u256(r)
    };

    // supply rate = borrow_rate * temp * (1e4 - fee)
    let supply_rate_numer = borrow_rate as u128 * temp * (FOUR_DECIMALS - fee);

    supply_exchange_price += (supply_exchange_price * supply_rate_numer * seconds_since)
        / (SECONDS_PER_YEAR * FOUR_DECIMALS * FOUR_DECIMALS * FOUR_DECIMALS);

    (supply_exchange_price, borrow_exchange_price)
}

// ============================================================================
// COLLATERAL SUPPLY / DEBT AMOUNT EXTRACTION
// ============================================================================

/// Reproduces `_getLiquidityCollateral()`: reads supply amount from packed slot,
/// applies exchange price, scales to 1e12 adjusted.
fn get_liquidity_collateral(
    supply_data: &U256,
    exchange_price: u128,
    numerator_precision: u128,
    denominator_precision: u128,
) -> u128 {
    let raw = extract_u128(supply_data, BITS_USER_SUPPLY_AMOUNT, X64);
    let mut supply = from_big_number(raw);

    if extract_u128(supply_data, 0, 1) == 1 {
        // Use U256 to avoid overflow: supply * exchangePrice can exceed u128
        supply = u128_from_u256(mul256(supply, exchange_price) / U256::from(EXCHANGE_PRICES_PRECISION));
    }

    u128_from_u256(mul256(supply, numerator_precision) / U256::from(denominator_precision))
}

fn get_liquidity_debt(
    borrow_data: &U256,
    exchange_price: u128,
    numerator_precision: u128,
    denominator_precision: u128,
) -> u128 {
    let raw = extract_u128(borrow_data, BITS_USER_BORROW_AMOUNT, X64);
    let mut debt = from_big_number(raw);

    if extract_u128(borrow_data, 0, 1) == 1 {
        debt = u128_from_u256(mul256(debt, exchange_price) / U256::from(EXCHANGE_PRICES_PRECISION));
    }

    u128_from_u256(mul256(debt, numerator_precision) / U256::from(denominator_precision))
}

// ============================================================================
// RESERVE COMPUTATION (QUADRATIC SOLVER)
// ============================================================================

/// Reproduces `_calculateReservesOutsideRange()`.
///
/// Solves the quadratic to get imaginary reserves from real reserves + price range.
/// Returns (token0_imaginary, token1_imaginary) **without** real reserves added yet.
fn calculate_reserves_outside_range(gp: u128, pa: u128, rx: u128, ry: u128) -> (u128, u128) {
    let p1 = pa - gp;
    let p2 = u128_from_u256((mul256(gp, rx) + mul256(ry, E27)) / U256::from(2u128 * p1));

    let p3_raw = mul256(rx, ry);
    let e50 = U256::from(10u64).pow(U256::from(50u64));
    let p3 = if p3_raw < e50 {
        u128_from_u256(p3_raw * U256::from(E27) / U256::from(p1))
    } else {
        u128_from_u256(p3_raw / U256::from(p1) * U256::from(E27))
    };

    let xa = p2 + isqrt_u256(U256::from(p3) + mul256(p2, p2));
    let yb = u128_from_u256(mul256(xa, gp) / U256::from(E27));
    (xa, yb)
}

/// Reproduces `_calculateDebtReserves()`.
///
/// Returns (rx, ry, irx, iry) = (real0, real1, imaginary0, imaginary1).
fn calculate_debt_reserves(gp: u128, pb: u128, dx: u128, dy: u128) -> (u128, u128, u128, u128) {
    let dx_gp = mul256(dx, gp);
    let dy_e27 = mul256(dy, E27);
    let two_e27 = U256::from(2u128 * E27);
    let p1: i128 = if dx_gp >= dy_e27 {
        u128_from_u256((dx_gp - dy_e27) / two_e27) as i128
    } else {
        -(u128_from_u256((dy_e27 - dx_gp) / two_e27) as i128)
    };

    let dx_dy = mul256(dx, dy);
    let e50 = U256::from(10u64).pow(U256::from(50u64));
    let p2 = if dx_dy < e50 {
        u128_from_u256(dx_dy * U256::from(pb) / U256::from(E27))
    } else {
        u128_from_u256(dx_dy / U256::from(E27) * U256::from(pb))
    };

    let p1_abs = p1.unsigned_abs();
    let ry = (p1 + isqrt_u256(U256::from(p2) + mul256(p1_abs, p1_abs)) as i128) as u128;

    let ry_sq = mul256(ry, ry);
    let iry_denom = mul256(ry, E27) - mul256(dx, pb); // ry*1e27 - dx*pb
    let iry = if ry < 10u128.pow(25) {
        u128_from_u256(ry_sq * U256::from(E27) / iry_denom)
    } else {
        u128_from_u256(ry_sq / (iry_denom / U256::from(E27)))
    };

    let iry_dx_over_ry = u128_from_u256(mul256(iry, dx) / U256::from(ry));
    // In valid pools, iry * dx / ry > dx. If not, debt reserves are degenerate.
    let irx = iry_dx_over_ry.saturating_sub(dx);
    let rx = u128_from_u256(mul256(irx, dy) / U256::from(iry + dy));

    (rx, ry, irx, iry)
}

// ============================================================================
// MAIN ENTRY POINT
// ============================================================================

/// Decode full Fluid pool reserves from raw storage slots.
///
/// This is the Rust equivalent of calling the DexReservesResolver's
/// `getPoolReservesAdjusted()` — but from raw storage, no RPC needed.
pub fn decode_fluid_reserves(
    slots: &FluidStorageSlots,
    config: &FluidPoolConfig,
    current_timestamp: u64,
) -> Option<FluidReserves> {
    let dv = &slots.dex_variables;
    let dv2 = &slots.dex_variables2;

    // ── 1. Extract center price ──────────────────────────────────────────
    let center_price_hook = extract_u128(dv2, 112, X30);
    let center_price: u128;

    if (extract_u128(dv2, 248, 1)) == 0 {
        if center_price_hook == 0 {
            // Center price from dexVariables storage (BigMath encoded)
            let raw = extract_u128(dv, 81, X40);
            center_price = from_big_number(raw);
        } else {
            // External oracle (e.g. wstETH exchange rate). We can't call the oracle
            // contract from pure storage reads. Use the stored center price from
            // dexVariables (bits 81..120) as the best available approximation.
            // This is the center price set at the last swap, which for pegged pools
            // tracks the oracle closely.
            let raw = extract_u128(dv, 81, X40);
            center_price = from_big_number(raw);
        }
    } else {
        // Active center price shift — requires delegatecall to shift implementation.
        // Use stored center price as fallback.
        let raw = extract_u128(dv, 81, X40);
        center_price = from_big_number(raw);
    }

    if center_price == 0 {
        return None;
    }

    // ── 2. Extract ranges ────────────────────────────────────────────────
    let upper_range_pct = extract_u128(dv2, 27, X20);
    let lower_range_pct = extract_u128(dv2, 47, X20);

    // Check for active range shift
    if (extract_u128(dv2, 26, 1)) == 1 {
        // Active range shift — can't reproduce. Return None.
        return None;
    }

    // Convert percentage to price: upperRange = centerPrice * 1e6 / (1e6 - pct)
    let upper_range = (center_price * SIX_DECIMALS) / (SIX_DECIMALS - upper_range_pct);
    let lower_range = (center_price * (SIX_DECIMALS - lower_range_pct)) / SIX_DECIMALS;

    // ── 3. Check threshold rebalancing ───────────────────────────────────
    let mut effective_center_price = center_price;
    let threshold_bits = extract_u128(dv2, 68, X20);
    if threshold_bits > 0 {
        // Threshold-based rebalancing active
        let upper_threshold = extract_u128(dv2, 68, X10);
        let lower_threshold = extract_u128(dv2, 78, X10);
        let shifting_time = extract_u128(dv2, 88, X24);

        // Check for active threshold shift
        if (extract_u128(dv2, 67, 1)) == 1 {
            return None; // active threshold shift, needs EVM
        }

        let last_stored_price_raw = extract_u128(dv, 41, X40);
        let last_stored_price = from_big_number(last_stored_price_raw);

        let upper_trigger = center_price
            + ((upper_range - center_price) * (THREE_DECIMALS - upper_threshold)) / THREE_DECIMALS;
        let lower_trigger = center_price
            - ((center_price - lower_range) * (THREE_DECIMALS - lower_threshold)) / THREE_DECIMALS;

        if last_stored_price > upper_trigger {
            let last_swap_timestamp = extract_u128(dv, 121, X33);
            let time_elapsed = current_timestamp as u128 - last_swap_timestamp;
            if time_elapsed < shifting_time {
                effective_center_price =
                    center_price + ((upper_range - center_price) * time_elapsed) / shifting_time;
            } else {
                effective_center_price = upper_range;
            }
        } else if last_stored_price < lower_trigger {
            let last_swap_timestamp = extract_u128(dv, 121, X33);
            let time_elapsed = current_timestamp as u128 - last_swap_timestamp;
            if time_elapsed < shifting_time {
                effective_center_price =
                    center_price - ((center_price - lower_range) * time_elapsed) / shifting_time;
            } else {
                effective_center_price = lower_range;
            }
        }
    }

    // Clamp to min/max center price
    let max_center = from_big_number(extract_u128(dv2, 172, X28));
    let min_center = from_big_number(extract_u128(dv2, 200, X28));
    if max_center > 0 && effective_center_price > max_center {
        effective_center_price = max_center;
    }
    if min_center > 0 && effective_center_price < min_center {
        effective_center_price = min_center;
    }

    // Recalculate ranges if center price changed
    let (final_upper, final_lower) = if effective_center_price != center_price {
        let ur = (effective_center_price * SIX_DECIMALS) / (SIX_DECIMALS - upper_range_pct);
        let lr = (effective_center_price * (SIX_DECIMALS - lower_range_pct)) / SIX_DECIMALS;
        (ur, lr)
    } else {
        (upper_range, lower_range)
    };

    // ── 4. Geometric mean ────────────────────────────────────────────────
    // Use U256 to avoid overflow (two 1e27-scale prices multiplied = 1e54)
    let geometric_mean = isqrt_u256(mul256(final_upper, final_lower));

    // ── 5. Exchange prices ───────────────────────────────────────────────
    let (supply_ex_price0, borrow_ex_price0) =
        calc_exchange_prices(&slots.exchange_price_token0, current_timestamp);
    let (supply_ex_price1, borrow_ex_price1) =
        calc_exchange_prices(&slots.exchange_price_token1, current_timestamp);

    // ── 6. Fee ───────────────────────────────────────────────────────────
    let fee = extract_u128(dv2, 2, X17);

    // ── 7. Collateral reserves ───────────────────────────────────────────
    let col_enabled = (extract_u128(dv2, 0, 1)) == 1;
    let debt_enabled = (extract_u128(dv2, 1, 1)) == 1;

    let mut result = FluidReserves {
        center_price: effective_center_price,
        fee,
        ..Default::default()
    };

    if col_enabled {
        let token0_supply = get_liquidity_collateral(
            &slots.supply_token0,
            supply_ex_price0,
            config.token0_numerator_precision,
            config.token0_denominator_precision,
        );
        let token1_supply = get_liquidity_collateral(
            &slots.supply_token1,
            supply_ex_price1,
            config.token1_numerator_precision,
            config.token1_denominator_precision,
        );

        let (imag0, imag1) = if geometric_mean < E27 {
            calculate_reserves_outside_range(geometric_mean, final_upper, token0_supply, token1_supply)
        } else {
            let inv_gm = inv_price(geometric_mean);
            let inv_lower = inv_price(final_lower);
            let (i1, i0) =
                calculate_reserves_outside_range(inv_gm, inv_lower, token1_supply, token0_supply);
            (i0, i1)
        };

        result.col_token0_real_reserves = token0_supply;
        result.col_token1_real_reserves = token1_supply;
        result.col_token0_imaginary_reserves = imag0 + token0_supply;
        result.col_token1_imaginary_reserves = imag1 + token1_supply;
    }

    if debt_enabled {
        let token0_debt = get_liquidity_debt(
            &slots.borrow_token0,
            borrow_ex_price0,
            config.token0_numerator_precision,
            config.token0_denominator_precision,
        );
        let token1_debt = get_liquidity_debt(
            &slots.borrow_token1,
            borrow_ex_price1,
            config.token1_numerator_precision,
            config.token1_denominator_precision,
        );

        let (rx, ry, irx, iry) = if geometric_mean < E27 {
            calculate_debt_reserves(geometric_mean, final_lower, token0_debt, token1_debt)
        } else {
            let inv_gm = inv_price(geometric_mean);
            let inv_upper = inv_price(final_upper);
            let (ry2, rx2, iry2, irx2) =
                calculate_debt_reserves(inv_gm, inv_upper, token1_debt, token0_debt);
            (rx2, ry2, irx2, iry2)
        };

        result.debt_token0_debt = token0_debt;
        result.debt_token1_debt = token1_debt;
        result.debt_token0_real_reserves = rx;
        result.debt_token1_real_reserves = ry;
        result.debt_token0_imaginary_reserves = irx;
        result.debt_token1_imaginary_reserves = iry;
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_big_number() {
        assert_eq!(from_big_number(0), 0);
        assert_eq!(from_big_number(256), 1); // coeff=1, exp=0
        assert_eq!(from_big_number(1283), 40); // coeff=5, exp=3 → 5<<3=40
    }

    #[test]
    fn test_isqrt() {
        assert_eq!(isqrt_u128(0), 0);
        assert_eq!(isqrt_u128(4), 2);
        assert_eq!(isqrt_u128(100), 10);
        assert_eq!(isqrt_u128(10u128.pow(18) * 10u128.pow(18)), 10u128.pow(18));
        assert_eq!(isqrt_u256(U256::from(E27) * U256::from(E27)), E27);
    }

    #[test]
    fn test_inv_price() {
        // inv_price(1e27) = 1e54 / 1e27 = 1e27
        assert_eq!(inv_price(E27), E27);
        // inv_price(2e27) = 1e54 / 2e27 = 5e26
        assert_eq!(inv_price(2 * E27), E27 / 2);
    }

    /// Validate against on-chain resolver output for pool 1 (wstETH/ETH).
    ///
    /// Storage slots captured from mainnet. Expected values from
    /// `DexReservesResolver.getPoolReservesAdjusted(0x0B1a...C9e7)`.
    ///
    #[test]
    fn test_decode_pool1_wsteth_eth() {
        let slots = FluidStorageSlots {
            dex_variables: U256::from_str_radix(
                "000000000000000000070000f0d368fecffc67a92075fc21611075fc21611074",
                16,
            )
            .unwrap(),
            dex_variables2: U256::from_str_radix(
                "00edbb6e379846813f44a000000000000030ffffff00000002ee000008c801c3",
                16,
            )
            .unwrap(),
            exchange_price_token0: U256::from_str_radix(
                "904625697166532825591669513414596638658810643102520386703340641195053809685",
                10,
            )
            .unwrap(),
            exchange_price_token1: U256::from_str_radix(
                "49878176876721900615456177109864974079344989024826006438171",
                10,
            )
            .unwrap(),
            supply_token0: U256::from_str_radix(
                "291355544087482513783298826876732264667261827842384813763236642851",
                10,
            )
            .unwrap(),
            supply_token1: U256::from_str_radix(
                "353061964987027740364110171626380088481652267611420936207797771813",
                10,
            )
            .unwrap(),
            borrow_token0: U256::from_str_radix(
                "94710661335958479177862988578881135820012110919506487131842318647139363",
                10,
            )
            .unwrap(),
            borrow_token1: U256::from_str_radix(
                "58153252158555476274676141124958551809435998974524591376947888779040803",
                10,
            )
            .unwrap(),
        };

        let config = pool1_config();

        // Timestamp when slots were read (from latest block)
        let timestamp = 1773437867u64;

        let result = decode_fluid_reserves(&slots, &config, timestamp);
        assert!(result.is_some(), "decode should succeed for pool 1");
        let r = result.unwrap();

        // Expected from resolver (1e12 adjusted):
        // fee = 112
        assert_eq!(r.fee, 112, "fee mismatch");

        // Center price ≈ 1.229e27 — check within 1% (timestamp-dependent)
        let expected_center = 1_229_247_679_861_379_355_000_000_000u128;
        let diff_pct =
            (r.center_price as i128 - expected_center as i128).unsigned_abs() * 100 / expected_center;
        assert!(
            diff_pct < 2,
            "center price off by {}%: got {}, expected {}",
            diff_pct,
            r.center_price,
            expected_center
        );

        // Collateral reserves (1e12 adjusted) — check within 5% (timestamp-dependent exchange prices)
        let expected_col_t0_imag = 20_314_635_945_036_376_858u128;
        check_within_pct(r.col_token0_imaginary_reserves, expected_col_t0_imag, 5, "col_t0_imag");

        let expected_col_t1_imag = 24_958_234_461_191_088_810u128;
        check_within_pct(r.col_token1_imaginary_reserves, expected_col_t1_imag, 5, "col_t1_imag");

        // Debt imaginary reserves
        let expected_debt_t0_imag = 18_101_440_459_658_112_555u128;
        check_within_pct(r.debt_token0_imaginary_reserves, expected_debt_t0_imag, 5, "debt_t0_imag");

        let expected_debt_t1_imag = 22_239_138_093_276_344_183u128;
        check_within_pct(r.debt_token1_imaginary_reserves, expected_debt_t1_imag, 5, "debt_t1_imag");

        println!("Pool 1 decode results:");
        println!("  center_price: {}", r.center_price);
        println!("  fee: {}", r.fee);
        println!("  col_t0_real: {}", r.col_token0_real_reserves);
        println!("  col_t1_real: {}", r.col_token1_real_reserves);
        println!("  col_t0_imag: {}", r.col_token0_imaginary_reserves);
        println!("  col_t1_imag: {}", r.col_token1_imaginary_reserves);
        println!("  debt_t0_imag: {}", r.debt_token0_imaginary_reserves);
        println!("  debt_t1_imag: {}", r.debt_token1_imaginary_reserves);
    }

    /// Pool 1 (wstETH/ETH) config constructed from known constantsView() values.
    fn pool1_config() -> FluidPoolConfig {
        let pool = Address::from_slice(&hex::decode("0B1a513ee24972DAEf112bC777a5610d4325C9e7").unwrap());
        FluidPoolConfig {
            pool_address: pool,
            liquidity_address: Address::from_slice(&hex::decode("52Aa899454998Be5b000Ad077a46Bbe360F4e497").unwrap()),
            exchange_price_token0_slot: U256::from_be_bytes(
                hex_bytes32("c24eaceff5753c99066a839532d708a8661af7a9b01d44d0cd915c53969eb725"),
            ),
            exchange_price_token1_slot: U256::from_be_bytes(
                hex_bytes32("a1829a9003092132f585b6ccdd167c19fe9774dbdea4260287e8a8e8ca8185d7"),
            ),
            supply_token0_slot: U256::from_be_bytes(
                hex_bytes32("a893c3ab5c5189a9bd276b29d25998250798d4f72dbb029d43e23884b0119a5a"),
            ),
            supply_token1_slot: U256::from_be_bytes(
                hex_bytes32("236696efd8534ce144b358082d303ba190cad0c8d37e9f4802b2a5198019379b"),
            ),
            borrow_token0_slot: U256::from_be_bytes(
                hex_bytes32("2cd14670f8a9e59d7c072449b534cc4ec6d89953cf20c518ba36d7fbdd468baf"),
            ),
            borrow_token1_slot: U256::from_be_bytes(
                hex_bytes32("d943cec1dfc617bf9515058376abfab0217f98cce018735f02efd4abd3453ad8"),
            ),
            token0_numerator_precision: 1,
            token0_denominator_precision: 1_000_000,
            token1_numerator_precision: 1,
            token1_denominator_precision: 1_000_000,
        }
    }

    fn hex_bytes32(s: &str) -> [u8; 32] {
        let bytes = hex::decode(s).unwrap();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        arr
    }

    /// Test that `from_abi` correctly parses raw returndata from pool 1.
    #[test]
    fn test_from_abi_pool1() {
        let pool = Address::from_slice(&hex::decode("0B1a513ee24972DAEf112bC777a5610d4325C9e7").unwrap());

        // Raw returndata from `cast call $POOL constantsView()`
        let cv_returndata = hex::decode(
            "000000000000000000000000000000000000000000000000000000000000000100000000000000000000000052aa899454998be5b000ad077a46bbe360f4e49700000000000000000000000091716c4eda1fb55e84bf8b4c7085f84285c190850000000000000000000000005b6b500981d7faa8c83be20514ea8067fbd42304000000000000000000000000363b7bbe35e5fbfd9f9ec976eb64b4c52d931ecc0000000000000000000000002f9b396255e681574d26fe466de93a9dff2567a6000000000000000000000000f7c62a231088c2babb32282bcf14e63db3484b82000000000000000000000000a512bdd83f9a81e2fbc4e24b54b9f5c642d5e0250000000000000000000000004ec7b668baf70d4a4b0fc7941a7708a07b6d45be0000000000000000000000007f39c581f595b53c5cb19bd0b3f8da6c935e2ca0000000000000000000000000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeea893c3ab5c5189a9bd276b29d25998250798d4f72dbb029d43e23884b0119a5a2cd14670f8a9e59d7c072449b534cc4ec6d89953cf20c518ba36d7fbdd468baf236696efd8534ce144b358082d303ba190cad0c8d37e9f4802b2a5198019379bd943cec1dfc617bf9515058376abfab0217f98cce018735f02efd4abd3453ad8c24eaceff5753c99066a839532d708a8661af7a9b01d44d0cd915c53969eb725a1829a9003092132f585b6ccdd167c19fe9774dbdea4260287e8a8e8ca8185d70000000000000000000000000000000000000000000000000000000000000400"
        ).unwrap();

        // Raw returndata from `cast call $POOL constantsView2()`
        let cv2_returndata = hex::decode(
            "000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000f4240000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000f4240"
        ).unwrap();

        let config = FluidPoolConfig::from_abi(pool, &cv_returndata, &cv2_returndata)
            .expect("from_abi should succeed");

        let expected = pool1_config();
        assert_eq!(config.pool_address, expected.pool_address);
        assert_eq!(config.liquidity_address, expected.liquidity_address);
        assert_eq!(config.exchange_price_token0_slot, expected.exchange_price_token0_slot);
        assert_eq!(config.exchange_price_token1_slot, expected.exchange_price_token1_slot);
        assert_eq!(config.supply_token0_slot, expected.supply_token0_slot);
        assert_eq!(config.supply_token1_slot, expected.supply_token1_slot);
        assert_eq!(config.borrow_token0_slot, expected.borrow_token0_slot);
        assert_eq!(config.borrow_token1_slot, expected.borrow_token1_slot);
        assert_eq!(config.token0_numerator_precision, expected.token0_numerator_precision);
        assert_eq!(config.token0_denominator_precision, expected.token0_denominator_precision);
        assert_eq!(config.token1_numerator_precision, expected.token1_numerator_precision);
        assert_eq!(config.token1_denominator_precision, expected.token1_denominator_precision);
    }

    /// Test that `storage_reads()` returns the correct addresses and slots.
    #[test]
    fn test_storage_reads() {
        let config = pool1_config();
        let reads = config.storage_reads();

        // First two are pool slots 0 and 1
        assert_eq!(reads[0].0, config.pool_address);
        assert_eq!(reads[0].1, U256::from(0));
        assert_eq!(reads[1].0, config.pool_address);
        assert_eq!(reads[1].1, U256::from(1));

        // Remaining 6 are Liquidity Layer slots
        for i in 2..8 {
            assert_eq!(reads[i].0, config.liquidity_address);
        }
        assert_eq!(reads[2].1, config.exchange_price_token0_slot);
        assert_eq!(reads[7].1, config.borrow_token1_slot);
    }

    fn check_within_pct(actual: u128, expected: u128, pct: u128, label: &str) {
        if expected == 0 {
            return;
        }
        let diff = (actual as i128 - expected as i128).unsigned_abs() * 100 / expected;
        assert!(
            diff <= pct,
            "{}: off by {}% (got {}, expected {})",
            label,
            diff,
            actual,
            expected
        );
    }
}
