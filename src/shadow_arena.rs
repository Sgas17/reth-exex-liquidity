//! ExEx-side shadow pool-arena writer (ITE-16, step 3).
//!
//! `ShadowArena` is the in-process writer that will make ExEx the sole writer of
//! the pool arena, replacing the `ExEx -> socket -> arena_service` replication.
//! The mmap layout lives in the shared [`arena_layout`] crate and the writer
//! (slot allocation + typed write API + mmap open) in [`arena_writer`]; both are
//! also used by `arena_service`, so the two writers are the same code driven by
//! different inputs.
//!
//! It opens a *shadow* arena on a separate mmap path (the `SHADOW_ARENA_PATH`
//! env flag) so it can run alongside the live socket path and be diffed against
//! arena_service's arena before cutover. When the flag is unset, the shadow
//! writer is disabled and the ExEx behaves exactly as before.
//!
//! Sub-step 3a added block-boundary plumbing; 3b hydrates startup topology from
//! a rich whitelist plus anchor-pinned storage reads. Live per-block apply lands
//! in 3c; reorg writes in 3d.

use crate::types::{PoolUpdateMessage, ReorgEpilogueUpdate};
use arena_layout::{
    AnyEkuboPool, AnyUniswapV3Pool, AnyUniswapV4Pool, CurveStablePoolData, CurveTricryptoPoolData,
    CurveTwoCryptoPoolData, SIGNAL_REASON_LIVE_BLOCK_APPLY, SIGNAL_REASON_LIVE_BLOCK_EMPTY,
};
use arena_writer::{ArenaMmap, SharedArenaWriter};
use std::path::{Path, PathBuf};

/// Env var naming the shadow arena mmap path. When unset, the shadow writer is
/// disabled.
pub const SHADOW_ARENA_PATH_ENV: &str = "SHADOW_ARENA_PATH";

/// Scraped V2 pool state for shadow-arena hydration. Token addresses + decimals
/// come from the rich whitelist; reserves are scraped from chain state at the
/// frozen anchor block.
pub struct V2Hydration {
    pub address: [u8; 20],
    pub token0: [u8; 20],
    pub token1: [u8; 20],
    pub reserve0: u128,
    pub reserve1: u128,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
}

pub struct UniswapV3Hydration {
    pub address: [u8; 20],
    pub pool: AnyUniswapV3Pool,
}

pub struct UniswapV4Hydration {
    pub pool_id: [u8; 32],
    pub pool: AnyUniswapV4Pool,
}

pub struct EkuboHydration {
    pub pool_id: [u8; 32],
    pub pool: AnyEkuboPool,
}

pub struct CurveStableHydration {
    pub address: [u8; 20],
    pub pool: CurveStablePoolData,
}

pub struct CurveTwoCryptoHydration {
    pub address: [u8; 20],
    pub pool: CurveTwoCryptoPoolData,
}

pub struct CurveTricryptoHydration {
    pub address: [u8; 20],
    pub pool: CurveTricryptoPoolData,
}

pub struct FluidHydration {
    pub address: [u8; 20],
    pub token0: [u8; 20],
    pub token1: [u8; 20],
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub col_token0_real: u128,
    pub col_token1_real: u128,
    pub col_token0_imaginary: u128,
    pub col_token1_imaginary: u128,
    pub debt_token0_real: u128,
    pub debt_token1_real: u128,
    pub debt_token0_imaginary: u128,
    pub debt_token1_imaginary: u128,
    pub center_price: u128,
    pub fee: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StartupHydrationCounts {
    pub v2: usize,
    pub v3: usize,
    pub v4: usize,
    pub ekubo: usize,
    pub curve_stable: usize,
    pub curve_twocrypto: usize,
    pub curve_tricrypto: usize,
    pub fluid: usize,
}

impl StartupHydrationCounts {
    pub fn total(self) -> usize {
        self.v2
            + self.v3
            + self.v4
            + self.ekubo
            + self.curve_stable
            + self.curve_twocrypto
            + self.curve_tricrypto
            + self.fluid
    }
}

/// In-process writer for the (shadow) pool arena.
pub struct ShadowArena {
    arena: ArenaMmap,
    /// Frozen anchor block all slots were hydrated at. Live apply (3c) skips
    /// updates with `block <= scraped_at_block` (the replay guard).
    scraped_at_block: u64,
    /// Pool updates applied since the last `end_block`, used to signal
    /// LIVE_BLOCK_APPLY (with the count) vs LIVE_BLOCK_EMPTY, matching
    /// arena_service so block signals stay diff-comparable.
    applied_this_block: u64,
}

impl ShadowArena {
    /// Open the shadow arena iff `SHADOW_ARENA_PATH` is set; `Ok(None)`
    /// otherwise (the ExEx then runs unchanged).
    pub fn from_env() -> eyre::Result<Option<Self>> {
        match std::env::var_os(SHADOW_ARENA_PATH_ENV) {
            Some(path) => Ok(Some(Self::open(&PathBuf::from(path))?)),
            None => Ok(None),
        }
    }

    /// Open (creating if needed) the shadow arena at `path` and reset it to a
    /// fresh state — matching arena_service, which resets header + slot
    /// assignments on start so tracker state and topology begin in sync.
    pub fn open(path: &Path) -> eyre::Result<Self> {
        let mut arena = ArenaMmap::open(path)
            .map_err(|e| eyre::eyre!("open shadow arena at {}: {e}", path.display()))?;
        arena.init();
        tracing::info!(
            path = %path.display(),
            "Shadow arena opened (ITE-16: block-signal plumbing + startup hydration)"
        );
        Ok(Self {
            arena,
            scraped_at_block: 0,
            applied_this_block: 0,
        })
    }

    /// Hydrate startup pool slots from scraped state + whitelist metadata,
    /// frozen at `anchor_block`. Creates slots and bumps `slot_version` once so
    /// readers rebuild their lookup from one coherent topology snapshot.
    #[allow(clippy::too_many_arguments)]
    pub fn hydrate_startup(
        &mut self,
        anchor_block: u64,
        v2: &[V2Hydration],
        v3: &[UniswapV3Hydration],
        v4: &[UniswapV4Hydration],
        ekubo: &[EkuboHydration],
        curve_stable: &[CurveStableHydration],
        curve_twocrypto: &[CurveTwoCryptoHydration],
        curve_tricrypto: &[CurveTricryptoHydration],
        fluid: &[FluidHydration],
    ) -> StartupHydrationCounts {
        self.scraped_at_block = anchor_block;
        let mut writer = SharedArenaWriter::new(self.arena.region_mut());
        let mut counts = StartupHydrationCounts::default();

        for p in v2 {
            match writer.add_v2_pool(
                p.address,
                p.reserve0,
                p.reserve1,
                p.token0,
                p.token1,
                p.token0_decimals,
                p.token1_decimals,
            ) {
                Ok(()) => counts.v2 += 1,
                Err(e) => tracing::warn!(address = ?p.address, "shadow V2 hydration failed: {e}"),
            }
        }

        for p in v3 {
            match writer.add_v3_pool(p.pool.clone()) {
                Ok(()) => counts.v3 += 1,
                Err(e) => tracing::warn!(address = ?p.address, "shadow V3 hydration failed: {e}"),
            }
        }

        for p in v4 {
            match writer.add_v4_pool(p.pool.clone()) {
                Ok(()) => counts.v4 += 1,
                Err(e) => tracing::warn!(pool_id = ?p.pool_id, "shadow V4 hydration failed: {e}"),
            }
        }

        for p in ekubo {
            match writer.add_ekubo_pool(p.pool.clone()) {
                Ok(()) => counts.ekubo += 1,
                Err(e) => {
                    tracing::warn!(pool_id = ?p.pool_id, "shadow Ekubo hydration failed: {e}")
                }
            }
        }

        for p in curve_stable {
            match writer.add_curve_stable_pool(p.address, &p.pool) {
                Ok(()) => counts.curve_stable += 1,
                Err(e) => {
                    tracing::warn!(address = ?p.address, "shadow CurveStable hydration failed: {e}")
                }
            }
        }

        for p in curve_twocrypto {
            match writer.add_curve_twocrypto_pool(p.address, &p.pool) {
                Ok(()) => counts.curve_twocrypto += 1,
                Err(e) => {
                    tracing::warn!(address = ?p.address, "shadow CurveTwoCrypto hydration failed: {e}")
                }
            }
        }

        for p in curve_tricrypto {
            match writer.add_curve_tricrypto_pool(p.address, &p.pool) {
                Ok(()) => counts.curve_tricrypto += 1,
                Err(e) => {
                    tracing::warn!(address = ?p.address, "shadow CurveTricrypto hydration failed: {e}")
                }
            }
        }

        for p in fluid {
            match writer.add_fluid_pool(
                p.address,
                p.token0,
                p.token1,
                p.fee,
                p.token0_decimals,
                p.token1_decimals,
            ) {
                Ok(()) => {
                    if let Err(e) = writer.update_fluid_reserves(
                        p.address,
                        p.col_token0_real,
                        p.col_token1_real,
                        p.col_token0_imaginary,
                        p.col_token1_imaginary,
                        p.debt_token0_real,
                        p.debt_token1_real,
                        p.debt_token0_imaginary,
                        p.debt_token1_imaginary,
                        p.center_price,
                        u128::from(p.fee),
                    ) {
                        tracing::warn!(address = ?p.address, "shadow Fluid reserve hydration failed: {e}");
                    } else {
                        counts.fluid += 1;
                    }
                }
                Err(e) => {
                    tracing::warn!(address = ?p.address, "shadow Fluid hydration failed: {e}")
                }
            }
        }

        writer.signal_topology_change();
        tracing::info!(
            ?counts,
            total = counts.total(),
            anchor_block,
            "Shadow arena startup hydration complete"
        );
        counts
    }

    /// Hydrate only V2 pool slots. Kept as a focused unit-test/convenience
    /// wrapper around the multi-protocol startup path.
    #[allow(dead_code)]
    pub fn hydrate_v2(&mut self, anchor_block: u64, pools: &[V2Hydration]) -> usize {
        self.hydrate_startup(anchor_block, pools, &[], &[], &[], &[], &[], &[], &[])
            .v2
    }

    /// Apply one committed-block pool update (ITE-16 step 3c).
    ///
    /// Replay-guarded by the frozen hydration anchor: events at or below
    /// `scraped_at_block` are already reflected in the hydrated state, so they
    /// are skipped (`Ok(false)`). Because the ExEx hydrates every pool at one
    /// anchor, a single global guard suffices (unlike arena_service's per-pool
    /// guard). Applying delegates to [`shadow_apply::apply_live_event`], which
    /// mirrors arena_service's writer calls exactly.
    pub fn apply_live_event(
        &mut self,
        event: &PoolUpdateMessage,
    ) -> std::result::Result<bool, crate::shadow_apply::ApplyError> {
        if event.block_number <= self.scraped_at_block {
            return Ok(false);
        }
        let applied = {
            let mut writer = SharedArenaWriter::new(self.arena.region_mut());
            crate::shadow_apply::apply_live_event(&mut writer, event)?
        };
        if applied {
            self.applied_this_block += 1;
        }
        Ok(applied)
    }

    /// Apply a reorg-epilogue update (ITE-16 step 3d): the definitive post-reorg
    /// slot0/fluid state read from chain at the settled tip. Authoritative
    /// absolute write — not replay-guarded — and counted toward the next block
    /// signal so the epilogue resync is observable.
    pub fn apply_reorg_epilogue(
        &mut self,
        update: &ReorgEpilogueUpdate,
    ) -> std::result::Result<bool, crate::shadow_apply::ApplyError> {
        let applied = {
            let mut writer = SharedArenaWriter::new(self.arena.region_mut());
            crate::shadow_apply::apply_reorg_epilogue(&mut writer, update)?
        };
        if applied {
            self.applied_this_block += 1;
        }
        Ok(applied)
    }

    /// Block boundary end (3a plumbing, 3c apply count). Signals the header so a
    /// reader sees the shadow arena advance: LIVE_BLOCK_APPLY with the applied
    /// count for non-empty blocks, LIVE_BLOCK_EMPTY otherwise — matching
    /// arena_service so the block signal stays diff-comparable. Resets the
    /// per-block applied counter.
    pub fn end_block(&mut self, block_number: u64) {
        let applied = std::mem::take(&mut self.applied_this_block);
        let reason = if applied == 0 {
            SIGNAL_REASON_LIVE_BLOCK_EMPTY
        } else {
            SIGNAL_REASON_LIVE_BLOCK_APPLY
        };
        self.arena
            .region()
            .header
            .signal_update_complete(block_number, applied, reason, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        PoolIdentifier, PoolUpdate, Protocol, ReorgEpilogueUpdate, Slot0State, UpdateType,
    };
    use alloy_primitives::{Address, I256, U256};
    use arena_layout::ekubo::EkuboLowPoolData;
    use arena_layout::{
        AnyEkuboPool, AnyUniswapV3Pool, AnyUniswapV4Pool, SharedArenaRegion, UniswapV3LowPoolData,
        UniswapV4LowPoolData, SHARED_ARENA_VERSION,
    };
    use arena_writer::SharedArenaWriter;
    use std::sync::atomic::Ordering;

    fn temp_arena_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ite16_{tag}_{}.arena", std::process::id()))
    }

    fn addr(byte: u8) -> [u8; 20] {
        [byte; 20]
    }

    /// Proves `arena_layout` compiles into the ExEx (reth) build and that this
    /// crate's `alloy_primitives::U256` unifies with `arena_layout`'s `U256`
    /// `#[repr(C)]` fields — i.e. both repos resolve to one alloy-primitives.
    #[test]
    fn arena_layout_types_are_usable_from_exex() {
        let mut pool = UniswapV3LowPoolData::default();
        pool.sqrt_price_x96 = U256::from(123_456_u64);
        assert_eq!(pool.sqrt_price_x96, U256::from(123_456_u64));

        assert_eq!(SHARED_ARENA_VERSION, 5);
        assert!(SharedArenaRegion::size() > 0);
    }

    /// Proves the ExEx can open the arena mmap and write pool state through the
    /// shared `arena_writer::SharedArenaWriter`.
    #[test]
    fn exex_writes_arena_via_shared_writer() {
        let path = temp_arena_path("shadow_write");
        let mut arena = ArenaMmap::open(&path).expect("open shadow arena");
        arena.init();
        arena.validate().expect("fresh region validates");

        let addr = [0xAB_u8; 20];
        {
            let mut writer = SharedArenaWriter::new(arena.region_mut());
            writer
                .add_v2_pool(addr, 1_000, 2_000, [0x11; 20], [0x22; 20], 18, 6)
                .expect("add v2 pool");

            let got = writer.get_v2_pool(&addr).expect("read pool back");
            assert_eq!(got.reserve0, 1_000);
            assert_eq!(got.reserve1, 2_000);
            assert_eq!(got.token0_decimals, 18);
        }

        let _ = std::fs::remove_file(&path);
    }

    /// 3a plumbing: end_block signals the header (block number + update sequence
    /// bump) so a reader sees the shadow arena advance per block.
    #[test]
    fn shadow_end_block_signals_header() {
        let path = temp_arena_path("shadow_signal");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        let before = shadow.arena.region().header.get_sequence();
        shadow.end_block(100);

        assert_eq!(
            shadow.arena.region().header.get_sequence(),
            before + 1,
            "end_block must bump the update sequence"
        );
        assert_eq!(shadow.arena.region().header.get_block_number(), 100);

        let _ = std::fs::remove_file(&path);
    }

    /// 3b-1: hydrate_v2 creates a slot per pool (readable back) and records the
    /// frozen anchor block.
    #[test]
    fn hydrate_v2_creates_readable_slots() {
        let path = temp_arena_path("shadow_hydrate");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        let pools = vec![V2Hydration {
            address: [0xAB; 20],
            token0: [0x11; 20],
            token1: [0x22; 20],
            reserve0: 1_000,
            reserve1: 2_000,
            token0_decimals: 6,
            token1_decimals: 18,
        }];
        let created = shadow.hydrate_v2(12_345, &pools);
        assert_eq!(created, 1);
        assert_eq!(shadow.scraped_at_block, 12_345);

        // Re-open a writer to read the slot back (rebuilds lookup from assignments).
        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got = writer.get_v2_pool(&[0xAB; 20]).expect("slot exists");
        assert_eq!(got.reserve0, 1_000);
        assert_eq!(got.reserve1, 2_000);
        assert_eq!(got.token0_decimals, 6);
        assert_eq!(got.token1_decimals, 18);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hydrate_startup_creates_curve_and_fluid_slots() {
        let path = temp_arena_path("shadow_hydrate_multi");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        let mut stable = CurveStablePoolData::default();
        stable.n_coins = 2;
        stable.balances[0] = 10;
        stable.balances[1] = 20;
        stable.fee = 4_000_000;
        stable.coins[0] = addr(0x11);
        stable.coins[1] = addr(0x22);
        stable.rate_multipliers[0] = 1_000_000_000_000_000_000;
        stable.rate_multipliers[1] = 1_000_000_000_000_000_000;

        let mut twocrypto = CurveTwoCryptoPoolData::default();
        twocrypto.balances = [30, 40];
        twocrypto.price_scale = 1_000_000_000_000_000_000;
        twocrypto.d = 70;
        twocrypto.coins = [addr(0x33), addr(0x44)];

        let fluid = FluidHydration {
            address: addr(0xCC),
            token0: addr(0x55),
            token1: addr(0x66),
            token0_decimals: 6,
            token1_decimals: 18,
            col_token0_real: 1,
            col_token1_real: 2,
            col_token0_imaginary: 3,
            col_token1_imaginary: 4,
            debt_token0_real: 5,
            debt_token1_real: 6,
            debt_token0_imaginary: 7,
            debt_token1_imaginary: 8,
            center_price: 9,
            fee: 500,
        };

        let counts = shadow.hydrate_startup(
            12_345,
            &[],
            &[],
            &[],
            &[],
            &[CurveStableHydration {
                address: addr(0xAA),
                pool: stable,
            }],
            &[CurveTwoCryptoHydration {
                address: addr(0xBB),
                pool: twocrypto,
            }],
            &[],
            &[fluid],
        );
        assert_eq!(counts.curve_stable, 1);
        assert_eq!(counts.curve_twocrypto, 1);
        assert_eq!(counts.fluid, 1);
        assert_eq!(counts.total(), 3);
        assert_eq!(shadow.scraped_at_block, 12_345);

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        assert_eq!(
            writer
                .get_curve_stable_pool(&addr(0xAA))
                .expect("stable slot")
                .balances[1],
            20
        );
        assert_eq!(
            writer
                .get_curve_twocrypto_pool(&addr(0xBB))
                .expect("twocrypto slot")
                .d,
            70
        );
        let fluid = writer.get_fluid_pool(&addr(0xCC)).expect("fluid slot");
        assert_eq!(fluid.col_token0_real, 1);
        assert_eq!(fluid.debt_token1_imaginary, 8);
        assert_eq!(fluid.token0_decimals, 6);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hydrate_startup_creates_v3_v4_ekubo_slots() {
        let path = temp_arena_path("shadow_hydrate_ticks");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        let mut v3 = UniswapV3LowPoolData::default();
        v3.common.pool_id = addr(0xA3);
        v3.common.is_active.store(true, Ordering::Release);
        v3.sqrt_price_x96 = U256::from(1_000u64);
        v3.tick = 10;
        v3.liquidity = 100_000;
        v3.fee = 500;
        v3.tick_spacing = 10;
        v3.token0_decimals = 6;
        v3.token1_decimals = 18;
        v3.tick_count = 1;
        v3.ticks[0] = (0, 100, 100);
        v3.bitmap_count = 1;
        v3.tick_bitmap[0] = (0, [1u8; 32]);

        let v4_id = [0xB4; 32];
        let mut v4 = UniswapV4LowPoolData::default();
        v4.pool_id = v4_id;
        v4.common.pool_id.copy_from_slice(&v4_id[..20]);
        v4.common.is_active.store(true, Ordering::Release);
        v4.sqrt_price_x96 = U256::from(2_000u64);
        v4.tick = 20;
        v4.liquidity = 200_000;
        v4.fee = 500;
        v4.tick_spacing = 10;
        v4.token0_decimals = 6;
        v4.token1_decimals = 18;

        let ekubo_id = [0xE0; 32];
        let mut ekubo = EkuboLowPoolData::default();
        ekubo.pool_id = ekubo_id;
        ekubo.common.pool_id.copy_from_slice(&ekubo_id[..20]);
        ekubo.common.is_active.store(true, Ordering::Release);
        ekubo.sqrt_price_x96 = U256::from(3_000u64);
        ekubo.tick = 30;
        ekubo.liquidity = 300_000;
        ekubo.fee = 42;
        ekubo.tick_spacing = 10;
        ekubo.type_config = 0x8000_000a;
        ekubo.token0_decimals = 6;
        ekubo.token1_decimals = 18;

        let counts = shadow.hydrate_startup(
            12_346,
            &[],
            &[UniswapV3Hydration {
                address: addr(0xA3),
                pool: AnyUniswapV3Pool::Low(v3),
            }],
            &[UniswapV4Hydration {
                pool_id: v4_id,
                pool: AnyUniswapV4Pool::Low(v4),
            }],
            &[EkuboHydration {
                pool_id: ekubo_id,
                pool: AnyEkuboPool::Low(ekubo),
            }],
            &[],
            &[],
            &[],
            &[],
        );

        assert_eq!(counts.v3, 1);
        assert_eq!(counts.v4, 1);
        assert_eq!(counts.ekubo, 1);
        assert_eq!(counts.total(), 3);

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got_v3 = writer.get_v3_pool(&addr(0xA3)).expect("v3 slot");
        assert_eq!(got_v3.sqrt_price_x96(), U256::from(1_000u64));
        assert_eq!(got_v3.tick(), 10);
        assert!(writer.get_v4_pool(&v4_id).is_some());
        assert!(writer.contains_pool_v4(&ekubo_id));

        let _ = std::fs::remove_file(&path);
    }

    fn v2_swap_event(pool: [u8; 20], block: u64, a0: i64, a1: i64) -> PoolUpdateMessage {
        PoolUpdateMessage {
            pool_id: PoolIdentifier::Address(Address::from(pool)),
            protocol: Protocol::UniswapV2,
            update_type: UpdateType::Swap,
            block_number: block,
            block_timestamp: 0,
            tx_index: 0,
            log_index: 0,
            is_revert: false,
            update: PoolUpdate::V2Swap {
                amount0: I256::try_from(a0).expect("a0"),
                amount1: I256::try_from(a1).expect("a1"),
            },
        }
    }

    /// 3c: a V2 swap above the anchor folds reserve deltas; the replay guard
    /// skips an event at the anchor block (already reflected in hydrated state).
    #[test]
    fn live_v2_swap_folds_reserve_deltas_after_anchor() {
        let path = temp_arena_path("live_v2_swap");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_v2(
            100,
            &[V2Hydration {
                address: addr(0xC2),
                token0: addr(0x11),
                token1: addr(0x22),
                reserve0: 1_000,
                reserve1: 2_000,
                token0_decimals: 18,
                token1_decimals: 6,
            }],
        );

        // Replay guard: event at the anchor block is skipped.
        let at_anchor = v2_swap_event(addr(0xC2), 100, 500, -300);
        assert!(!shadow
            .apply_live_event(&at_anchor)
            .expect("apply at anchor"));

        // Above the anchor: deltas fold into reserves (+500 / -300).
        let after = v2_swap_event(addr(0xC2), 101, 500, -300);
        assert!(shadow.apply_live_event(&after).expect("apply after anchor"));

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let pool = writer.get_v2_pool(&addr(0xC2)).expect("v2 pool");
        assert_eq!(pool.reserve0, 1_500);
        assert_eq!(pool.reserve1, 1_700);

        let _ = std::fs::remove_file(&path);
    }

    /// 3c: an event for a pool not in the shadow topology (e.g. live-added but
    /// not yet hydrated) is skipped, not an error.
    #[test]
    fn live_event_for_unhydrated_pool_is_skipped() {
        let path = temp_arena_path("live_v2_missing");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_v2(100, &[]);
        let ev = v2_swap_event(addr(0xDD), 101, 1, -1);
        assert!(!shadow.apply_live_event(&ev).expect("apply"));
        let _ = std::fs::remove_file(&path);
    }

    /// 3c: a V3 swap above the anchor overwrites slot0 with absolute post-state.
    #[test]
    fn live_v3_swap_overwrites_slot0_after_anchor() {
        let path = temp_arena_path("live_v3_swap");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        let mut v3 = UniswapV3LowPoolData::default();
        v3.common.pool_id = addr(0xA3);
        v3.common.is_active.store(true, Ordering::Release);
        v3.sqrt_price_x96 = U256::from(1_000u64);
        v3.tick = 10;
        v3.liquidity = 100_000;
        v3.fee = 500;
        v3.tick_spacing = 10;
        v3.token0_decimals = 6;
        v3.token1_decimals = 18;

        shadow.hydrate_startup(
            100,
            &[],
            &[UniswapV3Hydration {
                address: addr(0xA3),
                pool: AnyUniswapV3Pool::Low(v3),
            }],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        let ev = PoolUpdateMessage {
            pool_id: PoolIdentifier::Address(Address::from(addr(0xA3))),
            protocol: Protocol::UniswapV3,
            update_type: UpdateType::Swap,
            block_number: 101,
            block_timestamp: 0,
            tx_index: 0,
            log_index: 0,
            is_revert: false,
            update: PoolUpdate::V3Swap {
                sqrt_price_x96: U256::from(2_222u64),
                liquidity: 250_000,
                tick: 42,
            },
        };
        assert!(shadow.apply_live_event(&ev).expect("apply v3 swap"));

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got = writer.get_v3_pool(&addr(0xA3)).expect("v3 pool");
        assert_eq!(got.sqrt_price_x96(), U256::from(2_222u64));
        assert_eq!(got.tick(), 42);

        let _ = std::fs::remove_file(&path);
    }

    /// 3c (round-07/08 fix): an Ekubo PositionUpdated (`EkuboLiquidity`, emitted
    /// as Mint/Burn) must (a) overwrite slot0 with the authoritative post-state
    /// and (b) fold the tick-range liquidity delta into the arena tick array +
    /// bitmap (downstream Ekubo quote context reads both), and (c) signal a
    /// non-empty live block. Regression for round-07 dropping the tick fields.
    #[test]
    fn live_ekubo_position_update_overwrites_slot0() {
        let path = temp_arena_path("live_ekubo_pos");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        let ekubo_id = [0xE0; 32];
        let mut ekubo = EkuboLowPoolData::default();
        ekubo.pool_id = ekubo_id;
        ekubo.common.pool_id.copy_from_slice(&ekubo_id[..20]);
        ekubo.common.is_active.store(true, Ordering::Release);
        ekubo.sqrt_price_x96 = U256::from(3_000u64);
        ekubo.tick = 30;
        ekubo.liquidity = 300_000;
        ekubo.fee = 42;
        ekubo.tick_spacing = 10;
        ekubo.type_config = 0x8000_000a;
        ekubo.token0_decimals = 6;
        ekubo.token1_decimals = 18;

        shadow.hydrate_startup(
            100,
            &[],
            &[],
            &[],
            &[EkuboHydration {
                pool_id: ekubo_id,
                pool: AnyEkuboPool::Low(ekubo),
            }],
            &[],
            &[],
            &[],
            &[],
        );

        let ev = PoolUpdateMessage {
            pool_id: PoolIdentifier::PoolId(ekubo_id),
            protocol: Protocol::Ekubo,
            update_type: UpdateType::Mint,
            block_number: 101,
            block_timestamp: 0,
            tx_index: 0,
            log_index: 0,
            is_revert: false,
            update: PoolUpdate::EkuboLiquidity {
                tick_lower: -10,
                tick_upper: 10,
                liquidity_delta: 5_000,
                sqrt_ratio: U256::from(9_999u64),
                liquidity: 350_000,
                tick: 33,
            },
        };
        assert!(shadow
            .apply_live_event(&ev)
            .expect("apply ekubo position update"));
        // One applied event → non-empty block signal with count 1.
        shadow.end_block(101);
        assert_eq!(
            shadow.arena.region().header.get_signal_reason(),
            SIGNAL_REASON_LIVE_BLOCK_APPLY
        );
        assert_eq!(shadow.arena.region().header.get_pools_updated_count(), 1);

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got = writer.get_ekubo_pool(&ekubo_id).expect("ekubo pool");
        // (a) authoritative slot0 post-state.
        assert_eq!(got.sqrt_price_x96(), U256::from(9_999u64));
        assert_eq!(got.tick(), 33);
        assert_eq!(got.liquidity(), 350_000);

        // (b) tick array + bitmap folded the delta.
        let AnyEkuboPool::Low(p) = got else {
            panic!("expected Low-tier Ekubo pool");
        };
        let n = p.tick_count as usize;
        let lower = p.ticks[..n]
            .iter()
            .find(|(t, _, _)| *t == -10)
            .expect("lower tick present");
        assert_eq!(lower.1, 5_000, "lower gross");
        assert_eq!(lower.2, 5_000, "lower net (+delta)");
        let upper = p.ticks[..n]
            .iter()
            .find(|(t, _, _)| *t == 10)
            .expect("upper tick present");
        assert_eq!(upper.1, 5_000, "upper gross");
        assert_eq!(upper.2, -5_000, "upper net (-delta)");

        for tick in [-10i32, 10] {
            let (word, idx) = arena_layout::ekubo::ekubo_tick_to_word_and_index(tick, 10);
            let bm = p.tick_bitmap[..p.bitmap_count as usize]
                .iter()
                .find(|(w, _)| *w == word)
                .unwrap_or_else(|| panic!("bitmap word present for tick {tick}"));
            let val = U256::from_be_bytes(bm.1);
            assert_eq!(
                (val >> idx as usize) & U256::from(1),
                U256::from(1),
                "bitmap bit set for tick {tick}"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    /// 3c (round-07 fix): a non-empty live block signals LIVE_BLOCK_APPLY with the
    /// applied count, while an empty block signals LIVE_BLOCK_EMPTY — matching
    /// arena_service so the header signal stays diff-comparable.
    #[test]
    fn live_apply_signals_block_apply_with_count() {
        let path = temp_arena_path("live_signal");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_v2(
            100,
            &[V2Hydration {
                address: addr(0xC2),
                token0: addr(0x11),
                token1: addr(0x22),
                reserve0: 1_000,
                reserve1: 2_000,
                token0_decimals: 18,
                token1_decimals: 6,
            }],
        );

        // No applies this block → empty signal, count 0.
        shadow.end_block(101);
        {
            let h = &shadow.arena.region().header;
            assert_eq!(h.get_signal_reason(), SIGNAL_REASON_LIVE_BLOCK_EMPTY);
            assert_eq!(h.get_pools_updated_count(), 0);
        }

        // One applied update → apply signal, count 1, counter reset for next block.
        let ev = v2_swap_event(addr(0xC2), 102, 500, -300);
        assert!(shadow.apply_live_event(&ev).expect("apply"));
        shadow.end_block(102);
        {
            let h = &shadow.arena.region().header;
            assert_eq!(h.get_signal_reason(), SIGNAL_REASON_LIVE_BLOCK_APPLY);
            assert_eq!(h.get_pools_updated_count(), 1);
            assert_eq!(h.get_block_number(), 102);
        }

        // Next block with no applies → back to empty, count 0 (counter was reset).
        shadow.end_block(103);
        {
            let h = &shadow.arena.region().header;
            assert_eq!(h.get_signal_reason(), SIGNAL_REASON_LIVE_BLOCK_EMPTY);
            assert_eq!(h.get_pools_updated_count(), 0);
        }

        let _ = std::fs::remove_file(&path);
    }

    /// 3d: a reverted V2 swap (`is_revert = true`) un-applies the delta, returning
    /// reserves to their pre-swap value.
    #[test]
    fn live_v2_swap_revert_unapplies_delta() {
        let path = temp_arena_path("revert_v2");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");
        shadow.hydrate_v2(
            100,
            &[V2Hydration {
                address: addr(0xC2),
                token0: addr(0x11),
                token1: addr(0x22),
                reserve0: 1_000,
                reserve1: 2_000,
                token0_decimals: 18,
                token1_decimals: 6,
            }],
        );

        // Forward swap at block 101: +500 / -300 → 1500 / 1700.
        assert!(shadow
            .apply_live_event(&v2_swap_event(addr(0xC2), 101, 500, -300))
            .expect("forward"));

        // Reorg reverts block 101 at block 102: applies the inverse → 1000 / 2000.
        let mut revert = v2_swap_event(addr(0xC2), 102, 500, -300);
        revert.is_revert = true;
        assert!(shadow.apply_live_event(&revert).expect("revert"));

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let pool = writer.get_v2_pool(&addr(0xC2)).expect("v2 pool");
        assert_eq!(pool.reserve0, 1_000);
        assert_eq!(pool.reserve1, 2_000);

        let _ = std::fs::remove_file(&path);
    }

    /// 3d: a reorg-epilogue `Slot0Final` overwrites a V3 pool's slot0 with the
    /// definitive post-reorg state (the mechanism that refreshes pools swapped in
    /// the reverted chain but not the new one).
    #[test]
    fn reorg_epilogue_slot0_final_overwrites_v3() {
        let path = temp_arena_path("epilogue_v3");
        let mut shadow = ShadowArena::open(&path).expect("open shadow arena");

        let mut v3 = UniswapV3LowPoolData::default();
        v3.common.pool_id = addr(0xA3);
        v3.common.is_active.store(true, Ordering::Release);
        v3.sqrt_price_x96 = U256::from(1_000u64);
        v3.tick = 10;
        v3.liquidity = 100_000;
        v3.fee = 500;
        v3.tick_spacing = 10;
        v3.token0_decimals = 6;
        v3.token1_decimals = 18;

        shadow.hydrate_startup(
            100,
            &[],
            &[UniswapV3Hydration {
                address: addr(0xA3),
                pool: AnyUniswapV3Pool::Low(v3),
            }],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        let epilogue = ReorgEpilogueUpdate::Slot0Final {
            pool_id: PoolIdentifier::Address(Address::from(addr(0xA3))),
            protocol: Protocol::UniswapV3,
            state: Slot0State {
                sqrt_price_x96: U256::from(7_777u64),
                liquidity: 123_456,
                tick: -5,
            },
        };
        assert!(shadow.apply_reorg_epilogue(&epilogue).expect("epilogue"));

        // Epilogue write counts toward the block signal.
        shadow.end_block(120);
        assert_eq!(
            shadow.arena.region().header.get_signal_reason(),
            SIGNAL_REASON_LIVE_BLOCK_APPLY
        );

        let writer = SharedArenaWriter::new(shadow.arena.region_mut());
        let got = writer.get_v3_pool(&addr(0xA3)).expect("v3 pool");
        assert_eq!(got.sqrt_price_x96(), U256::from(7_777u64));
        assert_eq!(got.tick(), -5);

        let _ = std::fs::remove_file(&path);
    }
}
